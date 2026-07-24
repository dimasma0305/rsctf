//! Git transport policy, checkout locking, and subprocess-backed synchronization.

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsStr;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use tokio::time::timeout;

use super::{
    MAX_REPO_DEPTH, MAX_REPO_ENTRIES, MAX_REPO_FILES, MAX_REPO_FILE_BYTES, MAX_REPO_TOTAL_BYTES,
};
use crate::utils::error::{AppError, AppResult};

/// Hard wall-clock cap on a single `git` invocation. Two minutes is generous for
/// a shallow clone of any real-world CTF repo; past that we assume the connection
/// is wedged (DNS, TLS handshake, proxy, revoked token mid-fetch) and kill it so
/// the poll tick stays alive for subsequent bindings.
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const GIT_DNS_TIMEOUT: Duration = Duration::from_secs(5);

/// GitHub's smart-HTTP transport authenticates with HTTP Basic using this fixed
/// username and the PAT as the password. Works identically for classic `ghp_` and
/// fine-grained `github_pat_` tokens.
const GIT_AUTH_USER: &str = "x-access-token";

#[derive(Clone, Copy)]
enum RepoUrlPolicy {
    GithubHttps,
    Web,
    SyncTransport,
}

pub fn validate_github_repo_url(raw: &str) -> AppResult<String> {
    validate_repo_url(raw, RepoUrlPolicy::GithubHttps)
}

pub fn validate_binding_repo_url(raw: &str) -> AppResult<String> {
    validate_repo_url(raw, RepoUrlPolicy::Web)
}

pub(super) fn validate_sync_repo_url(raw: &str) -> AppResult<String> {
    validate_repo_url(raw, RepoUrlPolicy::SyncTransport)
}

fn validate_repo_url(raw: &str, policy: RepoUrlPolicy) -> AppResult<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.starts_with('-') {
        return Err(AppError::bad_request("invalid repository URL"));
    }
    let parsed = reqwest::Url::parse(raw)
        .map_err(|_| AppError::bad_request("repository URL must be absolute http(s)"))?;
    let scheme_ok = match policy {
        RepoUrlPolicy::GithubHttps => parsed.scheme() == "https",
        RepoUrlPolicy::Web | RepoUrlPolicy::SyncTransport => {
            matches!(parsed.scheme(), "http" | "https")
        }
    };
    if !scheme_ok || parsed.cannot_be_a_base() || parsed.fragment().is_some() {
        return Err(AppError::bad_request(
            "repository URL must be absolute http(s)",
        ));
    }
    let host = parsed
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| AppError::bad_request("repository URL requires a host"))?;
    if is_local_git_host(host) {
        return Err(AppError::bad_request(
            "local repository hosts are not allowed",
        ));
    }

    let has_userinfo = !parsed.username().is_empty() || parsed.password().is_some();
    let internal_auth = parsed.username() == GIT_AUTH_USER && parsed.password().is_some();
    if has_userinfo && !matches!(policy, RepoUrlPolicy::SyncTransport) {
        return Err(AppError::bad_request(
            "repository URL must not contain userinfo",
        ));
    }
    if has_userinfo && !internal_auth {
        return Err(AppError::bad_request(
            "repository URL contains invalid credentials",
        ));
    }
    if internal_auth && parsed.scheme() != "https" {
        return Err(AppError::bad_request(
            "repository credentials require HTTPS",
        ));
    }

    if matches!(policy, RepoUrlPolicy::GithubHttps)
        && (!host.eq_ignore_ascii_case("github.com")
            || parsed.port_or_known_default() != Some(443)
            || parsed
                .path_segments()
                .is_none_or(|segments| segments.filter(|s| !s.is_empty()).count() < 2))
    {
        return Err(AppError::bad_request(
            "repoUrl must be an HTTPS github.com repository URL",
        ));
    }
    Ok(raw.to_string())
}

fn is_local_git_host(host: &str) -> bool {
    let host = host.trim_end_matches('.');
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") || lower.ends_with(".local") {
        return true;
    }
    host.parse::<IpAddr>().is_ok_and(|ip| !is_public_git_ip(ip))
}

/// Permit only globally routable unicast destinations. Keep this explicit
/// rather than relying on platform routing: repository URLs are persisted and
/// re-resolved by scheduled scans, so a hostname that later points at loopback,
/// link-local metadata, a private subnet, or a transition prefix must fail
/// before Git opens a socket.
fn is_public_git_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 168)
                || (a == 192 && b == 0 && c == 0)
                || (a == 192 && b == 0 && c == 2)
                || (a == 198 && (b == 18 || b == 19))
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            // Global IPv6 unicast is 2000::/3. Exclude special-purpose ranges
            // inside that block, including 6to4 and documentation prefixes.
            segments[0] & 0xe000 == 0x2000
                && !(segments[0] == 0x2001 && segments[1] < 0x0200)
                && !(segments[0] == 0x2001 && segments[1] == 0x0db8)
                && segments[0] != 0x2002
                && !(segments[0] == 0x3fff && segments[1] < 0x1000)
        }
    }
}

#[derive(Debug)]
struct GitTransportPolicy {
    curl_resolve: Option<String>,
}

impl GitTransportPolicy {
    fn command_args(&self, args: &[&str]) -> Vec<String> {
        let mut configured = vec![
            "-c".to_string(),
            "http.followRedirects=false".to_string(),
            // Clear inherited CURLOPT_RESOLVE entries before adding the exact
            // host mapping approved for this invocation.
            "-c".to_string(),
            "http.curloptResolve=".to_string(),
        ];
        if let Some(resolve) = &self.curl_resolve {
            configured.push("-c".to_string());
            configured.push(format!("http.curloptResolve={resolve}"));
        }
        configured.extend(args.iter().map(|arg| (*arg).to_string()));
        configured
    }
}

fn pinned_transport_policy(
    host: &str,
    port: u16,
    addresses: impl IntoIterator<Item = IpAddr>,
) -> AppResult<GitTransportPolicy> {
    let addresses = addresses.into_iter().collect::<BTreeSet<_>>();
    if addresses.is_empty() {
        return Err(AppError::bad_request(
            "repository host did not resolve to an address",
        ));
    }
    if addresses.iter().any(|address| !is_public_git_ip(*address)) {
        return Err(AppError::bad_request(
            "repository host must resolve only to public addresses",
        ));
    }

    let curl_resolve = host.parse::<IpAddr>().is_err().then(|| {
        let addresses = addresses
            .iter()
            .map(|address| match address {
                IpAddr::V4(address) => address.to_string(),
                IpAddr::V6(address) => format!("[{address}]"),
            })
            .collect::<Vec<_>>()
            .join(",");
        format!("{host}:{port}:{addresses}")
    });
    Ok(GitTransportPolicy { curl_resolve })
}

async fn resolve_git_transport(url: &str) -> AppResult<GitTransportPolicy> {
    let parsed =
        reqwest::Url::parse(url).map_err(|_| AppError::bad_request("invalid repository URL"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| AppError::bad_request("repository URL requires a host"))?;
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| AppError::bad_request("repository URL requires a known port"))?;

    if let Ok(address) = host.parse::<IpAddr>() {
        return pinned_transport_policy(host, port, [address]);
    }

    let resolved = timeout(GIT_DNS_TIMEOUT, tokio::net::lookup_host((host, port)))
        .await
        .map_err(|_| AppError::bad_request("repository host lookup timed out"))?
        .map_err(|_| AppError::bad_request("repository host could not be resolved"))?;
    pinned_transport_policy(host, port, resolved.map(|address| address.ip()))
}

pub fn validate_git_ref(raw: Option<&str>) -> AppResult<Option<String>> {
    let Some(value) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let invalid = value.len() > 255
        || value.starts_with('-')
        || value.starts_with('/')
        || value.ends_with('/')
        || value.ends_with('.')
        || value.ends_with(".lock")
        || value.contains("..")
        || value.contains("//")
        || value.contains("@{")
        || value.chars().any(|c| {
            c.is_control()
                || c.is_whitespace()
                || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        });
    if invalid {
        return Err(AppError::bad_request("invalid git ref"));
    }
    Ok(Some(value.to_string()))
}

pub(super) fn url_without_credentials(raw: &str) -> AppResult<String> {
    let mut parsed =
        reqwest::Url::parse(raw).map_err(|_| AppError::bad_request("invalid repository URL"))?;
    parsed
        .set_password(None)
        .map_err(|_| AppError::bad_request("invalid repository URL"))?;
    parsed
        .set_username("")
        .map_err(|_| AppError::bad_request("invalid repository URL"))?;
    Ok(parsed.to_string())
}

type CheckoutMutex = AsyncMutex<()>;

static CHECKOUT_LOCKS: LazyLock<Mutex<HashMap<PathBuf, Weak<CheckoutMutex>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Owned guard for one persistent repository checkout. Callers hold it across
/// sync and every path-based read/write that follows, preventing a concurrent
/// scan or push-back from replacing files between containment checks and reads.
pub struct CheckoutLockGuard {
    _guard: OwnedMutexGuard<()>,
    _distributed: Option<crate::utils::single_flight::PgAdvisoryLock>,
    _checkout_permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

/// Serialize the complete lifecycle of a persistent checkout. The key resolves
/// the nearest existing ancestor before appending any missing suffix, so callers
/// agree even when the configured storage root is itself a symlink and the
/// checkout has not been cloned yet.
pub async fn lock_checkout(path: &Path) -> CheckoutLockGuard {
    let key = checkout_lock_key(path);
    let checkout_lock = {
        let mut locks = CHECKOUT_LOCKS.lock().unwrap_or_else(|e| e.into_inner());
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
            lock
        } else {
            let lock = Arc::new(CheckoutMutex::new(()));
            locks.insert(key, Arc::downgrade(&lock));
            lock
        }
    };
    CheckoutLockGuard {
        _guard: checkout_lock.lock_owned().await,
        _distributed: None,
        _checkout_permit: None,
    }
}

fn checkout_gate() -> &'static Arc<tokio::sync::Semaphore> {
    static GATE: LazyLock<Arc<tokio::sync::Semaphore>> = LazyLock::new(|| {
        let permits = std::env::var("RSCTF_REPO_SCAN_CONCURRENCY")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| (1..=4).contains(value))
            .unwrap_or(1);
        Arc::new(tokio::sync::Semaphore::new(permits))
    });
    &GATE
}

/// Serialize one persistent checkout across every replica sharing the storage
/// root. The process-local guard prevents duplicate work in this binary; the
/// PostgreSQL guard prevents two binaries from mutating the same `.git`
/// directory concurrently.
pub async fn lock_checkout_distributed(
    pool: &sqlx::PgPool,
    path: &Path,
) -> AppResult<CheckoutLockGuard> {
    let key = checkout_lock_key(path);
    let checkout_lock = {
        let mut locks = CHECKOUT_LOCKS.lock().unwrap_or_else(|e| e.into_inner());
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
            lock
        } else {
            let lock = Arc::new(CheckoutMutex::new(()));
            locks.insert(key.clone(), Arc::downgrade(&lock));
            lock
        }
    };
    let local = checkout_lock.lock_owned().await;
    // A repository scan can hold this checkout lock while it briefly takes an
    // A&D configuration lock and performs ordinary queries. Bound that nesting
    // independently from container provisioning so scans cannot reserve the
    // entire pool or consume provisioning permits for the duration of a clone.
    let checkout_permit = checkout_gate()
        .clone()
        .acquire_owned()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let distributed = crate::utils::single_flight::PgAdvisoryLock::acquire(
        pool,
        &format!("git-checkout:{}", key.display()),
    )
    .await
    .map_err(|error| AppError::internal(format!("lock shared repository checkout: {error}")))?;
    Ok(CheckoutLockGuard {
        _guard: local,
        _distributed: Some(distributed),
        _checkout_permit: Some(checkout_permit),
    })
}

fn checkout_lock_key(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut current = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        if let Ok(mut canonical) = std::fs::canonicalize(current) {
            for component in missing.iter().rev() {
                canonical.push(component);
            }
            return canonical;
        }
        let Some(name) = current.file_name() else {
            return absolute;
        };
        missing.push(name.to_os_string());
        let Some(parent) = current.parent() else {
            return absolute;
        };
        current = parent;
    }
}

/// A GitHub personal access token used to authenticate `git` against a private
/// repository over HTTPS.
///
/// The token is embedded into the clone/fetch URL as Basic-auth userinfo by
/// [`GitCredentials::apply`]. It is never written to `.git/config` (the URL we
/// pass on the command line is transient to that one invocation) and is scrubbed
/// from any error text via [`sanitize`].
#[derive(Clone)]
pub struct GitCredentials {
    /// The PAT. Empty means "no credentials" — [`apply`](Self::apply) then
    /// returns the URL unchanged, which is correct for public repos.
    pub token: String,
}

impl GitCredentials {
    /// Construct credentials from a token string.
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
        }
    }

    /// Rewrite an `https://` URL to embed the token as `x-access-token:<pat>`
    /// Basic-auth userinfo.
    ///
    /// Any userinfo already present on the URL is stripped first, so calling this
    /// twice is idempotent and an operator-supplied URL that already carries a
    /// (possibly stale) credential is overridden. Non-`https` URLs and an empty
    /// token pass through unchanged.
    ///
    /// Note: the token is embedded verbatim. GitHub PATs are URL-safe
    /// (`[A-Za-z0-9_]`), so no percent-encoding is required for them; a token
    /// containing `/`, `@`, or `:` would need encoding — out of scope here since
    /// this path is GitHub-only.
    pub fn apply(&self, url: &str) -> String {
        if self.token.is_empty() {
            return url.to_string();
        }
        let Ok(mut parsed) = reqwest::Url::parse(url) else {
            return url.to_string();
        };
        if parsed.scheme() != "https"
            || parsed.set_username(GIT_AUTH_USER).is_err()
            || parsed.set_password(Some(&self.token)).is_err()
        {
            return url.to_string();
        }
        parsed.to_string()
    }
}

/// Shallow-clone the repo at `url` into `dest`, or fast-forward it if `dest`
/// already holds a checkout.
///
/// On first sync (`dest/.git` absent) this runs
/// `git clone --depth 1 --single-branch [--branch <branch>] <url> <dest>`. On a
/// subsequent sync it refreshes the remote URL (an operator may have edited it),
/// clears any stale `*.lock` left by an interrupted prior run, then does
/// `fetch --depth 1` + `reset --hard FETCH_HEAD` + `clean -fdx` so the working
/// tree exactly matches the requested ref with no history accumulation and no
/// leftover untracked files from a previous import.
///
/// `branch` is the branch or tag to check out; `None` tracks the upstream default
/// branch. Pass an authenticated URL (see [`GitCredentials::apply`]) for private
/// repos.
///
/// Any non-zero `git` exit or a timeout maps to [`AppError::internal`] carrying
/// the (credential-scrubbed) stderr.
pub async fn sync_repo(url: &str, branch: Option<&str>, dest: &Path) -> AppResult<()> {
    let url = validate_sync_repo_url(url)?;
    let transport = resolve_git_transport(&url).await?;
    let clean_url = url_without_credentials(&url)?;
    let branch = validate_git_ref(branch)?;
    let branch = branch.as_deref();
    let git_dir = dest.join(".git");

    if git_dir.exists() {
        let cleared = clear_stale_git_locks(&git_dir).await;
        if cleared > 0 {
            tracing::warn!(
                dir = %dest.display(),
                count = cleared,
                "git_sync: cleared stale git lock(s) left by an interrupted prior sync"
            );
        }

        tracing::debug!(url = %redact_url(&url), dir = %dest.display(), "git_sync: fetching");
        // Keep the remote URL current in case the binding's URL/token changed.
        run_git(dest, &["remote", "set-url", "origin", &clean_url]).await?;
        // "--" so a ref beginning with '-' is treated as a refspec, not an option.
        let refspec = branch.unwrap_or("HEAD");
        run_git_with_transport(
            Some(dest),
            &transport,
            &["fetch", "--depth", "1", "--", &url, refspec],
        )
        .await?;
        // FETCH_HEAD always points at whatever we just fetched.
        run_git(dest, &["reset", "--hard", "FETCH_HEAD"]).await?;
        // Drop untracked files a previous import may have written (e.g. build
        // artifacts). Safe: the checkout dir is internal, nothing else touches it.
        run_git(dest, &["clean", "-fdx"]).await?;
    } else {
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await.map_err(|e| {
                    AppError::internal(format!(
                        "git_sync: create parent dir {}: {e}",
                        parent.display()
                    ))
                })?;
            }
        }

        let dest_str = dest
            .to_str()
            .ok_or_else(|| AppError::internal("git_sync: destination path is not valid UTF-8"))?;

        tracing::info!(url = %redact_url(&url), dir = %dest.display(), "git_sync: cloning");
        let mut args: Vec<&str> = vec!["clone", "--depth", "1", "--single-branch"];
        if let Some(b) = branch {
            args.push("--branch");
            args.push(b);
        }
        args.push("--");
        args.push(&url);
        args.push(dest_str);

        // Clone runs from the parent dir; git creates `dest` itself.
        let cwd = dest.parent().filter(|p| !p.as_os_str().is_empty());
        run_git_with_transport(cwd, &transport, &args).await?;
        // Clone records its authenticated URL as origin; scrub credentials as
        // soon as the checkout exists so a PAT never persists in `.git/config`.
        run_git(dest, &["remote", "set-url", "origin", &clean_url]).await?;
    }

    if let Err(e) = validate_checkout_tree(dest).await {
        // Do not retain an oversized attacker-controlled checkout on disk. The
        // checkout lock held by persistent callers makes this removal race-free.
        let _ = tokio::fs::remove_dir_all(dest).await;
        return Err(e);
    }
    Ok(())
}

pub(super) async fn validate_checkout_tree(root: &Path) -> AppResult<()> {
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut entries_seen = 0usize;
    let mut files_seen = 0usize;
    let mut total_bytes = 0u64;
    while let Some((current, depth)) = stack.pop() {
        if depth > MAX_REPO_DEPTH {
            return Err(AppError::bad_request("repository tree is too deep"));
        }
        let mut entries = tokio::fs::read_dir(&current).await.map_err(|e| {
            AppError::internal(format!("git_sync: read_dir {}: {e}", current.display()))
        })?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| AppError::internal(format!("git_sync: read repository entry: {e}")))?
        {
            if depth == 0 && entry.file_name() == OsStr::new(".git") {
                continue;
            }
            entries_seen += 1;
            if entries_seen > MAX_REPO_ENTRIES {
                return Err(AppError::bad_request(
                    "repository contains too many entries",
                ));
            }
            let file_type = entry.file_type().await.map_err(|e| {
                AppError::internal(format!("git_sync: stat {}: {e}", entry.path().display()))
            })?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                stack.push((entry.path(), depth + 1));
            } else if file_type.is_file() {
                files_seen += 1;
                if files_seen > MAX_REPO_FILES {
                    return Err(AppError::bad_request("repository contains too many files"));
                }
                let len = entry
                    .metadata()
                    .await
                    .map_err(|e| {
                        AppError::internal(format!(
                            "git_sync: stat {}: {e}",
                            entry.path().display()
                        ))
                    })?
                    .len();
                if len > MAX_REPO_FILE_BYTES
                    || total_bytes.saturating_add(len) > MAX_REPO_TOTAL_BYTES
                {
                    return Err(AppError::bad_request("repository exceeds the size limit"));
                }
                total_bytes = total_bytes.saturating_add(len);
            }
        }
    }
    Ok(())
}

/// Return the SHA the checkout at `dest` currently points at (`git rev-parse
/// HEAD`). Useful for recording the synced commit and short-circuiting an import
/// when the SHA hasn't moved since the last scan.
pub async fn head_sha(dest: &Path) -> AppResult<String> {
    let out = run_git(dest, &["rev-parse", "HEAD"]).await?;
    Ok(out.trim().to_string())
}

/// Remove stale `*.lock` files under a `.git` directory left by a git process
/// that was killed mid-operation. Sweeps recursively so nested ref locks
/// (`refs/heads/<branch>.lock`, `logs/…`) are cleared too, not just top-level
/// ones. Best-effort: callers hold the per-binding lock, so nothing legitimately
/// owns these. Returns the number removed.
async fn clear_stale_git_locks(git_dir: &Path) -> usize {
    let mut cleared = 0usize;
    let mut stack = vec![git_dir.to_path_buf()];

    while let Some(current) = stack.pop() {
        let Ok(mut entries) = tokio::fs::read_dir(&current).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            match entry.file_type().await {
                Ok(ft) if ft.is_dir() => stack.push(path),
                Ok(ft)
                    if ft.is_file()
                        && path.extension().and_then(OsStr::to_str) == Some("lock")
                        && tokio::fs::remove_file(&path).await.is_ok() =>
                {
                    cleared += 1;
                }
                _ => {}
            }
        }
    }

    cleared
}

/// Run `git <args>` in `cwd`, returning captured stdout on success.
pub(super) async fn run_git(cwd: &Path, args: &[&str]) -> AppResult<String> {
    run_git_opt_cwd(Some(cwd), args).await
}

/// Run one HTTP(S) Git operation with the URL's current public DNS answers
/// pinned into libcurl and redirects disabled. This closes both DNS-rebinding
/// and redirect pivots between validation and the outbound socket.
pub(super) async fn run_git_network(cwd: &Path, url: &str, args: &[&str]) -> AppResult<String> {
    let url = validate_sync_repo_url(url)?;
    let transport = resolve_git_transport(&url).await?;
    run_git_with_transport(Some(cwd), &transport, args).await
}

async fn run_git_with_transport(
    cwd: Option<&Path>,
    transport: &GitTransportPolicy,
    args: &[&str],
) -> AppResult<String> {
    let configured = transport.command_args(args);
    let configured = configured.iter().map(String::as_str).collect::<Vec<_>>();
    run_git_opt_cwd(cwd, &configured).await
}

/// Run `git <args>`, optionally in `cwd` (else the process's current dir).
///
/// stdout is captured and returned; stderr is captured only for error messages.
/// A non-zero exit or a [`GIT_COMMAND_TIMEOUT`] overrun maps to
/// [`AppError::internal`] with credential-scrubbed stderr. Interactive credential
/// prompts are disabled (`GIT_TERMINAL_PROMPT=0`) so a misconfigured private repo
/// fails fast instead of hanging the worker.
async fn run_git_opt_cwd(cwd: Option<&Path>, args: &[&str]) -> AppResult<String> {
    let mut cmd = Command::new("git");
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        // A repository-controlled .lfsconfig must not create a second network
        // request outside the pinned smart-HTTP transport.
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // On a timeout the `cmd.output()` future is dropped; without this tokio
        // leaves the wedged git running (orphaned, still holding its *.lock).
        // Kill-on-drop restores RSCTF's kill-on-timeout so the next sync's stale-
        // lock sweep never races a live process.
        .kill_on_drop(true);

    let output = match timeout(GIT_COMMAND_TIMEOUT, cmd.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            return Err(AppError::internal(format!(
                "git {}: failed to run (is git installed?): {e}",
                sanitize(&args.join(" "))
            )));
        }
        Err(_) => {
            return Err(AppError::internal(format!(
                "git {} timed out after {}s",
                sanitize(&args.join(" ")),
                GIT_COMMAND_TIMEOUT.as_secs()
            )));
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_string());
        return Err(AppError::internal(format!(
            "git {} exited {}: {}",
            sanitize(&args.join(" ")),
            code,
            sanitize(stderr.trim())
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Redact an embedded `x-access-token:<pat>@` Basic-auth credential from a string
/// so a PAT never lands in an error message or log line, even if git echoes the
/// URL it was handed.
fn sanitize(s: &str) -> String {
    const MARKER: &str = "x-access-token:";
    if !s.contains(MARKER) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(idx) = rest.find(MARKER) {
        out.push_str(&rest[..idx]);
        out.push_str(MARKER);
        out.push_str("***");
        // Resume after the userinfo terminator so the host/path stay visible.
        let after = &rest[idx + MARKER.len()..];
        match after.find('@') {
            Some(at) => rest = &after[at..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Convenience wrapper to scrub a URL for logging.
fn redact_url(url: &str) -> String {
    sanitize(url)
}

#[cfg(test)]
mod transport_tests {
    use super::*;

    #[test]
    fn outbound_policy_rejects_internal_and_special_addresses() {
        for address in [
            "0.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "192.168.0.1",
            "198.18.0.1",
            "224.0.0.1",
            "::1",
            "::ffff:127.0.0.1",
            "64:ff9b:1::1",
            "2001:db8::1",
            "2002:7f00:1::1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
        ] {
            let address = address.parse::<IpAddr>().unwrap();
            assert!(
                !is_public_git_ip(address),
                "accepted special address {address}"
            );
        }

        for address in ["1.1.1.1", "93.184.216.34", "2606:4700:4700::1111"] {
            let address = address.parse::<IpAddr>().unwrap();
            assert!(
                is_public_git_ip(address),
                "rejected public address {address}"
            );
        }
    }

    #[test]
    fn a_single_private_dns_answer_rejects_the_complete_resolution() {
        let result = pinned_transport_policy(
            "git.example",
            443,
            [
                "93.184.216.34".parse().unwrap(),
                "127.0.0.1".parse().unwrap(),
            ],
        );
        assert!(result.is_err());
    }

    #[test]
    fn transport_disables_redirects_and_pins_every_validated_address() {
        let policy = pinned_transport_policy(
            "git.example",
            443,
            [
                "2606:4700:4700::1111".parse().unwrap(),
                "93.184.216.34".parse().unwrap(),
            ],
        )
        .unwrap();
        let args = policy.command_args(&["fetch", "https://git.example/team/repo"]);
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-c", "http.followRedirects=false"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-c", "http.curloptResolve="]));
        assert!(args.iter().any(|arg| {
            arg == "http.curloptResolve=git.example:443:93.184.216.34,[2606:4700:4700::1111]"
        }));
    }

    #[test]
    fn public_ip_literals_need_no_dns_override() {
        let policy =
            pinned_transport_policy("93.184.216.34", 443, ["93.184.216.34".parse().unwrap()])
                .unwrap();
        assert!(policy.curl_resolve.is_none());
    }
}
