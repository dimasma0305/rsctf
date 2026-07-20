//! BYOC ("bring your own container") self-hosted A&D onboarding, ported from
//! RSCTF's `AdGameController` `Byoc/*` surface.
//!
//! For a challenge flagged `ad_self_hosted`, a team runs the vulnerable service
//! on their **own** machine and joins it to the game over a single outbound
//! connection — no public IP, inbound firewall rule, or VPN needed. The client
//! (`AdChallengePanel.tsx`) renders two download buttons that hit:
//!
//!   * `GET Byoc/Setup/{challengeId}`   → a one-command `setup.sh`
//!   * `GET Byoc/Compose/{challengeId}` → a `docker-compose.yml` to fill in
//!
//! Both are gated on an **accepted** A&D participant and stream the file as a
//! `text/plain` attachment, so the buttons work end-to-end.
//!
//! ## Best-effort divergences from RSCTF (relay infra not wired in rsctf)
//!
//! RSCTF's BYOC runs a WebSocket tunnel *relay* (`Byoc/Agent`) and streams the
//! real service image (`Byoc/Image`). rsctf wires both in-process:
//!
//!   * `Byoc/Agent` upgrades to the in-process yamux tunnel (`services::byoc_tunnel`).
//!   * `Byoc/Image` streams the challenge's built SERVICE image as a `docker save`
//!     tarball, so `setup.sh` `docker load`s it and runs the REAL service.
//!   * `setup.sh`'s image pull stays **best-effort** (non-fatal): if the image
//!     hasn't been built yet it falls back to a self-contained placeholder
//!     service (a digest-pinned `alpine/socat` image serving the rotating flag)
//!     so the script still runs — but the placeholder won't pass a functional
//!     checker.
//!   * The generated bundle also embeds the deterministic WireGuard config
//!     (reusing `vpn.rs::render_wg_config`) as an L3 connectivity fallback.
//!
//! BYOC tokens (`adbyocagent:` / `adbyocimage:`) are derived from the game secret,
//! a rotatable team secret, and `(participation, challenge)`. Every handshake also
//! revalidates the participation and challenge state against the database.

use axum::http::header;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::*;
mod agent_image;
use agent_image::{default_byoc_agent_image, immutable_agent_image};

/// Immutable multi-platform placeholder (amd64/arm64/arm/ppc64le/s390x).
const DEFAULT_BYOC_FALLBACK_IMAGE: &str =
    "docker.io/alpine/socat@sha256:4e625a62c9ea40ccbce93b9a4fcc6b41740a9f308389c216f34c88ce3abb275b";
const BYOC_SECRET_CACHE_CONTROL: &str = "private, no-store";

/// Deterministic per-`(participation, challenge)` BYOC token, hex-encoded.
/// `domain` domain-separates the agent/image tokens (mirrors RSCTF's
/// `AdTokenUtils.ByocAgentToken` / `ByocImageToken` prefixes).
pub(crate) fn byoc_token(
    domain: &str,
    game_secret: &str,
    team_secret: &str,
    participation_id: i32,
    challenge_id: i32,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update(game_secret.as_bytes());
    hasher.update(team_secret.as_bytes());
    hasher.update(participation_id.to_le_bytes());
    hasher.update(challenge_id.to_le_bytes());
    hex::encode(hasher.finalize())
}

/// A filesystem-safe, unique-per-challenge slug from title + id, so a team doing
/// several BYOC challenges gets distinct download filenames — RSCTF `Slugify`.
fn slugify(title: &str, challenge_id: i32) -> String {
    let mut out = String::new();
    for ch in title.to_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let slug = out.trim_matches('-');
    if slug.is_empty() {
        format!("challenge-{challenge_id}")
    } else {
        format!("{slug}-{challenge_id}")
    }
}

/// The self-hosted A&D challenge a BYOC download targets, plus the derived
/// game-scoped connection material shared by `setup.sh` and the compose.
struct ByocContext {
    title: String,
    container_image: Option<String>,
    svc_port: i32,
    tunnel_url: String,
    image_url: String,
    agent_image: String,
    agent_image_requires_amd64: bool,
    wg_config: String,
}

const MAX_CONCURRENT_IMAGE_EXPORTS: usize = 4;
const MAX_IMAGE_EXPORT_CAPABILITY_KEYS: usize = 1_024;
const MAX_IMAGE_EXPORT_PARTICIPATION_KEYS: usize = 1_024;
/// A healthy local Docker export should produce or accept another chunk well
/// inside this window. It bounds a paused daemon and a client that stops reading.
const IMAGE_EXPORT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// An idle timeout alone still permits an authenticated client to trickle bytes
/// forever, so every export also has an absolute wall-clock lifetime.
const IMAGE_EXPORT_MAX_DURATION: Duration = Duration::from_secs(10 * 60);

static IMAGE_EXPORT_ADMISSION: LazyLock<ImageExportAdmission> =
    LazyLock::new(|| ImageExportAdmission::new(MAX_CONCURRENT_IMAGE_EXPORTS));

/// Bound Docker image exports globally, per participation, and per BYOC
/// capability. Permits move into the streaming task, so disconnecting or timing
/// out the HTTP response releases admission instead of leaving a detached export.
struct ImageExportAdmission {
    global: Arc<Semaphore>,
    identities: Mutex<ImageExportIdentityGates>,
}

#[derive(Default)]
struct ImageExportIdentityGates {
    participations: HashMap<i32, Weak<Semaphore>>,
    capabilities: HashMap<(i32, i32), Weak<Semaphore>>,
}

struct ImageExportPermit {
    _global: OwnedSemaphorePermit,
    _participation: OwnedSemaphorePermit,
    _capability: OwnedSemaphorePermit,
}

impl ImageExportAdmission {
    fn new(global_limit: usize) -> Self {
        Self {
            global: Arc::new(Semaphore::new(global_limit)),
            identities: Mutex::new(ImageExportIdentityGates::default()),
        }
    }

    fn try_admit(&self, participation_id: i32, challenge_id: i32) -> Option<ImageExportPermit> {
        // Take global capacity first so a capability-flood cannot populate the
        // identity map while all useful export capacity is already occupied.
        let global = self.global.clone().try_acquire_owned().ok()?;
        let (participation, capability) = {
            let mut identities = self
                .identities
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if identities.participations.len() >= MAX_IMAGE_EXPORT_PARTICIPATION_KEYS {
                identities
                    .participations
                    .retain(|_, gate| gate.strong_count() > 0);
            }
            if identities.capabilities.len() >= MAX_IMAGE_EXPORT_CAPABILITY_KEYS {
                identities
                    .capabilities
                    .retain(|_, gate| gate.strong_count() > 0);
            }

            let participation = match identities
                .participations
                .get(&participation_id)
                .and_then(Weak::upgrade)
            {
                Some(gate) => gate,
                None => {
                    if identities.participations.len() >= MAX_IMAGE_EXPORT_PARTICIPATION_KEYS {
                        return None;
                    }
                    let gate = Arc::new(Semaphore::new(1));
                    identities
                        .participations
                        .insert(participation_id, Arc::downgrade(&gate));
                    gate
                }
            };
            let capability = match identities
                .capabilities
                .get(&(participation_id, challenge_id))
                .and_then(Weak::upgrade)
            {
                Some(gate) => gate,
                None => {
                    if identities.capabilities.len() >= MAX_IMAGE_EXPORT_CAPABILITY_KEYS {
                        return None;
                    }
                    let gate = Arc::new(Semaphore::new(1));
                    identities
                        .capabilities
                        .insert((participation_id, challenge_id), Arc::downgrade(&gate));
                    gate
                }
            };
            (participation, capability)
        };
        let participation = participation.try_acquire_owned().ok()?;
        let capability = capability.try_acquire_owned().ok()?;
        Some(ImageExportPermit {
            _global: global,
            _participation: participation,
            _capability: capability,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageExportEnd {
    Complete,
    SourceError,
    ClientDisconnected,
    SourceIdleTimeout,
    ClientIdleTimeout,
    DurationLimit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageExportStartError {
    Empty,
    SourceError,
    IdleTimeout,
}

/// Poll Docker far enough to prove that the immutable archive has started. The
/// caller retains the authorization fence through this bounded wait and releases
/// it only after the first chunk exists (or startup terminates).
async fn poll_image_export_start<S>(
    mut stream: S,
    idle_timeout: Duration,
) -> Result<(bytes::Bytes, S), ImageExportStartError>
where
    S: futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Unpin,
{
    use futures::StreamExt;

    match tokio::time::timeout(idle_timeout, stream.next()).await {
        Ok(Some(Ok(first))) => Ok((first, stream)),
        Ok(Some(Err(_))) => Err(ImageExportStartError::SourceError),
        Ok(None) => Err(ImageExportStartError::Empty),
        Err(_) => Err(ImageExportStartError::IdleTimeout),
    }
}

/// Forward Docker's archive stream without allowing either side to pin an
/// export permit indefinitely. Durations are parameters so timeout behavior is
/// deterministic and fast in unit tests.
async fn forward_image_export<S>(
    mut stream: S,
    tx: tokio::sync::mpsc::Sender<Result<bytes::Bytes, std::io::Error>>,
    idle_timeout: Duration,
    max_duration: Duration,
) -> ImageExportEnd
where
    S: futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Unpin,
{
    use futures::StreamExt;

    let forward = async {
        loop {
            let item = match tokio::time::timeout(idle_timeout, stream.next()).await {
                Ok(Some(item)) => item,
                Ok(None) => return ImageExportEnd::Complete,
                Err(_) => {
                    let _ = tx.try_send(Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "Docker image export stopped producing data",
                    )));
                    return ImageExportEnd::SourceIdleTimeout;
                }
            };
            let source_error = item.is_err();
            match tokio::time::timeout(idle_timeout, tx.send(item)).await {
                Ok(Ok(())) if source_error => return ImageExportEnd::SourceError,
                Ok(Ok(())) => {}
                Ok(Err(_)) => return ImageExportEnd::ClientDisconnected,
                Err(_) => return ImageExportEnd::ClientIdleTimeout,
            }
        }
    };

    match tokio::time::timeout(max_duration, forward).await {
        Ok(end) => end,
        Err(_) => {
            let _ = tx.try_send(Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Docker image export exceeded its maximum duration",
            )));
            ImageExportEnd::DurationLimit
        }
    }
}

/// Resolve + validate a BYOC download: caller is an accepted participant, the
/// challenge is a self-hosted enabled A&D challenge in this game, and build the
/// shared connection material (tunnel/image URLs, WG config).
async fn resolve_byoc(
    st: &SharedState,
    user: &CurrentUser,
    game_id: i32,
    challenge_id: i32,
    public_origin: &str,
) -> AppResult<ByocContext> {
    let part = resolve_participation(st, user, game_id).await?;
    let mut roster_access = super::vpn::acquire_roster_access(st, user, &part).await?;
    let grant = super::byoc_authorization::load_byoc_grant_on(
        roster_access.transaction_mut(),
        game_id,
        part.id,
        part.team_id,
        challenge_id,
        false,
    )
    .await?
    .ok_or_else(|| AppError::not_found("no such self-hosted challenge in this game"))?;

    let (http_scheme, public_authority) = public_origin
        .split_once("://")
        .ok_or_else(|| AppError::internal("canonical public URL has no scheme"))?;
    let ws_scheme = if http_scheme == "https" { "wss" } else { "ws" };
    let svc_port = grant.expose_port.unwrap_or(80);

    let agent_token = byoc_token(
        "adbyocagent:",
        &grant.game_secret,
        &grant.team_secret,
        part.id,
        challenge_id,
    );
    let image_token = byoc_token(
        "adbyocimage:",
        &grant.game_secret,
        &grant.team_secret,
        part.id,
        challenge_id,
    );
    let tunnel_url = format!(
        "{ws_scheme}://{public_authority}/api/stateful/Game/{game_id}/Ad/Byoc/Agent/{}/{challenge_id}/{agent_token}",
        part.id
    );
    let image_url = format!(
        "{http_scheme}://{public_authority}/api/stateful/Game/{game_id}/Ad/Byoc/Image/{}/{challenge_id}/{image_token}",
        part.id
    );

    // A mutable tag would turn registry or publisher compromise into code
    // execution on team hosts. Overrides are therefore accepted only by digest.
    let configured_agent_image = std::env::var("RSCTF_AD_BYOC_AGENT_IMAGE").ok();
    let (configured_agent_image, agent_image_requires_amd64) = match configured_agent_image {
        Some(image) => (image, false),
        None => {
            let (image, requires_amd64) = default_byoc_agent_image().ok_or_else(|| {
                AppError::unavailable(
                    "this server build has no matching BYOC agent digest; set RSCTF_AD_BYOC_AGENT_IMAGE to the immutable digest built from this release",
                )
            })?;
            (image.to_string(), requires_amd64)
        }
    };
    let agent_image = immutable_agent_image(&configured_agent_image).ok_or_else(|| {
        AppError::internal(
            "RSCTF_AD_BYOC_AGENT_IMAGE must be an immutable OCI sha256 digest reference",
        )
    })?;

    let wg_config = super::vpn::render_wg_config_for_game(st, game_id, &user.name, part.id).await?;
    let container_image = grant.setup_runtime_image(st)?;
    roster_access.release().await?;

    Ok(ByocContext {
        title: grant.title,
        container_image,
        svc_port,
        tunnel_url,
        image_url,
        agent_image,
        agent_image_requires_amd64,
        wg_config,
    })
}

/// Resolve the one canonical public origin used in generated capability URLs.
/// A configured `RSCTF_PUBLIC_URL` always wins. The development fallback uses
/// only the actual Host header; an untrusted `X-Forwarded-Host` must never steer
/// an enrolled agent (and its secret capability) to another origin.
fn canonical_public_origin(configured: Option<&str>, headers: &HeaderMap) -> AppResult<String> {
    if let Some(configured) = configured {
        return normalize_public_origin(configured).ok_or_else(|| {
            AppError::internal("RSCTF_PUBLIC_URL is not a canonical HTTP(S) origin")
        });
    }

    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| match value.split(',').next().map(str::trim) {
            Some("http") => Some("http"),
            Some("https") => Some("https"),
            _ => None,
        })
        .unwrap_or("http");
    let authority = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("localhost");
    normalize_public_origin(&format!("{scheme}://{authority}"))
        .ok_or_else(|| AppError::bad_request("request Host is not a valid public authority"))
}

fn normalize_public_origin(value: &str) -> Option<String> {
    let uri = value.parse::<axum::http::Uri>().ok()?;
    let scheme = match uri.scheme_str()? {
        "http" => "http",
        "https" => "https",
        _ => return None,
    };
    if uri
        .path_and_query()
        .is_some_and(|path| path.as_str() != "/")
    {
        return None;
    }
    let authority = uri.authority()?;
    if authority.as_str().contains('@') || !safe_public_host(authority.host()) {
        return None;
    }
    Some(format!("{scheme}://{authority}"))
}

fn safe_public_host(host: &str) -> bool {
    let ip_literal = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    if ip_literal.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && label.as_bytes().first() != Some(&b'-')
                && label.as_bytes().last() != Some(&b'-')
        })
}

/// `GET /api/Game/{id}/Ad/Byoc/Setup/{challengeId}` — one-command installer.
///
/// If the challenge ships a service image, the script tries to pull it from the
/// (stubbed, best-effort) image endpoint and, on failure, falls back to a
/// placeholder that serves the rotating flag — so it runs either way. Writes the
/// compose (service + tunnel agent) and the bundled WireGuard config, then
/// `docker compose up -d`.
pub async fn byoc_setup(
    State(st): State<SharedState>,
    user: CurrentUser,
    headers: HeaderMap,
    Path((id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<Response> {
    let public_origin = canonical_public_origin(st.config.public_url.as_deref(), &headers)?;
    let ctx = resolve_byoc(&st, &user, id, challenge_id, &public_origin).await?;
    let script = build_setup_script(id, challenge_id, &ctx);
    let fname = format!("setup-{}.sh", slugify(&ctx.title, challenge_id));
    Ok(text_attachment("application/x-sh", &fname, script))
}

/// `GET /api/Game/{id}/Ad/Byoc/Compose/{challengeId}` — the bring-your-own
/// docker-compose (self-contained placeholder service that runs out of the box).
pub async fn byoc_compose(
    State(st): State<SharedState>,
    user: CurrentUser,
    headers: HeaderMap,
    Path((id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<Response> {
    let public_origin = canonical_public_origin(st.config.public_url.as_deref(), &headers)?;
    let ctx = resolve_byoc(&st, &user, id, challenge_id, &public_origin).await?;
    let compose = build_compose(id, challenge_id, &ctx);
    let fname = format!("docker-compose-{}.yml", slugify(&ctx.title, challenge_id));
    Ok(text_attachment("application/yaml", &fname, compose))
}

/// `GET /api/Game/{id}/Ad/Byoc/Agent/{participationId}/{challengeId}/{token}` —
/// RSCTF's WebSocket tunnel relay. rsctf has no relay service wired, so this is
/// a **best-effort stub** returning a clear 503.
/// `GET /api/Game/{id}/Ad/Byoc/Agent/{pid}/{cid}/{token}` (WebSocket) — the team's
/// agent dials this; rsctf runs the in-process yamux tunnel over it (see
/// `services::byoc_tunnel`). Verifies the deterministic agent token + that the
/// challenge is a BYOC A&D challenge in the game before upgrading.
pub async fn byoc_agent(
    State(st): State<SharedState>,
    Path((id, pid, cid, token)): Path<(i32, i32, i32, String)>,
    headers: HeaderMap,
    ws: axum::extract::ws::WebSocketUpgrade,
) -> Response {
    if !st.config.runtime_role.capabilities().network {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "BYOC tunnel route reached a non-network replica; check stateful route configuration",
        )
            .into_response();
    }
    if !byoc_agent_protocol_offered(&headers) {
        return (
            StatusCode::UPGRADE_REQUIRED,
            "BYOC agent protocol rsctf-byoc-v2 is required; update rsctf-byoc-agent before connecting",
        )
            .into_response();
    }
    let authorization = match super::byoc_authorization::authorize_byoc_capability(
        st.pg(),
        id,
        pid,
        cid,
        "adbyocagent:",
        &token,
    )
    .await
    {
        Ok(Some(authorization)) => authorization,
        Ok(None) => {
            return (StatusCode::FORBIDDEN, "invalid or revoked BYOC capability").into_response();
        }
        Err(error) => return error.into_response(),
    };
    ws.max_frame_size(1024 * 1024)
        .max_message_size(1024 * 1024)
        .on_upgrade(move |socket| async move {
            // Keep the transaction fence through Axum's upgrade hand-off, then
            // release it before serving the long-lived tunnel. A roster change
            // cannot cross admission, and a tunnel cannot pin a DB connection.
            if let Err(error) = authorization.release().await {
                tracing::warn!(pid, cid, %error, "BYOC agent authorization fence release failed");
                return;
            }
            crate::services::byoc_tunnel::serve_agent(st, id, pid, cid, token, socket).await;
        })
}

fn byoc_agent_protocol_offered(headers: &HeaderMap) -> bool {
    headers
        .get_all(crate::services::byoc_tunnel::AGENT_PROTOCOL_HEADER)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|protocol| protocol.trim() == crate::services::byoc_tunnel::AGENT_PROTOCOL)
}

/// `GET /api/Game/{id}/Ad/Byoc/Image/{participationId}/{challengeId}/{token}` —
/// streams the challenge's built SERVICE image as a `docker save` tarball, which
/// `setup.sh` pipes into `docker load` so the team runs the REAL vulnerable
/// service (not the fallback placeholder). Verifies the deterministic image token
/// + that the challenge is a BYOC A&D challenge in the game.
pub async fn byoc_image(
    State(st): State<SharedState>,
    Path((id, pid, cid, token)): Path<(i32, i32, i32, String)>,
) -> Response {
    if !st.config.runtime_role.capabilities().network {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "BYOC image route reached a non-network replica; check stateful route configuration\n",
        )
            .into_response();
    }
    // Reserve bounded export capacity before retaining a PostgreSQL
    // transaction. Requests beyond useful capacity must not consume the pool.
    let Some(export_permit) = IMAGE_EXPORT_ADMISSION.try_admit(pid, cid) else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "a BYOC image export is already active or export capacity is full\n",
        )
            .into_response();
    };
    let authorization = match super::byoc_authorization::authorize_byoc_capability(
        st.pg(),
        id,
        pid,
        cid,
        "adbyocimage:",
        &token,
    )
    .await
    {
        Ok(Some(authorization)) => authorization,
        Ok(None) => {
            return (StatusCode::FORBIDDEN, "invalid or revoked BYOC capability").into_response();
        }
        Err(error) => return error.into_response(),
    };
    let Ok(image) = authorization.grant().runtime_image(&st) else {
        let _ = authorization.release().await;
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "no immutable service image built for this challenge yet\n",
        )
            .into_response();
    };
    let title = authorization.grant().title.clone();
    let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
        drop(export_permit);
        let _ = authorization.release().await;
        return (StatusCode::INTERNAL_SERVER_ERROR, "docker unavailable\n").into_response();
    };
    // Linearize revocation with export startup, not with a client-paced body.
    // A revoker waits until Docker produces the first immutable archive chunk
    // (bounded by the source-idle timeout). After that standard HTTP semantics
    // allow this already-started response to finish, while every new request
    // observes the revoked state.
    use futures::StreamExt;
    let export_started = std::time::Instant::now();
    let stream = docker
        .export_image(&image)
        .map(|chunk| chunk.map_err(std::io::Error::other));
    let (first, stream) = match poll_image_export_start(stream, IMAGE_EXPORT_IDLE_TIMEOUT).await {
        Ok(started) => started,
        Err(start_error) => {
            tracing::warn!(pid, cid, ?start_error, "BYOC image export failed to start");
            drop(export_permit);
            let _ = authorization.release().await;
            let (status, message) = match start_error {
                ImageExportStartError::IdleTimeout => (
                    StatusCode::GATEWAY_TIMEOUT,
                    "docker image export did not start before its timeout\n",
                ),
                ImageExportStartError::Empty | ImageExportStartError::SourceError => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "docker image export could not be started\n",
                ),
            };
            return (status, message).into_response();
        }
    };
    if let Err(error) = authorization.release().await {
        drop(export_permit);
        return error.into_response();
    }
    let remaining_duration = IMAGE_EXPORT_MAX_DURATION.saturating_sub(export_started.elapsed());
    let stream = futures::stream::iter([Ok::<bytes::Bytes, std::io::Error>(first)]).chain(stream);
    // Stream `docker save <image>` (GET /images/{name}/get). A dedicated task owns
    // `docker` + `image` + its admission permits so the response byte stream is
    // `'static`. Disconnects, 30 seconds without progress, or the ten-minute
    // wall-clock limit stop the task and release every export slot.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(8);
    tokio::spawn(async move {
        let _export_permit = export_permit;
        let end =
            forward_image_export(stream, tx, IMAGE_EXPORT_IDLE_TIMEOUT, remaining_duration).await;
        if !matches!(
            end,
            ImageExportEnd::Complete | ImageExportEnd::ClientDisconnected
        ) {
            tracing::warn!(pid, cid, ?end, "BYOC image export ended early");
        }
    });
    let body = axum::body::Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
    (
        [
            (header::CONTENT_TYPE, "application/x-tar".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}.tar\"", slugify(&title, cid)),
            ),
            (header::CACHE_CONTROL, BYOC_SECRET_CACHE_CONTROL.to_string()),
        ],
        body,
    )
        .into_response()
}

/// Build a `text/plain` attachment response with the given content-type override
/// and download filename.
fn text_attachment(content_type: &str, filename: &str, body: String) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
            (header::CACHE_CONTROL, BYOC_SECRET_CACHE_CONTROL.to_string()),
        ],
        body.into_bytes(),
    )
        .into_response()
}

/// Escape a title for safe single-line embedding in generated shell/YAML.
fn safe_title(title: &str) -> String {
    title
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn compose_scalar(value: &str) -> String {
    // A JSON string is also a valid YAML scalar and gives us unambiguous escapes
    // for quotes, backslashes, CR/LF, and every other control character. Docker
    // Compose expands `$` even in quoted values, so double it before encoding.
    serde_json::Value::String(value.replace('$', "$$")).to_string()
}

fn encoded_file_command(path: &str, contents: &[u8]) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(contents);
    format!(
        "printf '%s' {} | base64 -d > {} && chmod 600 {}",
        shell_single_quote(&encoded),
        shell_single_quote(path),
        shell_single_quote(path)
    )
}

/// A daemon-local alias for the exact archive loaded by `setup.sh`. A Docker
/// archive saved by digest has no usable `RepoTags`, so this local name avoids a
/// registry lookup and keeps reviewed revisions separate.
fn reviewed_service_image_name(game_id: i32, challenge_id: i32, image: &str) -> String {
    let identity = Sha256::digest(image.as_bytes());
    format!(
        "rsctf-byoc-{game_id}-{challenge_id}-service:reviewed-{}",
        hex::encode(&identity[..8])
    )
}

/// One-command installer: best-effort pull the real image (falling back to a
/// placeholder), write the compose + bundled WireGuard config, and start.
fn build_setup_script(game_id: i32, challenge_id: i32, ctx: &ByocContext) -> String {
    let title = safe_title(&ctx.title);
    let dir = format!("rsctf-byoc-{game_id}-{challenge_id}");
    let mut lines: Vec<String> = vec![
        "#!/bin/sh".into(),
        format!("# rsctf Attack & Defense — self-hosted setup for \"{title}\""),
        "# Run it:  sh setup.sh        (needs docker + docker compose)".into(),
        "set -e".into(),
        // The compose document carries the tunnel capability and the WireGuard
        // file carries a private key. Do not inherit a permissive host umask.
        "umask 077".into(),
    ];
    if ctx.agent_image_requires_amd64 {
        lines.extend([
            "case \"$(uname -m)\" in".into(),
            "  x86_64|amd64) ;;".into(),
            "  *) echo 'The built-in rsctf BYOC agent currently supports Linux amd64 only. Ask the organizer to configure RSCTF_AD_BYOC_AGENT_IMAGE with an immutable multi-architecture digest.' >&2; exit 1 ;;".into(),
            "esac".into(),
        ]);
    }
    lines.extend([
        format!("DIR={}", shell_single_quote(&dir)),
        "if [ -L \"$DIR\" ]; then echo 'Refusing a symlinked BYOC setup directory.' >&2; exit 1; fi".into(),
        "if [ -e \"$DIR\" ] && [ ! -d \"$DIR\" ]; then echo 'The BYOC setup path exists but is not a directory.' >&2; exit 1; fi".into(),
        "mkdir -p \"$DIR\"".into(),
        "if [ \"$(stat -c '%u' \"$DIR\" 2>/dev/null)\" != \"$(id -u)\" ]; then echo 'The BYOC setup directory belongs to another user.' >&2; exit 1; fi".into(),
        "chmod 700 \"$DIR\"".into(),
        "cd \"$DIR\"".into(),
        // Remove only the two files this installer owns. This safely unlinks a
        // pre-planted output symlink and ensures a rerun cannot retain an old
        // world-readable mode despite the restrictive umask.
        "rm -f docker-compose.yml rsctf-ad.conf".into(),
        "".into(),
        "SERVICE_IMAGE=\"\"".into(),
        "echo '[1/4] Fetching the real service image from the game server (best-effort)...'".into(),
    ]);

    // The image pull is non-fatal: on any failure (image not built yet) we fall
    // back to a placeholder so the script still runs. Byoc/Image now streams the
    // real docker-save tarball, so the pull normally succeeds.
    if let Some(image) = ctx.container_image.as_deref().filter(|s| !s.is_empty()) {
        let reviewed_image = reviewed_service_image_name(game_id, challenge_id, image);
        lines.push(format!(
            "REVIEWED_IMAGE={}",
            shell_single_quote(&reviewed_image)
        ));
        lines.push(format!(
            "if LOAD_OUTPUT=$(curl -fSL {} 2>/dev/null | docker load 2>/dev/null); then",
            shell_single_quote(&ctx.image_url)
        ));
        // `docker load` reports either `Loaded image: <ref>` or `Loaded image
        // ID: sha256:...`. Resolve that daemon-owned result to its content ID,
        // then create the only name the generated Compose file will accept.
        // Every expansion remains quoted: even a hostile daemon response cannot
        // become shell syntax.
        lines.push("  LOADED_REF=$(printf '%s\\n' \"$LOAD_OUTPUT\" | sed -n -e 's/^Loaded image ID: //p' -e 's/^Loaded image: //p' | tail -n 1)".into());
        lines.push("  if [ -n \"$LOADED_REF\" ] && LOADED_ID=$(docker image inspect --format '{{.Id}}' \"$LOADED_REF\" 2>/dev/null) && [ -n \"$LOADED_ID\" ] && docker image tag \"$LOADED_ID\" \"$REVIEWED_IMAGE\" 2>/dev/null; then".into());
        lines.push("    SERVICE_IMAGE=\"$REVIEWED_IMAGE\"".into());
        lines.push("    echo \"  loaded and pinned $SERVICE_IMAGE\"".into());
        lines.push("  else".into());
        lines.push("    echo '  the archive loaded but its immutable local identity could not be verified — using the pinned placeholder.'".into());
        lines.push("  fi".into());
        lines.push("else".into());
        lines.push(
            "  echo '  image pull unavailable on this server — using a placeholder that serves the rotating flag.'"
                .into(),
        );
        lines.push("fi".into());
    } else {
        lines.push(
            "echo '  this challenge ships no image — using a placeholder that serves the rotating flag.'"
                .into(),
        );
    }

    // Select between two fully-rendered, encoded compose documents. Keeping
    // server-provided values out of an executable heredoc prevents a newline,
    // quote, `$()`, or heredoc-marker payload from becoming shell syntax.
    let fallback_compose = build_setup_compose(game_id, challenge_id, ctx, None);
    let real_compose = ctx
        .container_image
        .as_deref()
        .filter(|image| !image.is_empty())
        .map(|image| {
            let reviewed_image = reviewed_service_image_name(game_id, challenge_id, image);
            build_setup_compose(game_id, challenge_id, ctx, Some(&reviewed_image))
        });
    lines.push("".into());
    lines.push("echo '[2/4] Writing docker-compose.yml...'".into());
    lines.push("if [ -n \"$SERVICE_IMAGE\" ]; then".into());
    if let Some(real_compose) = real_compose {
        lines.push(format!(
            "  {}",
            encoded_file_command("docker-compose.yml", real_compose.as_bytes())
        ));
    } else {
        lines.push("  echo 'internal error: service image selection is inconsistent' >&2".into());
        lines.push("  exit 1".into());
    }
    lines.push("else".into());
    lines.push(format!(
        "  {}",
        encoded_file_command("docker-compose.yml", fallback_compose.as_bytes())
    ));
    lines.push("fi".into());

    // Bundle the deterministic WireGuard config as an L3 connectivity fallback.
    // Encode the complete payload rather than selecting a heredoc delimiter:
    // user-controlled display text can contain any fixed delimiter on its own
    // line, while base64 has no shell metacharacters or line breaks here.
    lines.push("".into());
    lines.push("echo '[3/4] Writing bundled WireGuard config (rsctf-ad.conf)...'".into());
    lines.push(encoded_file_command(
        "rsctf-ad.conf",
        ctx.wg_config.as_bytes(),
    ));
    lines.push(
        "echo '  if the tunnel agent cannot connect, bring the VPN up:  wg-quick up ./rsctf-ad.conf'"
            .into(),
    );

    lines.push("".into());
    lines.push("echo '[4/4] Starting...'".into());
    // Fix the file, empty interpolation environment, and project name so planted
    // Compose configuration cannot steer startup.
    lines.push(format!(
        "COMPOSE_PROJECT_NAME={} docker compose --env-file /dev/null -f docker-compose.yml up -d",
        shell_single_quote(&dir)
    ));
    lines.push(
        "echo 'Done — watch the platform; your status should go green within a tick.'".into(),
    );
    lines.push("".into());
    lines.join("\n")
}

fn build_setup_compose(
    game_id: i32,
    challenge_id: i32,
    ctx: &ByocContext,
    service_image: Option<&str>,
) -> String {
    let port = ctx.svc_port;
    let mut lines = vec![
        format!("name: rsctf-byoc-{game_id}-{challenge_id}"),
        "services:".into(),
        "  # The vulnerable service. Patch it to defend; it reads its".into(),
        "  # rotating flag from RSCTF_FLAG_FILE (we deliver it to /shared/flag).".into(),
        "  service:".into(),
    ];
    if let Some(image) = service_image {
        lines.push(format!("    image: {}", compose_scalar(image)));
        // The setup script has just tagged Docker load's returned content ID with
        // this local-only name. Never let Compose replace it from a registry.
        lines.push("    pull_policy: never".into());
    } else {
        lines.push(format!("    image: {DEFAULT_BYOC_FALLBACK_IMAGE}"));
        lines.push(format!(
            "    command: [\"TCP-LISTEN:{port},fork,reuseaddr\", \"SYSTEM:cat /shared/flag 2>/dev/null\"]"
        ));
    }
    lines.extend([
        format!("    container_name: rsctf-byoc-{game_id}-{challenge_id}-service"),
        "    restart: unless-stopped".into(),
        "    environment:".into(),
        "      RSCTF_FLAG_FILE: /shared/flag".into(),
        "    volumes:".into(),
        "      - flag:/shared:ro".into(),
        "  # The tunnel agent — public image, token baked in. Don't edit.".into(),
        "  rsctf-agent:".into(),
        format!("    image: {}", compose_scalar(&ctx.agent_image)),
        "    restart: unless-stopped".into(),
        "    environment:".into(),
        "      RSCTF_BYOC_MODE: agent".into(),
        format!(
            "      RSCTF_BYOC_TUNNEL_URL: {}",
            compose_scalar(&ctx.tunnel_url)
        ),
        format!("      RSCTF_BYOC_SERVICE: 'service:{port}'"),
        "      RSCTF_BYOC_FLAG_FILE: /shared/flag".into(),
        format!(
            "      RSCTF_BYOC_SERVICE_CONTAINER: 'rsctf-byoc-{game_id}-{challenge_id}-service'"
        ),
        "    volumes:".into(),
        "      - flag:/shared".into(),
        "      # Optional BYOC SSH/admin shell (disabled by default): uncommenting".into(),
        "      # the next line grants this agent root-equivalent Docker access on".into(),
        "      # THIS host. Service and flag streams work without the socket.".into(),
        "      # - /var/run/docker.sock:/var/run/docker.sock".into(),
        "    depends_on:".into(),
        "      - service".into(),
        "volumes:".into(),
        "  flag:".into(),
        "".into(),
    ]);
    lines.join("\n")
}

/// The bring-your-own-service compose (placeholder service that works out of the
/// box). Mirrors RSCTF's `BuildByocCompose`.
fn build_compose(game_id: i32, challenge_id: i32, ctx: &ByocContext) -> String {
    let title = safe_title(&ctx.title);
    let port = ctx.svc_port;
    let lines: Vec<String> = vec![
        format!("# rsctf Attack & Defense — self-hosted service for \"{title}\""),
        "# Unique per challenge, so you can run several Bring Your Own Container (BYOC) challenges side by side.".into(),
        format!("name: rsctf-byoc-{game_id}-{challenge_id}"),
        "#".into(),
        "#   docker compose up -d        # that's it — works out of the box.".into(),
        "#".into(),
        "# This runs immediately: the rsctf agent makes one outbound connection to".into(),
        "# the game (no public IP / inbound firewall / VPN), and the placeholder".into(),
        "# 'service' serves the rotating flag so your status goes GREEN right away.".into(),
        "# Then replace the 'service' block with your real vulnerable service — it".into(),
        format!("# only has to listen on port {port} and read its flag from /shared/flag."),
        "services:".into(),
        "  # ───────────────────────────────────────────────────────────────────".into(),
        "  # >>> REPLACE THIS with your service (build: ./yourdir  OR  image: you/img).".into(),
        "  #     Keep the flag volume; your service must listen on the port below.".into(),
        "  # The default just serves /shared/flag so the connection works on day one.".into(),
        "  service:".into(),
        format!("    image: {DEFAULT_BYOC_FALLBACK_IMAGE}"),
        format!("    command: [\"TCP-LISTEN:{port},fork,reuseaddr\", \"SYSTEM:cat /shared/flag 2>/dev/null\"]"),
        "    restart: unless-stopped".into(),
        "    volumes:".into(),
        "      - flag:/shared:ro        # rotating flag at /shared/flag (read-only to you)".into(),
        "".into(),
        "  # The tunnel agent — public image, token baked in. Don't edit this.".into(),
        "  rsctf-agent:".into(),
        format!("    image: {}", compose_scalar(&ctx.agent_image)),
        "    restart: unless-stopped".into(),
        "    environment:".into(),
        "      RSCTF_BYOC_MODE: agent".into(),
        format!(
            "      RSCTF_BYOC_TUNNEL_URL: {}",
            compose_scalar(&ctx.tunnel_url)
        ),
        format!("      RSCTF_BYOC_SERVICE: \"service:{port}\""),
        "      RSCTF_BYOC_FLAG_FILE: /shared/flag".into(),
        "    volumes:".into(),
        "      - flag:/shared".into(),
        "    depends_on:".into(),
        "      - service".into(),
        "".into(),
        "volumes:".into(),
        "  flag:".into(),
        "".into(),
    ];
    lines.join("\n")
}

#[cfg(test)]
#[path = "byoc_tests.rs"]
mod tests;
