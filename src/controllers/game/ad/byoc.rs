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
//!     service (`alpine/socat` serving the rotating flag) so the script still
//!     runs — but the placeholder won't pass a functional checker.
//!   * The generated bundle also embeds the deterministic WireGuard config
//!     (reusing `vpn.rs::render_wg_config`) as an L3 connectivity fallback.
//!
//! BYOC tokens (`adbyocagent:` / `adbyocimage:`) are derived from the game secret,
//! a rotatable team secret, and `(participation, challenge)`. Every handshake also
//! revalidates the participation and challenge state against the database.

use axum::http::header;
use axum::response::{IntoResponse, Response};

use super::*;

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
    wg_config: String,
}

/// Resolve + validate a BYOC download: caller is an accepted participant, the
/// challenge is a self-hosted enabled A&D challenge in this game, and build the
/// shared connection material (tunnel/image URLs, WG config).
async fn resolve_byoc(
    st: &SharedState,
    user: &CurrentUser,
    game_id: i32,
    challenge_id: i32,
    http_scheme: &str,
    host: &str,
) -> AppResult<ByocContext> {
    let part = resolve_participation(st, user, game_id).await?;
    let roster_access = super::vpn::acquire_roster_access(st, user, &part).await?;

    let game = game::Entity::find_by_id(game_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Game not found"))?;

    let team = team::Entity::find_by_id(part.team_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Team not found"))?;

    let chal = game_challenge::Entity::find()
        .filter(game_challenge::Column::Id.eq(challenge_id))
        .filter(game_challenge::Column::GameId.eq(game_id))
        .filter(game_challenge::Column::ChallengeType.eq(ChallengeType::AttackDefense))
        .filter(game_challenge::Column::AdSelfHosted.eq(true))
        .filter(game_challenge::Column::IsEnabled.eq(true))
        .filter(
            game_challenge::Column::ReviewStatus
                .eq(crate::utils::enums::ChallengeReviewStatus::Active),
        )
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("no such self-hosted challenge in this game"))?;

    let ws_scheme = if http_scheme == "https" { "wss" } else { "ws" };
    let svc_port = chal.expose_port.unwrap_or(80);

    let agent_token = byoc_token(
        "adbyocagent:",
        &game.private_key,
        &team.invite_token,
        part.id,
        challenge_id,
    );
    let image_token = byoc_token(
        "adbyocimage:",
        &game.private_key,
        &team.invite_token,
        part.id,
        challenge_id,
    );
    let tunnel_url = format!(
        "{ws_scheme}://{host}/api/stateful/Game/{game_id}/Ad/Byoc/Agent/{}/{challenge_id}/{agent_token}",
        part.id
    );
    let image_url = format!(
        "{http_scheme}://{host}/api/stateful/Game/{game_id}/Ad/Byoc/Image/{}/{challenge_id}/{image_token}",
        part.id
    );

    // Public relay image (Docker Hub) — overridable by operator env.
    let agent_image = std::env::var("RSCTF_AD_BYOC_AGENT_IMAGE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "dimasmaualana/rsctf-byoc-agent:latest".to_string());

    let wg_config = super::vpn::render_wg_config(st, &game, &user.name, part.id).await?;
    roster_access.release().await?;

    let container_image = if chal
        .container_image
        .as_deref()
        .is_some_and(|image| !image.trim().is_empty())
    {
        Some(crate::services::challenge_images::runtime_image(st, &chal)?)
    } else {
        None
    };

    Ok(ByocContext {
        title: chal.title,
        container_image,
        svc_port,
        tunnel_url,
        image_url,
        agent_image,
        wg_config,
    })
}

/// Derive `(http_scheme, host)` from the request headers, honoring a reverse
/// proxy's `X-Forwarded-*`. Defaults to `http` + the operator-configured public
/// host (or `localhost`) when nothing is present.
fn scheme_and_host(headers: &HeaderMap) -> (String, String) {
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').next().unwrap_or(s).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "http".to_string());
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(header::HOST))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var("RSCTF_PUBLIC_HOST")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "localhost".to_string());
    (scheme, host)
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
    let (scheme, host) = scheme_and_host(&headers);
    let ctx = resolve_byoc(&st, &user, id, challenge_id, &scheme, &host).await?;
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
    let (scheme, host) = scheme_and_host(&headers);
    let ctx = resolve_byoc(&st, &user, id, challenge_id, &scheme, &host).await?;
    let compose = build_compose(id, challenge_id, &ctx);
    let fname = format!("docker-compose-{}.yml", slugify(&ctx.title, challenge_id));
    Ok(text_attachment("application/yaml", &fname, compose))
}

/// Validate a BYOC capability against live ownership and publication state. A
/// deterministic token alone is insufficient because participation and challenge
/// authorization can be revoked after a bundle has been downloaded.
async fn authorize_byoc_capability(
    st: &SharedState,
    game_id: i32,
    participation_id: i32,
    challenge_id: i32,
    domain: &str,
    token: &str,
) -> Option<game_challenge::Model> {
    let game = game::Entity::find_by_id(game_id)
        .one(&st.db)
        .await
        .ok()
        .flatten()?;
    if !game.is_active(Utc::now()) {
        return None;
    }
    let part = participation::Entity::find()
        .filter(participation::Column::Id.eq(participation_id))
        .filter(participation::Column::GameId.eq(game_id))
        .filter(participation::Column::Status.eq(ParticipationStatus::Accepted))
        .one(&st.db)
        .await
        .ok()
        .flatten()?;
    let team = team::Entity::find_by_id(part.team_id)
        .one(&st.db)
        .await
        .ok()
        .flatten()?;
    let challenge = game_challenge::Entity::find()
        .filter(game_challenge::Column::Id.eq(challenge_id))
        .filter(game_challenge::Column::GameId.eq(game_id))
        .filter(game_challenge::Column::ChallengeType.eq(ChallengeType::AttackDefense))
        .filter(game_challenge::Column::AdSelfHosted.eq(true))
        .filter(game_challenge::Column::IsEnabled.eq(true))
        .filter(
            game_challenge::Column::ReviewStatus
                .eq(crate::utils::enums::ChallengeReviewStatus::Active),
        )
        .one(&st.db)
        .await
        .ok()
        .flatten()?;

    let expected = byoc_token(
        domain,
        &game.private_key,
        &team.invite_token,
        participation_id,
        challenge_id,
    );
    crate::utils::crypto_utils::ct_eq(&expected, token).then_some(challenge)
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
    ws: axum::extract::ws::WebSocketUpgrade,
) -> Response {
    if !st.config.runtime_role.capabilities().network {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "BYOC tunnel route reached a non-network replica; check stateful route configuration",
        )
            .into_response();
    }
    if authorize_byoc_capability(&st, id, pid, cid, "adbyocagent:", &token)
        .await
        .is_none()
    {
        return (StatusCode::FORBIDDEN, "invalid or revoked BYOC capability").into_response();
    }
    ws.max_frame_size(1024 * 1024)
        .max_message_size(1024 * 1024)
        .on_upgrade(move |socket| {
            crate::services::byoc_tunnel::serve_agent(st, id, pid, cid, token, socket)
        })
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
    let Some(chal) = authorize_byoc_capability(&st, id, pid, cid, "adbyocimage:", &token).await
    else {
        return (StatusCode::FORBIDDEN, "invalid or revoked BYOC capability").into_response();
    };
    let Ok(image) = crate::services::challenge_images::runtime_image(&st, &chal) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "no immutable service image built for this challenge yet\n",
        )
            .into_response();
    };
    let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "docker unavailable\n").into_response();
    };
    // Stream `docker save <image>` (GET /images/{name}/get). A dedicated task owns
    // `docker` + `image` so the response byte stream is `'static`; a slow/aborted
    // client just closes the channel and the task stops.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(8);
    tokio::spawn(async move {
        use futures::StreamExt;
        let mut stream = docker.export_image(&image);
        while let Some(chunk) = stream.next().await {
            if tx.send(chunk.map_err(std::io::Error::other)).await.is_err() {
                break;
            }
        }
    });
    let body = axum::body::Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
    (
        [
            (header::CONTENT_TYPE, "application/x-tar".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}.tar\"", slugify(&chal.title, cid)),
            ),
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
        ],
        body.into_bytes(),
    )
        .into_response()
}

/// Escape a title for safe single-line embedding in generated shell/YAML.
fn safe_title(title: &str) -> String {
    title.replace(['\n', '\r'], " ").replace('\'', "")
}

/// One-command installer: best-effort pull the real image (falling back to a
/// placeholder), write the compose + bundled WireGuard config, and start.
fn build_setup_script(game_id: i32, challenge_id: i32, ctx: &ByocContext) -> String {
    let title = safe_title(&ctx.title);
    let port = ctx.svc_port;
    let dir = format!("rsctf-byoc-{game_id}-{challenge_id}");
    let mut lines: Vec<String> = vec![
        "#!/bin/sh".into(),
        format!("# rsctf Attack & Defense — self-hosted setup for \"{title}\""),
        "# Run it:  sh setup.sh        (needs docker + docker compose)".into(),
        "set -e".into(),
        format!("DIR=\"{dir}\""),
        "mkdir -p \"$DIR\" && cd \"$DIR\"".into(),
        "".into(),
        "SERVICE_IMAGE=\"\"".into(),
        "echo '[1/4] Fetching the real service image from the game server (best-effort)...'".into(),
    ];

    // The image pull is non-fatal: on any failure (image not built yet) we fall
    // back to a placeholder so the script still runs. Byoc/Image now streams the
    // real docker-save tarball, so the pull normally succeeds.
    if let Some(image) = ctx.container_image.as_deref().filter(|s| !s.is_empty()) {
        let image = safe_title(image);
        lines.push(format!(
            "if curl -fSL \"{}\" 2>/dev/null | docker load 2>/dev/null; then",
            ctx.image_url
        ));
        lines.push(format!("  SERVICE_IMAGE=\"{image}\""));
        lines.push("  echo \"  loaded $SERVICE_IMAGE\"".into());
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

    // Build the compose 'service' block from whether the real image loaded.
    lines.push("".into());
    lines.push("echo '[2/4] Writing docker-compose.yml...'".into());
    lines.push("if [ -n \"$SERVICE_IMAGE\" ]; then".into());
    lines.push("  SERVICE_BLOCK=\"    image: $SERVICE_IMAGE\"".into());
    lines.push("else".into());
    lines.push(format!(
        "  SERVICE_BLOCK=\"    image: alpine/socat\n    command: [\\\"TCP-LISTEN:{port},fork,reuseaddr\\\", \\\"SYSTEM:cat /shared/flag 2>/dev/null\\\"]\""
    ));
    lines.push("fi".into());
    lines.push("".into());
    lines.push("cat > docker-compose.yml <<COMPOSE".into());
    lines.push(format!("name: rsctf-byoc-{game_id}-{challenge_id}"));
    lines.push("services:".into());
    lines.push("  # The vulnerable service. Patch it to defend; it reads its".into());
    lines.push("  # rotating flag from RSCTF_FLAG_FILE (we deliver it to /shared/flag).".into());
    lines.push("  service:".into());
    lines.push("$SERVICE_BLOCK".into());
    lines.push(format!(
        "    container_name: rsctf-byoc-{game_id}-{challenge_id}-service"
    ));
    lines.push("    restart: unless-stopped".into());
    lines.push("    environment:".into());
    lines.push("      RSCTF_FLAG_FILE: /shared/flag".into());
    lines.push("    volumes:".into());
    lines.push("      - flag:/shared:ro".into());
    lines.push("  # The tunnel agent — public image, token baked in. Don't edit.".into());
    lines.push("  rsctf-agent:".into());
    lines.push(format!("    image: {}", ctx.agent_image));
    lines.push("    restart: unless-stopped".into());
    lines.push("    environment:".into());
    lines.push("      RSCTF_BYOC_MODE: agent".into());
    lines.push(format!(
        "      RSCTF_BYOC_TUNNEL_URL: \"{}\"",
        ctx.tunnel_url
    ));
    lines.push(format!("      RSCTF_BYOC_SERVICE: \"service:{port}\""));
    lines.push("      RSCTF_BYOC_FLAG_FILE: /shared/flag".into());
    lines.push(format!(
        "      RSCTF_BYOC_SERVICE_CONTAINER: rsctf-byoc-{game_id}-{challenge_id}-service"
    ));
    lines.push("    volumes:".into());
    lines.push("      - flag:/shared".into());
    lines.push("      # Enables SSH / the admin console to open a shell in your service".into());
    lines.push("      # container over the tunnel (docker exec). This grants the agent".into());
    lines.push("      # Docker access on THIS host — it is your box + your container, but".into());
    lines.push("      # opt-out by removing the next line (service + flags still work).".into());
    lines.push("      - /var/run/docker.sock:/var/run/docker.sock".into());
    lines.push("    depends_on:".into());
    lines.push("      - service".into());
    lines.push("volumes:".into());
    lines.push("  flag:".into());
    lines.push("COMPOSE".into());

    // Bundle the deterministic WireGuard config as an L3 connectivity fallback
    // (the tunnel relay is a stub on this deployment). A single-quoted heredoc so
    // the key material is emitted verbatim.
    lines.push("".into());
    lines.push("echo '[3/4] Writing bundled WireGuard config (rsctf-ad.conf)...'".into());
    lines.push("cat > rsctf-ad.conf <<'WGCONF'".into());
    for l in ctx.wg_config.lines() {
        lines.push(l.to_string());
    }
    lines.push("WGCONF".into());
    lines.push(
        "echo '  if the tunnel agent cannot connect, bring the VPN up:  wg-quick up ./rsctf-ad.conf'"
            .into(),
    );

    lines.push("".into());
    lines.push("echo '[4/4] Starting...'".into());
    lines.push("docker compose up -d".into());
    lines.push(
        "echo 'Done — watch the platform; your status should go green within a tick.'".into(),
    );
    lines.push("".into());
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
        "    image: alpine/socat".into(),
        format!("    command: [\"TCP-LISTEN:{port},fork,reuseaddr\", \"SYSTEM:cat /shared/flag 2>/dev/null\"]"),
        "    restart: unless-stopped".into(),
        "    volumes:".into(),
        "      - flag:/shared:ro        # rotating flag at /shared/flag (read-only to you)".into(),
        "".into(),
        "  # The tunnel agent — public image, token baked in. Don't edit this.".into(),
        "  rsctf-agent:".into(),
        format!("    image: {}", ctx.agent_image),
        "    restart: unless-stopped".into(),
        "    environment:".into(),
        "      RSCTF_BYOC_MODE: agent".into(),
        format!("      RSCTF_BYOC_TUNNEL_URL: \"{}\"", ctx.tunnel_url),
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
mod tests {
    use super::byoc_token;

    #[test]
    fn team_secret_rotation_revokes_byoc_tokens() {
        let before = byoc_token("adbyocagent:", "game", "team-a", 7, 11);
        let after = byoc_token("adbyocagent:", "game", "team-b", 7, 11);

        assert_ne!(before, after);
        assert_eq!(before, byoc_token("adbyocagent:", "game", "team-a", 7, 11));
        assert_ne!(before, byoc_token("adbyocimage:", "game", "team-a", 7, 11));
    }
}
