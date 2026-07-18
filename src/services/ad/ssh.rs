//! In-process A&D SSH bastion (russh). A player runs the copy-paste command the
//! A&D panel shows — `ssh <challengeId>@<host> -p <port>` — and the bastion:
//!   1. authenticates them by the SSH key they registered in the Toolkit
//!      (`AdSshKeys`, matched on SHA256 fingerprint), and
//!   2. `docker exec`s an interactive shell straight into THAT team's container
//!      for the challenge in the username.
//!
//! No sshd is needed inside the challenge image; it reuses the Docker socket.
//! A deliberate divergence from RSCTF's external-bastion + relay model
//! (see the ad-vpn-in-process-decision memory).
//!
//! Best-effort: any bind/keygen failure just logs and disables the bastion —
//! it never blocks startup.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bollard::exec::{CreateExecOptions, ResizeExecOptions, StartExecResults};
use bollard::Docker;
use futures::StreamExt;
use russh::keys::ssh_key::private::{Ed25519Keypair, KeypairData};
use russh::keys::{HashAlg, PrivateKey, PublicKey};
use russh::server::{Auth, Handler, Msg, Server as _, Session};
use russh::{Channel, ChannelId, Disconnect, MethodKind, MethodSet};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::app_state::SharedState;
use crate::models::data::{
    ad_ssh_key, ad_team_service, config, game, game_challenge, participation, team, team_member,
    user,
};
use crate::utils::enums::{ChallengeReviewStatus, ChallengeType, ParticipationStatus, Role};

/// Per-team SSH shell throttle. An interactive bastion isn't throughput-bound; the
/// real risk is one team exhausting resources by opening thousands of shells (each a
/// docker exec / tunnel stream / PTY). Cap concurrent shells + the new-shell rate,
/// keyed by participation.
const MAX_CONCURRENT_SHELLS: usize = 5;
const MAX_SHELLS_PER_MINUTE: usize = 30;

#[derive(Default)]
struct TeamShells {
    active: usize,
    recent: std::collections::VecDeque<std::time::Instant>,
}
static SHELL_THROTTLE: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<i32, TeamShells>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// RAII slot: decrements a team's active-shell count on drop. Stored on the
/// connection handler so an abruptly-dropped connection (kill -9) still frees it.
struct ShellSlot {
    pid: i32,
}
impl Drop for ShellSlot {
    fn drop(&mut self) {
        let mut map = SHELL_THROTTLE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ts) = map.get_mut(&self.pid) {
            ts.active = ts.active.saturating_sub(1);
        }
    }
}

/// Reserve a shell slot for a team, enforcing the concurrent + per-minute caps.
fn acquire_shell_slot(pid: i32) -> Result<ShellSlot, &'static str> {
    let now = std::time::Instant::now();
    let mut map = SHELL_THROTTLE.lock().unwrap_or_else(|e| e.into_inner());
    let ts = map.entry(pid).or_default();
    ts.recent
        .retain(|t| now.duration_since(*t) < std::time::Duration::from_secs(60));
    if ts.active >= MAX_CONCURRENT_SHELLS {
        return Err("too many open shells for your team (max 5) — close one first");
    }
    if ts.recent.len() >= MAX_SHELLS_PER_MINUTE {
        return Err("opening shells too fast — wait a moment and retry");
    }
    ts.active += 1;
    ts.recent.push_back(now);
    Ok(ShellSlot { pid })
}

const HOST_KEY_CFG: &str = "Ad:Ssh:HostKey";

fn listen_port() -> u16 {
    std::env::var("RSCTF_AD_SSH_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2222)
}

/// `host:port` a player SSHes to — shown in the A&D panel. Defaults to the public
/// entry host on the bastion port.
pub fn jump_host() -> Option<String> {
    let host = std::env::var("RSCTF_AD_SSH_PUBLIC_HOST")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var("RSCTF_DOCKER_PUBLIC_ENTRY")
                .ok()
                .filter(|s| !s.is_empty())
        })?;
    Some(format!("{host}:{}", listen_port()))
}

/// Spawn the best-effort bastion listener as a shutdown-aware background task.
///
/// The returned handle deliberately is not a required-worker health signal: a
/// missing bastion must not take the API or round engine down. The process
/// lifecycle still tracks it so the socket and active SSH sessions are closed
/// before network ownership is released.
pub fn start(st: SharedState, shutdown: watch::Receiver<bool>) -> JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(e) = run(st, shutdown).await {
            tracing::warn!(error = %e, "ad_ssh: bastion stopped");
        }
    })
}

async fn run(
    st: SharedState,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if *shutdown.borrow() {
        return Ok(());
    }
    let host_key = tokio::select! {
        biased;
        _ = wait_for_shutdown(&mut shutdown) => return Ok(()),
        key = load_host_key(&st) => key,
    };
    let config = Arc::new(russh::server::Config {
        methods: MethodSet::from(&[MethodKind::PublicKey][..]),
        auth_rejection_time: Duration::from_secs(1),
        auth_rejection_time_initial: Some(Duration::from_millis(250)),
        max_auth_attempts: 3,
        inactivity_timeout: Some(Duration::from_secs(120)),
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 3,
        keys: vec![host_key],
        ..Default::default()
    });
    let addr = format!("0.0.0.0:{}", listen_port());
    let listener = tokio::select! {
        biased;
        _ = wait_for_shutdown(&mut shutdown) => return Ok(()),
        result = TcpListener::bind(&addr) => result?,
    };
    tracing::info!(addr = %addr, "ad_ssh: A&D SSH bastion listening");
    let mut server = Bastion { st };
    let mut running = server.run_on_socket(config, &listener);
    let handle = running.handle();
    tokio::select! {
        biased;
        _ = wait_for_shutdown(&mut shutdown) => {
            handle.shutdown("rsctf is shutting down".to_string());
            running.await?;
        },
        result = &mut running => result?,
    }
    Ok(())
}

async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    while !*shutdown.borrow() {
        if shutdown.changed().await.is_err() {
            break;
        }
    }
}

/// Load the persisted Ed25519 host key (stable across restarts so clients don't
/// see host-key-changed warnings), generating + storing one on first use.
async fn load_host_key(st: &SharedState) -> PrivateKey {
    if let Ok(Some(row)) = config::Entity::find_by_id(HOST_KEY_CFG.to_string())
        .one(&st.db)
        .await
    {
        if let Some(pem) = row.value {
            if let Ok(key) = PrivateKey::from_openssh(&pem) {
                return key;
            }
        }
    }
    // Generate from a CSPRNG seed without coupling this module to russh's
    // rand_core version.
    let mut seed = [0u8; 32];
    rand::fill(&mut seed);
    let key = PrivateKey::new(
        KeypairData::Ed25519(Ed25519Keypair::from_seed(&seed)),
        "rsctf-ad-bastion",
    )
    .expect("ed25519 host key");
    if let Ok(pem) = key.to_openssh(russh::keys::ssh_key::LineEnding::LF) {
        let pem = pem.to_string();
        let saved = match config::Entity::find_by_id(HOST_KEY_CFG.to_string())
            .one(&st.db)
            .await
        {
            Ok(Some(existing)) => {
                let mut am: config::ActiveModel = existing.into();
                am.value = Set(Some(pem));
                am.update(&st.db).await.is_ok()
            }
            _ => config::ActiveModel {
                config_key: Set(HOST_KEY_CFG.to_string()),
                value: Set(Some(pem)),
                cache_keys: Set(None),
            }
            .insert(&st.db)
            .await
            .is_ok(),
        };
        if !saved {
            tracing::warn!("ad_ssh: could not persist host key (using ephemeral)");
        }
    }
    key
}

/// Re-resolve every mutable grant behind a team-shared SSH key. This is used at
/// authentication, before opening a shell, and periodically while it is open so
/// deleting a key, rejecting a participation, banning a member, ending a game, or
/// disabling a challenge revokes access without waiting for the TCP session to end.
async fn validate_ssh_access(
    st: &SharedState,
    key_id: i32,
    participation_id: i32,
    challenge_id: i32,
) -> Result<game_challenge::Model, &'static str> {
    ad_ssh_key::Entity::find_by_id(key_id)
        .one(&st.db)
        .await
        .map_err(|_| "database error")?
        .filter(|key| key.participation_id == participation_id)
        .ok_or("SSH key has been revoked")?;

    let part = participation::Entity::find_by_id(participation_id)
        .one(&st.db)
        .await
        .map_err(|_| "database error")?
        .filter(|part| part.status == ParticipationStatus::Accepted)
        .ok_or("team is not accepted for this game")?;
    let game = game::Entity::find_by_id(part.game_id)
        .one(&st.db)
        .await
        .map_err(|_| "database error")?
        .filter(|game| game.is_active(chrono::Utc::now()))
        .ok_or("game is not active")?;
    let challenge = game_challenge::Entity::find_by_id(challenge_id)
        .one(&st.db)
        .await
        .map_err(|_| "database error")?
        .filter(|challenge| {
            challenge.game_id == game.id
                && challenge.challenge_type == ChallengeType::AttackDefense
                && challenge.is_enabled
                && challenge.review_status == ChallengeReviewStatus::Active
        })
        .ok_or("challenge is not available for SSH")?;

    let team = team::Entity::find_by_id(part.team_id)
        .one(&st.db)
        .await
        .map_err(|_| "database error")?
        .ok_or("team no longer exists")?;
    let mut member_ids: std::collections::BTreeSet<uuid::Uuid> = team_member::Entity::find()
        .filter(team_member::Column::TeamId.eq(team.id))
        .all(&st.db)
        .await
        .map_err(|_| "database error")?
        .into_iter()
        .map(|member| member.user_id)
        .collect();
    member_ids.insert(team.captain_id);
    let roster = user::Entity::find()
        .filter(user::Column::Id.is_in(member_ids.iter().copied()))
        .all(&st.db)
        .await
        .map_err(|_| "database error")?;
    if roster.len() != member_ids.len() || roster.iter().any(|member| member.role == Role::Banned) {
        return Err("team roster is not eligible for SSH");
    }

    Ok(challenge)
}

#[derive(Clone)]
struct Bastion {
    st: SharedState,
}

impl russh::server::Server for Bastion {
    type Handler = BastionHandler;
    fn new_client(&mut self, _peer: Option<std::net::SocketAddr>) -> BastionHandler {
        BastionHandler {
            st: self.st.clone(),
            ssh_key_id: None,
            participation_id: None,
            challenge_id: None,
            cols: 80,
            rows: 24,
            exec_id: None,
            exec_input: None,
            shell_slot: None,
            byoc_exec: None,
        }
    }
}

struct BastionHandler {
    st: SharedState,
    /// The concrete credential row must remain present for the session's lease.
    ssh_key_id: Option<i32>,
    /// Resolved at auth time from the offered key's registered `AdSshKey`.
    participation_id: Option<i32>,
    /// The SSH username — the challenge id to shell into.
    challenge_id: Option<i32>,
    cols: u32,
    rows: u32,
    exec_id: Option<String>,
    exec_input: Option<Pin<Box<dyn AsyncWrite + Send>>>,
    /// Throttle slot (concurrent + rate cap) — held for the connection's life so a
    /// dropped connection (even kill -9) frees it. Anchored to the handler, NOT the
    /// pump task, which can exit for reasons the connection outlives.
    shell_slot: Option<ShellSlot>,
    /// Keeps the BYOC tunnel fast-polling while a shell is open (else keystroke I/O
    /// is 50ms-batched). Dropped with the handler on connection close.
    byoc_exec: Option<crate::services::byoc_tunnel::ExecGuard>,
}

impl BastionHandler {
    /// Look up the team's container for the authenticated (participation, challenge),
    /// `docker exec` an interactive shell, and bridge it to the SSH channel. Returns
    /// a user-facing error string on any failure.
    async fn open_shell(
        &mut self,
        channel: ChannelId,
        handle: russh::server::Handle,
    ) -> Result<(), String> {
        if self.shell_slot.is_some() || self.exec_input.is_some() || self.byoc_exec.is_some() {
            return Err("this SSH connection already has an active shell".to_string());
        }
        let pid = self.participation_id.ok_or("not authenticated")?;
        let key_id = self.ssh_key_id.ok_or("not authenticated")?;
        let cid = self
            .challenge_id
            .ok_or("connect as ssh <challenge-id>@host (the number before @)")?;

        let challenge = validate_ssh_access(&self.st, key_id, pid, cid).await?;

        // Throttle BEFORE opening anything (docker exec or tunnel stream); the slot
        // lives on the handler, so a dropped connection releases it.
        self.shell_slot = Some(acquire_shell_slot(pid)?);

        // Self-hosted (BYOC) challenge: the service runs on the team's own machine,
        // reachable only via its agent tunnel — route the shell over an 'E' stream
        // (the agent docker-exec's a shell in ITS service container). Platform-hosted
        // challenges fall through to the local docker-exec path below.
        if challenge.ad_self_hosted {
            return self
                .open_shell_byoc(key_id, pid, cid, channel, handle)
                .await;
        }

        let svc = ad_team_service::Entity::find()
            .filter(ad_team_service::Column::ParticipationId.eq(pid))
            .filter(ad_team_service::Column::ChallengeId.eq(cid))
            .filter(ad_team_service::Column::GameId.eq(challenge.game_id))
            .one(&self.st.db)
            .await
            .map_err(|_| "database error")?
            .ok_or("no A&D service for that challenge on your team")?;
        let container = svc
            .container_id
            .filter(|c| !c.is_empty())
            .ok_or("your container isn't running yet — ask the operator to Ensure containers")?;

        let docker = Docker::connect_with_local_defaults().map_err(|_| "docker unavailable")?;
        let exec = docker
            .create_exec(
                &container,
                CreateExecOptions {
                    attach_stdin: Some(true),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    tty: Some(true),
                    env: Some(vec![
                        "TERM=xterm-256color".to_string(),
                        format!("COLUMNS={}", self.cols),
                        format!("LINES={}", self.rows),
                    ]),
                    cmd: Some(vec![
                        "/bin/sh".to_string(),
                        "-c".to_string(),
                        "if command -v bash >/dev/null 2>&1; then exec bash; else exec sh; fi"
                            .to_string(),
                    ]),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| format!("could not start a shell: {e}"))?;
        self.exec_id = Some(exec.id.clone());

        let started = docker
            .start_exec(&exec.id, None)
            .await
            .map_err(|e| format!("could not start a shell: {e}"))?;
        let StartExecResults::Attached { mut output, input } = started else {
            return Err("shell did not attach".to_string());
        };
        self.exec_input = Some(input);
        let _ = docker
            .resize_exec(
                &exec.id,
                ResizeExecOptions {
                    height: self.rows as u16,
                    width: self.cols as u16,
                },
            )
            .await;

        // Pump container output → SSH channel until the shell exits.
        let st = self.st.clone();
        tokio::spawn(async move {
            let mut lease = tokio::time::interval(Duration::from_secs(15));
            lease.tick().await;
            loop {
                tokio::select! {
                    chunk = output.next() => match chunk {
                        Some(Ok(log)) => {
                            if handle.data(channel, log.into_bytes()).await.is_err() {
                                break;
                            }
                        }
                        Some(Err(_)) | None => break,
                    },
                    _ = lease.tick() => {
                        if validate_ssh_access(&st, key_id, pid, cid).await.is_err() {
                            let _ = handle.disconnect(
                                Disconnect::ByApplication,
                                "SSH authorization revoked".to_string(),
                                String::new(),
                            ).await;
                            break;
                        }
                    }
                }
            }
            let _ = handle.eof(channel).await;
            let _ = handle.close(channel).await;
        });
        Ok(())
    }

    /// Route an interactive shell to a self-hosted (BYOC) service over the team's
    /// agent tunnel: open an `'E'` stream (the agent docker-exec's a shell in ITS
    /// service container) and bridge it to the SSH channel. `exec_id` stays `None`, so
    /// the local-docker resize path in `window_change_request` is cleanly skipped.
    async fn open_shell_byoc(
        &mut self,
        key_id: i32,
        pid: i32,
        cid: i32,
        channel: ChannelId,
        handle: russh::server::Handle,
    ) -> Result<(), String> {
        let (stream, guard) = crate::services::byoc_tunnel::open_exec_stream(
            &self.st,
            pid,
            cid,
            self.cols as u16,
            self.rows as u16,
        )
        .await
        .ok_or("your BYOC agent isn't connected — run the setup.sh bundle and retry")?;
        self.byoc_exec = Some(guard);

        // Split the tunnel stream: the write half receives SSH input (via `data`), the
        // read half is pumped to the SSH channel. yamux is futures-IO, so compat it to
        // tokio-IO to match `exec_input`'s box.
        use tokio_util::compat::{FuturesAsyncReadCompatExt, FuturesAsyncWriteCompatExt};
        let (rd, wr) = futures::AsyncReadExt::split(stream);
        self.exec_input = Some(Box::pin(wr.compat_write()));
        let mut rd = rd.compat();
        let st = self.st.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let mut lease = tokio::time::interval(Duration::from_secs(15));
            lease.tick().await;
            loop {
                tokio::select! {
                    read = tokio::io::AsyncReadExt::read(&mut rd, &mut buf) => match read {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if handle
                                .data(channel, bytes::Bytes::copy_from_slice(&buf[..n]))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    },
                    _ = lease.tick() => {
                        if validate_ssh_access(&st, key_id, pid, cid).await.is_err() {
                            let _ = handle.disconnect(
                                Disconnect::ByApplication,
                                "SSH authorization revoked".to_string(),
                                String::new(),
                            ).await;
                            break;
                        }
                    }
                }
            }
            let _ = handle.eof(channel).await;
            let _ = handle.close(channel).await;
        });
        Ok(())
    }
}

impl Handler for BastionHandler {
    type Error = russh::Error;

    async fn auth_publickey(
        &mut self,
        user: &str,
        public_key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        let Ok(challenge_id) = user.trim().parse::<i32>() else {
            return Ok(Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            });
        };
        let fingerprint = public_key.fingerprint(HashAlg::Sha256).to_string();
        match ad_ssh_key::Entity::find()
            .filter(ad_ssh_key::Column::Fingerprint.eq(&fingerprint))
            .one(&self.st.db)
            .await
        {
            Ok(Some(key))
                if validate_ssh_access(&self.st, key.id, key.participation_id, challenge_id)
                    .await
                    .is_ok() =>
            {
                self.ssh_key_id = Some(key.id);
                self.participation_id = Some(key.participation_id);
                self.challenge_id = Some(challenge_id);
                // Best-effort last-used bump.
                let mut am: ad_ssh_key::ActiveModel = key.into();
                am.last_used_at_utc = Set(Some(chrono::Utc::now()));
                let _ = am.update(&self.st.db).await;
                Ok(Auth::Accept)
            }
            _ => Ok(Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            }),
        }
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.cols = col_width;
        self.rows = row_height;
        let _ = session.handle().channel_success(channel).await;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let handle = session.handle();
        match self.open_shell(channel, handle.clone()).await {
            Ok(()) => {
                let _ = handle.channel_success(channel).await;
            }
            Err(msg) => {
                self.exec_input = None;
                self.byoc_exec = None;
                self.shell_slot = None;
                let _ = session.data(channel, format!("\r\n{msg}\r\n").into_bytes());
                let _ = handle.close(channel).await;
            }
        }
        Ok(())
    }

    async fn data(
        &mut self,
        _channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(input) = self.exec_input.as_mut() {
            let _ = input.write_all(data).await;
            let _ = input.flush().await;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn window_change_request(
        &mut self,
        _channel: ChannelId,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.cols = col_width;
        self.rows = row_height;
        if let Some(id) = self.exec_id.clone() {
            if let Ok(docker) = Docker::connect_with_local_defaults() {
                let _ = docker
                    .resize_exec(
                        &id,
                        ResizeExecOptions {
                            height: row_height as u16,
                            width: col_width as u16,
                        },
                    )
                    .await;
            }
        }
        Ok(())
    }
}
