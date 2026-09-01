//! Safe generation of the downloadable BYOC setup script.

use super::*;

pub(super) fn safe_title(title: &str) -> String {
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

pub(super) fn compose_scalar(value: &str) -> String {
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
pub(super) fn reviewed_service_image_name(game_id: i32, challenge_id: i32, image: &str) -> String {
    let identity = Sha256::digest(image.as_bytes());
    format!(
        "rsctf-byoc-{game_id}-{challenge_id}-service:reviewed-{}",
        hex::encode(&identity[..8])
    )
}

/// One-command installer: best-effort pull the real image (falling back to a
/// placeholder), write the compose + bundled WireGuard config, and start.
pub(super) fn build_setup_script(game_id: i32, challenge_id: i32, ctx: &ByocContext) -> String {
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
