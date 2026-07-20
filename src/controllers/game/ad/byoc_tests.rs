use super::*;

fn context_with_hostile_values(marker: &std::path::Path) -> ByocContext {
    let marker = marker.display();
    ByocContext {
        title: format!("quote' \" $(touch {marker})\r\nWGCONF\nCOMPOSE"),
        container_image: Some(format!(
            "registry.invalid/service:'\"\r\nCOMPOSE\n$(touch {marker})"
        )),
        svc_port: 31337,
        tunnel_url: format!("wss://ctf.invalid/agent/'\"\r\nCOMPOSE\n$(touch {marker})"),
        image_url: format!("https://ctf.invalid/image/'\"\r\nWGCONF\n$(touch {marker})"),
        agent_image: format!("agent:'\"\r\nCOMPOSE\n$(touch {marker})"),
        agent_image_requires_amd64: false,
        wg_config: format!(
            "[Interface]\r\n# hostile display name\r\nWGCONF\r\nprintf pwned > {marker}\r\n$(touch {marker})\r\n`touch {marker}`\r\nCOMPOSE\r\n"
        ),
    }
}

#[test]
fn only_a_same_build_agent_digest_can_be_the_compiled_default() {
    let compiled = option_env!("RSCTF_DEFAULT_BYOC_AGENT_IMAGE")
        .unwrap_or("")
        .trim();
    let compiled_multiarch = option_env!("RSCTF_DEFAULT_BYOC_AGENT_MULTIARCH") == Some("true");
    if compiled.is_empty() {
        assert_eq!(default_byoc_agent_image(), None);
    } else {
        let (image, requires_amd64) = default_byoc_agent_image().unwrap();
        assert_eq!(image, compiled);
        assert_eq!(immutable_agent_image(image).as_deref(), Some(image));
        assert_eq!(requires_amd64, !compiled_multiarch);
    }
}

#[test]
fn built_in_agent_setup_fails_early_on_unsupported_architectures() {
    let mut ctx = context_with_hostile_values(std::path::Path::new("/tmp/not-created"));
    ctx.agent_image = format!("registry.example/agent@sha256:{}", "a".repeat(64));
    ctx.agent_image_requires_amd64 = true;
    let built_in = build_setup_script(7, 11, &ctx);
    assert!(built_in.contains("x86_64|amd64"));
    assert!(built_in.contains("currently supports Linux amd64 only"));
    assert!(built_in.contains("RSCTF_AD_BYOC_AGENT_IMAGE"));

    ctx.agent_image = format!("registry.example/agent@sha256:{}", "b".repeat(64));
    ctx.agent_image_requires_amd64 = false;
    let override_script = build_setup_script(7, 11, &ctx);
    assert!(!override_script.contains("currently supports Linux amd64 only"));
}

#[test]
fn ack_capability_must_be_explicitly_offered() {
    let mut headers = HeaderMap::new();
    assert!(!byoc_agent_protocol_offered(&headers));
    headers.insert(
        crate::services::byoc_tunnel::AGENT_PROTOCOL_HEADER,
        axum::http::HeaderValue::from_static("legacy, rsctf-byoc-v2"),
    );
    assert!(byoc_agent_protocol_offered(&headers));
    headers.insert(
        crate::services::byoc_tunnel::AGENT_PROTOCOL_HEADER,
        axum::http::HeaderValue::from_static("rsctf-byoc-v20"),
    );
    assert!(!byoc_agent_protocol_offered(&headers));
}

#[test]
fn team_secret_rotation_revokes_byoc_tokens() {
    let before = byoc_token("adbyocagent:", "game", "team-a", 7, 11);
    let after = byoc_token("adbyocagent:", "game", "team-b", 7, 11);

    assert_ne!(before, after);
    assert_eq!(before, byoc_token("adbyocagent:", "game", "team-a", 7, 11));
    assert_ne!(before, byoc_token("adbyocimage:", "game", "team-a", 7, 11));
}

#[test]
fn byoc_agent_images_must_be_pinned_by_digest() {
    assert_eq!(
        immutable_agent_image(
            "registry.example/agent@sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        )
        .as_deref(),
        Some(
            "registry.example/agent@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        )
    );
    for mutable_or_invalid in [
        "ghcr.io/dimasma0305/rsctf-byoc-agent:latest",
        "registry.example/agent:1.2.3",
        "registry.example/agent@sha256:short",
        "registry.example/agent@sha256:zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
        "registry.example/agent @sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ] {
        assert_eq!(
            immutable_agent_image(mutable_or_invalid),
            None,
            "accepted mutable/invalid image {mutable_or_invalid:?}"
        );
    }
}

#[test]
fn generated_setup_keeps_the_root_equivalent_docker_socket_opt_in() {
    let ctx = context_with_hostile_values(std::path::Path::new("/tmp/not-created"));
    let compose = build_setup_compose(7, 11, &ctx, None);
    assert!(compose.contains("# - /var/run/docker.sock:/var/run/docker.sock"));
    assert!(!compose.contains("\n      - /var/run/docker.sock:/var/run/docker.sock"));
}

#[test]
fn generated_placeholder_service_uses_an_immutable_multi_arch_image() {
    let ctx = context_with_hostile_values(std::path::Path::new("/tmp/not-created"));
    for compose in [
        build_setup_compose(7, 11, &ctx, None),
        build_compose(7, 11, &ctx),
    ] {
        assert!(compose.contains(&format!("image: {DEFAULT_BYOC_FALLBACK_IMAGE}")));
        assert!(!compose.contains("image: alpine/socat\n"));
    }
}

#[test]
fn generated_real_service_uses_a_revision_scoped_local_image_without_pull() {
    let mut ctx = context_with_hostile_values(std::path::Path::new("/tmp/not-created"));
    let immutable = format!("registry.example/service@sha256:{}", "a".repeat(64));
    ctx.container_image = Some(immutable.clone());
    let reviewed = reviewed_service_image_name(7, 11, &immutable);
    let compose = build_setup_compose(7, 11, &ctx, Some(&reviewed));

    assert!(compose.contains(&format!("image: {}", compose_scalar(&reviewed))));
    assert!(compose.contains("    pull_policy: never"));
    assert!(!compose.contains(&compose_scalar(&immutable)));
    assert_eq!(reviewed, reviewed_service_image_name(7, 11, &immutable));
    assert_ne!(
        reviewed,
        reviewed_service_image_name(
            7,
            11,
            &format!("registry.example/service@sha256:{}", "b".repeat(64))
        )
    );
}

#[test]
fn secret_bundle_attachments_are_never_cacheable() {
    let response = text_attachment("application/yaml", "compose.yml", "secret".to_string());
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some(BYOC_SECRET_CACHE_CONTROL)
    );
}

#[test]
fn public_origin_ignores_forwarded_host_and_rejects_shell_syntax() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::HOST,
        axum::http::HeaderValue::from_static("ctf.example:8443"),
    );
    headers.insert(
        "x-forwarded-host",
        axum::http::HeaderValue::from_static("attacker.invalid/$(touch-pwned)"),
    );
    headers.insert(
        "x-forwarded-proto",
        axum::http::HeaderValue::from_static("https,$(touch-pwned)"),
    );

    assert_eq!(
        canonical_public_origin(None, &headers).ok().as_deref(),
        Some("https://ctf.example:8443")
    );
    assert_eq!(
        canonical_public_origin(Some("https://public.example/"), &headers)
            .ok()
            .as_deref(),
        Some("https://public.example")
    );

    headers.insert(
        header::HOST,
        axum::http::HeaderValue::from_static("ctf.example;$(touch-pwned)"),
    );
    assert!(canonical_public_origin(None, &headers).is_err());

    for hostile in [
        "javascript:alert(1)",
        "https://user@ctf.example",
        "https://ctf.example/$(touch-pwned)",
        "https://ctf.example/?next=$(touch-pwned)",
        "https://ctf.example/'\"$()",
        "https://ctf.example/\r\nWGCONF\r\n$(touch-pwned)",
    ] {
        assert_eq!(
            normalize_public_origin(hostile),
            None,
            "accepted {hostile:?}"
        );
    }

    assert_eq!(
        normalize_public_origin("http://[::1]:8080"),
        Some("http://[::1]:8080".to_string())
    );
}

#[test]
fn image_exports_are_bounded_globally_and_per_participation() {
    let admission = ImageExportAdmission::new(2);
    let first = admission.try_admit(1, 10).expect("first export admitted");

    assert!(admission.try_admit(1, 10).is_none());
    assert!(
        admission.try_admit(1, 11).is_none(),
        "one participation must not occupy parallel challenge exports"
    );
    let second = admission.try_admit(2, 10).expect("second export admitted");
    assert!(admission.try_admit(3, 10).is_none());

    drop(first);
    let replacement = admission
        .try_admit(1, 11)
        .expect("released participation and global slots are reusable");
    assert!(admission.try_admit(1, 10).is_none());

    drop(second);
    drop(replacement);
    assert!(admission.try_admit(3, 10).is_some());
}

#[tokio::test]
async fn paused_export_source_releases_on_idle_timeout() {
    let admission = ImageExportAdmission::new(1);
    let permit = admission.try_admit(7, 11).expect("export admitted");
    let stream = futures::stream::pending::<Result<bytes::Bytes, std::io::Error>>();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let end = {
        let _permit = permit;
        tokio::time::timeout(
            Duration::from_secs(2),
            forward_image_export(
                stream,
                tx,
                Duration::from_millis(20),
                Duration::from_secs(1),
            ),
        )
        .await
        .expect("forwarder itself hung")
    };

    assert_eq!(end, ImageExportEnd::SourceIdleTimeout);
    assert!(
        admission.try_admit(7, 11).is_some(),
        "timeout must release the participation and capability gates"
    );
    let error = rx
        .recv()
        .await
        .expect("timeout error sent")
        .expect_err("timeout must terminate the body with an error");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
}

#[tokio::test]
async fn stalled_download_consumer_releases_on_idle_timeout() {
    let stream = futures::stream::iter([
        Ok(bytes::Bytes::from_static(b"first")),
        Ok(bytes::Bytes::from_static(b"second")),
    ]);
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let end = tokio::time::timeout(
        Duration::from_secs(2),
        forward_image_export(
            stream,
            tx,
            Duration::from_millis(20),
            Duration::from_secs(1),
        ),
    )
    .await
    .expect("forwarder itself hung");

    assert_eq!(end, ImageExportEnd::ClientIdleTimeout);
    assert_eq!(
        rx.recv()
            .await
            .expect("buffered first chunk")
            .expect("chunk"),
        bytes::Bytes::from_static(b"first")
    );
    assert!(rx.recv().await.is_none());
}

#[tokio::test]
async fn trickling_export_releases_at_absolute_duration_limit() {
    let stream = futures::stream::unfold((), |_| async {
        tokio::time::sleep(Duration::from_millis(2)).await;
        Some((Ok(bytes::Bytes::from_static(b"chunk")), ()))
    });
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let end = tokio::time::timeout(
        Duration::from_secs(2),
        forward_image_export(
            Box::pin(stream),
            tx,
            Duration::from_secs(1),
            Duration::from_millis(30),
        ),
    )
    .await
    .expect("forwarder itself hung");

    assert_eq!(end, ImageExportEnd::DurationLimit);
    drain.await.expect("drain task");
}

#[tokio::test]
async fn export_start_returns_first_chunk_and_preserves_the_tail() {
    use futures::StreamExt;

    let stream = futures::stream::iter([
        Ok(bytes::Bytes::from_static(b"first")),
        Ok(bytes::Bytes::from_static(b"tail")),
    ]);
    let (first, mut tail) = poll_image_export_start(stream, Duration::from_secs(1))
        .await
        .expect("export did not start");
    assert_eq!(first, bytes::Bytes::from_static(b"first"));
    assert_eq!(
        tail.next().await.unwrap().unwrap(),
        bytes::Bytes::from_static(b"tail")
    );
    assert!(tail.next().await.is_none());
}

#[tokio::test]
async fn export_start_wait_is_bounded_before_authorization_handoff() {
    let stream = futures::stream::pending::<Result<bytes::Bytes, std::io::Error>>();
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        poll_image_export_start(stream, Duration::from_millis(20)),
    )
    .await
    .expect("startup poll ignored its bound");
    assert!(matches!(result, Err(ImageExportStartError::IdleTimeout)));
}

#[tokio::test]
async fn dropping_response_body_stops_the_next_produced_chunk() {
    let stream = futures::stream::iter([Ok(bytes::Bytes::from_static(b"chunk"))]);
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    drop(rx);
    let end = tokio::time::timeout(
        Duration::from_secs(1),
        forward_image_export(stream, tx, Duration::from_secs(1), Duration::from_secs(1)),
    )
    .await
    .expect("a dropped body left Docker export running");
    assert_eq!(end, ImageExportEnd::ClientDisconnected);
}

#[test]
fn compose_scalars_escape_controls_quotes_and_interpolation() {
    let scalar = compose_scalar("image:'\"\\\r\n$(touch pwned)");

    assert!(!scalar.contains('\r'));
    assert!(!scalar.contains('\n'));
    assert!(scalar.contains("\\r\\n"));
    assert!(scalar.contains("$$(touch pwned)"));
    assert_eq!(
        serde_json::from_str::<String>(&scalar).expect("valid JSON/YAML scalar"),
        "image:'\"\\\r\n$$(touch pwned)"
    );
}

#[cfg(unix)]
#[test]
fn hostile_generated_setup_is_shell_safe_and_preserves_wireguard_bytes() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let root = std::env::temp_dir().join(format!(
        "rsctf-byoc-script-security-{}",
        uuid::Uuid::new_v4()
    ));
    let bin = root.join("bin");
    let marker = root.join("pwned");
    std::fs::create_dir_all(&bin).expect("create emulation directory");

    let docker = bin.join("docker");
    std::fs::write(
        &docker,
        "#!/bin/sh\nif [ \"$1\" = load ]; then exit 1; fi\nexit 0\n",
    )
    .expect("write docker stub");
    std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o700))
        .expect("make docker stub executable");
    let curl = bin.join("curl");
    std::fs::write(&curl, "#!/bin/sh\nexit 1\n").expect("write curl stub");
    std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o700))
        .expect("make curl stub executable");

    let ctx = context_with_hostile_values(&marker);
    let script = build_setup_script(17, 23, &ctx);
    let syntax = Command::new("sh")
        .args(["-n", "-c", &script])
        .output()
        .expect("run shell parser");
    assert!(
        syntax.status.success(),
        "generated script did not parse: {}",
        String::from_utf8_lossy(&syntax.stderr)
    );

    let mut path = std::ffi::OsString::from(bin.as_os_str());
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    let output = Command::new("sh")
        .args(["-c", &script])
        .current_dir(&root)
        .env("PATH", path)
        .output()
        .expect("emulate generated installer");
    assert!(
        output.status.success(),
        "installer emulation failed; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !marker.exists(),
        "hostile payload escaped into shell execution"
    );

    let output_dir = root.join("rsctf-byoc-17-23");
    assert_eq!(
        std::fs::read(output_dir.join("rsctf-ad.conf")).expect("read generated WG config"),
        ctx.wg_config.as_bytes()
    );
    for sensitive in ["docker-compose.yml", "rsctf-ad.conf"] {
        let mode = std::fs::metadata(output_dir.join(sensitive))
            .expect("read generated file metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "{sensitive} is readable outside its owner");
    }
    let compose =
        std::fs::read_to_string(output_dir.join("docker-compose.yml")).expect("read compose");
    assert!(compose.contains(&compose_scalar(&ctx.agent_image)));
    assert!(compose.contains(&compose_scalar(&ctx.tunnel_url)));

    std::fs::remove_dir_all(root).expect("clean emulation directory");
}

#[cfg(unix)]
#[test]
fn successful_setup_tags_docker_loads_exact_result_before_compose_starts() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    const LOADED_ID: &str =
        "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    let root = std::env::temp_dir().join(format!(
        "rsctf-byoc-script-local-image-{}",
        uuid::Uuid::new_v4()
    ));
    let bin = root.join("bin");
    let marker = root.join("pwned");
    let tag_log = root.join("tag.log");
    let compose_log = root.join("started-compose.yml");
    let output_dir = root.join("rsctf-byoc-17-23");
    let symlink_target = root.join("outside-compose.yml");
    std::fs::create_dir_all(&bin).expect("create emulation directory");
    std::fs::create_dir_all(&output_dir).expect("create prior output directory");
    std::fs::set_permissions(&output_dir, std::fs::Permissions::from_mode(0o777))
        .expect("make prior directory permissive");
    std::fs::write(&symlink_target, "must remain unchanged\n").expect("write symlink target");
    std::os::unix::fs::symlink(&symlink_target, output_dir.join("docker-compose.yml"))
        .expect("plant compose symlink");
    std::fs::write(output_dir.join("rsctf-ad.conf"), "old secret\n")
        .expect("write permissive old WireGuard file");
    std::fs::set_permissions(
        output_dir.join("rsctf-ad.conf"),
        std::fs::Permissions::from_mode(0o644),
    )
    .expect("make old WireGuard file permissive");
    std::fs::write(output_dir.join(".env"), "COMPOSE_FILE=attacker.yml\n")
        .expect("plant compose environment");
    std::fs::write(
        output_dir.join("attacker.yml"),
        "services: { attacker: {} }\n",
    )
    .expect("plant alternate compose");

    let docker = bin.join("docker");
    std::fs::write(
        &docker,
        format!(
            "#!/bin/sh\n\
             case \"$1:$2\" in\n\
               load:) cat >/dev/null; printf '%s\\n' 'Loaded image ID: {LOADED_ID}' ;;\n\
               image:inspect) [ \"$5\" = '{LOADED_ID}' ] || exit 21; printf '%s\\n' '{LOADED_ID}' ;;\n\
               image:tag) printf '%s\\n%s\\n' \"$3\" \"$4\" > \"$RSCTF_TEST_TAG_LOG\" ;;\n\
               compose:--env-file) [ \"$3:$4:$5:$6:$7\" = '/dev/null:-f:docker-compose.yml:up:-d' ] || exit 23; [ \"$COMPOSE_PROJECT_NAME\" = 'rsctf-byoc-17-23' ] || exit 24; cp docker-compose.yml \"$RSCTF_TEST_COMPOSE_LOG\" ;;\n\
               *) exit 22 ;;\n\
             esac\n"
        ),
    )
    .expect("write docker stub");
    std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o700))
        .expect("make docker stub executable");
    let curl = bin.join("curl");
    std::fs::write(&curl, "#!/bin/sh\nprintf archive\n").expect("write curl stub");
    std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o700))
        .expect("make curl stub executable");

    let mut ctx = context_with_hostile_values(&marker);
    let immutable = format!("registry.example/service@sha256:{}", "a".repeat(64));
    ctx.container_image = Some(immutable.clone());
    let reviewed = reviewed_service_image_name(17, 23, &immutable);
    let script = build_setup_script(17, 23, &ctx);
    let mut path = std::ffi::OsString::from(bin.as_os_str());
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    let output = Command::new("sh")
        .args(["-c", &script])
        .current_dir(&root)
        .env("PATH", path)
        .env("RSCTF_TEST_TAG_LOG", &tag_log)
        .env("RSCTF_TEST_COMPOSE_LOG", &compose_log)
        .output()
        .expect("emulate generated installer");
    assert!(
        output.status.success(),
        "installer emulation failed; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !marker.exists(),
        "hostile payload escaped into shell execution"
    );
    assert_eq!(
        std::fs::read_to_string(&symlink_target).expect("read symlink target"),
        "must remain unchanged\n",
        "setup followed a planted output symlink"
    );

    let tagged = std::fs::read_to_string(&tag_log).expect("read image-tag trace");
    assert_eq!(tagged, format!("{LOADED_ID}\n{reviewed}\n"));
    let compose = std::fs::read_to_string(&compose_log).expect("read started compose");
    assert!(compose.contains(&format!("image: {}", compose_scalar(&reviewed))));
    assert!(compose.contains("    pull_policy: never"));
    assert!(!compose.contains(&compose_scalar(&immutable)));
    for sensitive in ["docker-compose.yml", "rsctf-ad.conf"] {
        let metadata =
            std::fs::symlink_metadata(output_dir.join(sensitive)).expect("read protected output");
        assert!(
            !metadata.file_type().is_symlink(),
            "{sensitive} stayed a symlink"
        );
        assert_eq!(metadata.permissions().mode() & 0o077, 0);
    }
    assert_eq!(
        std::fs::metadata(&output_dir)
            .expect("read protected directory")
            .permissions()
            .mode()
            & 0o077,
        0
    );

    std::fs::remove_dir_all(root).expect("clean emulation directory");
}

#[cfg(unix)]
#[test]
fn setup_refuses_a_symlinked_target_directory() {
    use std::os::unix::fs::symlink;
    use std::process::Command;

    let root = std::env::temp_dir().join(format!(
        "rsctf-byoc-script-directory-symlink-{}",
        uuid::Uuid::new_v4()
    ));
    let outside = root.join("outside");
    std::fs::create_dir_all(&outside).expect("create outside directory");
    let sentinel = outside.join("sentinel");
    std::fs::write(&sentinel, "unchanged\n").expect("write sentinel");
    symlink(&outside, root.join("rsctf-byoc-17-23")).expect("plant directory symlink");

    let ctx = context_with_hostile_values(&root.join("pwned"));
    let script = build_setup_script(17, 23, &ctx);
    let output = Command::new("sh")
        .args(["-c", &script])
        .current_dir(&root)
        .output()
        .expect("run generated installer");

    assert!(!output.status.success(), "symlinked directory was accepted");
    assert!(String::from_utf8_lossy(&output.stderr).contains("symlinked BYOC setup directory"));
    assert_eq!(
        std::fs::read_to_string(&sentinel).expect("read sentinel"),
        "unchanged\n"
    );
    assert!(!outside.join("docker-compose.yml").exists());
    assert!(!outside.join("rsctf-ad.conf").exists());

    std::fs::remove_dir_all(root).expect("clean emulation directory");
}
