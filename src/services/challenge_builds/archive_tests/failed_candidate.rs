use super::*;
use std::process::Command;

struct TestService {
    name: String,
    image: String,
}

impl Drop for TestService {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-fv", &self.name])
            .output();
        let _ = Command::new("docker")
            .args(["image", "rm", &self.image])
            .output();
    }
}

fn docker_cli(args: &[&str]) -> String {
    let output = Command::new("docker").args(args).output().unwrap();
    assert!(
        output.status.success(),
        "Docker: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[tokio::test]
#[ignore = "requires local Docker BuildKit and a cached Redis image"]
async fn buildkit_failed_patch_candidate_preserves_active_image_and_service() {
    let docker = Docker::connect_with_local_defaults().unwrap();
    let name = format!("rsctf-revision-test-{}", uuid::Uuid::new_v4());
    let service = TestService {
        image: format!("rsctf/diagnostic/{name}:active"),
        name,
    };
    let redis =
        std::env::var("RSCTF_PATCH_TEST_REDIS_IMAGE").unwrap_or_else(|_| "redis:7-alpine".into());
    docker_cli(&["tag", &redis, &service.image]);
    docker_cli(&[
        "run",
        "--pull=never",
        "--detach",
        "--network=none",
        "--name",
        &service.name,
        &service.image,
        "redis-server",
        "--save",
        "",
        "--appendonly",
        "no",
    ]);
    let active_id = docker_cli(&["inspect", "--format", "{{.Image}}", &service.name]);
    let state = || {
        docker_cli(&[
            "inspect",
            "--format",
            "{{.Id}} {{.State.StartedAt}}",
            &service.name,
        ])
    };
    let before = state();
    // Wait for the actual data path. Redis keeps this value in memory, so a
    // replacement or restart will lose it even if the container name is reused.
    let mut ready = false;
    for _ in 0..100 {
        let ping = Command::new("docker")
            .args(["exec", &service.name, "redis-cli", "PING"])
            .output()
            .unwrap();
        if ping.status.success() && ping.stdout.starts_with(b"PONG") {
            ready = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(ready);
    assert_eq!(
        docker_cli(&[
            "exec",
            &service.name,
            "redis-cli",
            "SET",
            "revision-test",
            "original"
        ]),
        "OK"
    );

    let candidate = zip_entry("Dockerfile", b"FROM scratch\nCOPY absent /payload\n");
    let failure = build_from_context(&docker, &service.image, candidate, None).await;
    assert_eq!(failure.status, ChallengeBuildStatus::Failed);
    assert!(failure.image_digest.is_none());

    // Also exercise an interrupted response at a frame boundary, and an image
    // tag replaced between the final Docker frame and the subsequent inspect.
    let mut mismatched = progress::BuildProgress::default();
    mismatched.record(
        serde_json::from_value(serde_json::json!({
            "id":"moby.image.id","aux":{"ID":format!("sha256:{}","0".repeat(64))}
        }))
        .unwrap(),
    );
    for progress in [progress::BuildProgress::default(), mismatched] {
        let result = docker_io::finish_archive_build(&docker, &service.image, progress).await;
        assert_eq!(result.status, ChallengeBuildStatus::Failed);
        assert!(result.image_digest.is_none());
    }
    assert_eq!(
        docker_cli(&["image", "inspect", "--format", "{{.Id}}", &service.image]),
        active_id
    );
    assert_eq!(state(), before);
    assert_eq!(
        docker_cli(&["exec", &service.name, "redis-cli", "GET", "revision-test"]),
        "original"
    );
}
