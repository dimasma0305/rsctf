//! Hardened Docker Compose documents for BYOC onboarding.

use super::*;

pub(super) fn build_setup_compose(
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
        // The setup script tagged Docker load's content ID with this local-only
        // name. Never let Compose replace it from a registry.
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
        "    read_only: true".into(),
        "    cap_drop:".into(),
        "      - ALL".into(),
        "    security_opt:".into(),
        "      - no-new-privileges:true".into(),
        "    pids_limit: 128".into(),
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

/// The bring-your-own-service compose with a working placeholder service.
pub(super) fn build_compose(game_id: i32, challenge_id: i32, ctx: &ByocContext) -> String {
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
        "    read_only: true".into(),
        "    cap_drop:".into(),
        "      - ALL".into(),
        "    security_opt:".into(),
        "      - no-new-privileges:true".into(),
        "    pids_limit: 128".into(),
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
