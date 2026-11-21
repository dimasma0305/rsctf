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

fn decode_contents(contents: &[u8]) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(contents);
    format!("printf '%s' '{encoded}' | base64 -d")
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
        format!("# Run it: sh setup-{game_id}-{challenge_id}.sh (needs Docker and Docker Compose)"),
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
        r#"if owner=$(stat -c '%u' "$DIR" 2>/dev/null) || owner=$(stat -f '%u' "$DIR" 2>/dev/null); then :; else echo 'Could not verify the BYOC setup directory owner.' >&2; exit 1; fi"#.into(),
        r#"if [ "$owner" != "$(id -u)" ]; then echo 'The BYOC setup directory belongs to another user.' >&2; exit 1; fi"#.into(),
        "chmod 700 \"$DIR\"".into(),
        "cd \"$DIR\"".into(),
        // Both Toolkit downloads use the same Compose project. Compose would
        // adopt and replace a team's standalone service even across directories.
        "keep_compose() { echo 'Kept your existing BYOC setup. Run docker compose up -d in your original challenge folder.' >&2; exit 1; }".into(),
        "for file in compose.yaml compose.yml docker-compose.yaml; do if [ -e \"$file\" ] || [ -L \"$file\" ]; then keep_compose; fi; done".into(),
        "project_services=$(docker ps --all --filter \"label=com.docker.compose.project=$DIR\" --filter 'label=com.docker.compose.service=service' --format '{{.Names}}')".into(),
        "case \"$project_services\" in ''|\"$DIR-service\") ;; *) keep_compose ;; esac".into(),
        r#"if [ -n "$project_services" ]; then
  project_dir=$(docker container inspect --format '{{index .Config.Labels "com.docker.compose.project.working_dir"}}' "$DIR-service")
  [ "$project_dir" = "$(pwd -P)" ] && [ -f docker-compose.yml ] && [ ! -L docker-compose.yml ] || keep_compose
fi"#.into(),
        "if [ -L service-src ]; then echo 'Refusing a symlinked challenge source directory.' >&2; exit 1; fi".into(),
        "if [ ! -d service-src ]; then mkdir -m 755 service-src; fi".into(),
        "".into(),
        "SERVICE_IMAGE=\"\"".into(),
        "SOURCE_LOCATION=\"./service-src on this machine\"".into(),
        "echo '[1/4] Fetching the real service image from the game server (best-effort)...'".into(),
    ]);

    // The image pull is non-fatal: on any failure (image not built yet) we fall
    // back to a placeholder so the script still runs. Byoc/Image now streams the
    // real docker-save tarball, so the pull normally succeeds.
    let fallback_compose = build_setup_compose(game_id, challenge_id, ctx, None);
    lines.push(format!(
        "write_placeholder() {{ {}; }}",
        decode_contents(fallback_compose.as_bytes())
    ));
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
        lines.push("    echo '  Could not identify the loaded service image.'".into());
        lines.push("  fi".into());
        lines.push("else".into());
        lines.push("  echo '  The reviewed service image could not be downloaded.'".into());
        lines.push("fi".into());
        lines.push("if [ -z \"$SERVICE_IMAGE\" ]; then".into());
        // An existing Compose file may contain team edits. A transient export
        // failure must never authorize replacing it or the current container.
        lines.push(format!("  if [ -e docker-compose.yml ] || docker container inspect 'rsctf-byoc-{game_id}-{challenge_id}-service' >/dev/null 2>&1; then"));
        lines.push("    echo 'Kept your existing BYOC setup. Retry with a fresh setup script when the reviewed image is available.' >&2; exit 1".into());
        lines.push("  fi".into());
        lines.push(format!("  if CACHED_ID=$(docker image inspect --format '{{{{.Id}}}}' {} 2>/dev/null) && [ -n \"$CACHED_ID\" ] && docker image tag \"$CACHED_ID\" \"$REVIEWED_IMAGE\"; then", shell_single_quote(image)));
        lines.push(
            "    SERVICE_IMAGE=\"$REVIEWED_IMAGE\"; echo '  Using the locally cached revision.'"
                .into(),
        );
        lines
            .push("  else echo '  Starting the placeholder for this new installation.'; fi".into());
        lines.push("fi".into());
    } else {
        // The organizer can withdraw an image while a team still has a patched
        // service, including one stopped with compose down. Only refresh an
        // existing placeholder when its team-owned settings still match.
        lines.extend([
            "keep_service() { echo 'Kept your existing BYOC setup. No reviewed service image is available. Your team can continue using its current Compose file.' >&2; exit 1; }".into(),
            format!("if current_image=$(docker container inspect --format '{{{{.Config.Image}}}}' 'rsctf-byoc-{game_id}-{challenge_id}-service' 2>/dev/null); then"),
            format!("  [ \"$current_image\" = '{}' ] || keep_service", DEFAULT_BYOC_FALLBACK_IMAGE),
            "fi".into(),
            "[ ! -L docker-compose.yml ] || keep_service".into(),
        ]);
        lines.push(
            "echo '  this challenge ships no image — using a placeholder that serves the rotating flag.'"
                .into(),
        );
    }

    // Select between rendered, encoded Compose documents. Keeping
    // server-provided values out of an executable heredoc prevents a newline,
    // quote, `$()`, or heredoc-marker payload from becoming shell syntax.
    let real_compose = ctx
        .container_image
        .as_deref()
        .filter(|image| !image.is_empty())
        .map(|image| {
            let reviewed_image = reviewed_service_image_name(game_id, challenge_id, image);
            let digest = Sha256::digest(image.as_bytes());
            let volume = format!("source-{}", hex::encode(&digest[..8]));
            let shared = super::compose::setup_compose(
                game_id,
                challenge_id,
                ctx,
                Some(&reviewed_image),
                Some(&volume),
            );
            (
                build_setup_compose(game_id, challenge_id, ctx, Some(&reviewed_image)),
                shared,
                volume,
            )
        });
    lines.push("".into());
    lines.push("echo '[2/4] Writing docker-compose.yml...'".into());
    lines.push(
        r#"staging=$(mktemp -d ./.rsctf-setup.XXXXXXXX)
trap 'rm -rf "$staging"' EXIT"#
            .into(),
    );
    lines.push("if [ -n \"$SERVICE_IMAGE\" ]; then".into());
    if let Some((host, shared, volume)) = real_compose {
        lines.push("  workdir=$(docker image inspect --format '{{.Config.WorkingDir}}' \"$SERVICE_IMAGE\" 2>/dev/null || true)".into());
        // Preserve an older service's writable layer when upgrading the observer.
        lines.push(format!("  if mounts=$(docker container inspect --format '{{{{json .Mounts}}}}' 'rsctf-byoc-{game_id}-{challenge_id}-service' 2>/dev/null); then"));
        lines.push(format!("    case \"$mounts\" in *'rsctf-byoc-{game_id}-{challenge_id}_source-'*) ;; *) workdir='' ;; esac"));
        // After compose down only the file tells us whether the service used a source volume.
        lines.push(r#"  elif [ -f docker-compose.yml ] && [ ! -L docker-compose.yml ] && ! grep -Eq '^      - source-[0-9a-f]{16}:/' docker-compose.yml; then workdir=''"#.into());
        lines.push("  fi".into());
        lines.push("  case \"$workdir\" in".into());
        lines.push("    /app|/srv/app|/opt/app|/usr/src/app|/var/www/html|/rsctf-source)".into());
        lines.push("      SOURCE_LOCATION=\"$workdir inside the service container\"".into());
        // Only the service's exact mount line matches. The agent mount stays read-only.
        lines.push(format!(r#"      {} | sed "s|^      - {volume}:/rsctf-source$|      - {volume}:$workdir|" > "$staging/compose" ;;"#, decode_contents(shared.as_bytes())));
        lines.push(format!(
            r#"    *) {} > "$staging/compose" ;;"#,
            decode_contents(host.as_bytes())
        ));
        lines.push("  esac".into());
    } else {
        lines.push("  echo 'internal error: service image selection is inconsistent' >&2".into());
        lines.push("  exit 1".into());
    }
    lines.push("else".into());
    lines.push(r#"  write_placeholder > "$staging/compose""#.into());
    lines.push("fi".into());

    // Compare text before running Compose. Its parser can resolve environment
    // references and included files, which are under the team's control.
    lines.push(r#"if [ -e docker-compose.yml ] && [ ! -L docker-compose.yml ]; then
  [ -f docker-compose.yml ] || keep_compose
  managed_settings() {
    revision=$(sed -nE "s|^    image: \"$DIR-service:reviewed-([0-9a-f]{16})\"$|\1|p" "$1") || return 1
    case "$revision" in *[!0-9a-f]*) return 1 ;; esac
    sed -E -e '/^  rsctf-agent:$/,/^volumes:$/{s/^    image: ".*"$/    image:/;s/^      RSCTF_BYOC_TUNNEL_URL: ".*"$/      RSCTF_BYOC_TUNNEL_URL:/;}' \
      -e "s|^(    image: \"$DIR-service:reviewed-)${revision:-none}\"$|\1REV\"|" \
      -e "s/^(      - |        source: |  )source-${revision:-none}(:.*|$)/\1source-REV\2/" "$1"
  }
  matches_generated() {
    managed_settings "$1" > "$staging/expected" || return 1
    cmp -s "$staging/current" "$staging/expected" && return 0
    # Bundles before source observation had the same service and two fewer agent settings.
    sed '/^  rsctf-agent:$/,/^volumes:$/{/^      RSCTF_BYOC_SOURCE_DISCOVERY: auto$/d;/^      - \.\/service-src:\/rsctf-source:ro$/d;}' "$staging/expected" > "$staging/legacy" || return 1
    cmp -s "$staging/current" "$staging/legacy"
  }
  managed_settings docker-compose.yml > "$staging/current" || keep_compose
  if ! matches_generated "$staging/compose"; then
    [ -n "$SERVICE_IMAGE" ] || keep_compose
    write_placeholder > "$staging/placeholder"
    matches_generated "$staging/placeholder" || keep_compose
  fi
fi"#.into());

    // Bundle the deterministic WireGuard config as an L3 connectivity fallback.
    // Encode the complete payload rather than selecting a heredoc delimiter:
    // user-controlled display text can contain any fixed delimiter on its own
    // line, while base64 has no shell metacharacters or line breaks here.
    lines.push("".into());
    lines.push("echo '[3/4] Writing bundled WireGuard config (rsctf-ad.conf)...'".into());
    lines.push(format!(
        r#"{} > "$staging/vpn""#,
        decode_contents(ctx.wg_config.as_bytes())
    ));
    lines.push(
        r#"for file in docker-compose.yml rsctf-ad.conf; do
  [ -L "$file" ] || [ ! -e "$file" ] || [ -f "$file" ] || keep_compose
done
for file in docker-compose.yml rsctf-ad.conf; do
  if [ -L "$file" ]; then rm -f "$file"; fi
done
mv -f "$staging/compose" docker-compose.yml
mv -f "$staging/vpn" rsctf-ad.conf"#
            .into(),
    );
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
    lines.push("echo \"Source history watches $SOURCE_LOCATION every minute.\"".into());
    lines.push("".into());
    lines.join("\n")
}
