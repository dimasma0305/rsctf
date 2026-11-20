use super::*;
use std::process::Command;

struct TestService {
    name: String,
    image: String,
    candidate: Option<String>,
}

impl Drop for TestService {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-fv", &self.name])
            .output();
        let _ = Command::new("docker")
            .args(["image", "rm", &self.image])
            .output();
        if let Some(id) = &self.candidate {
            let _ = Command::new("docker").args(["image", "rm", id]).output();
        }
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
    let mut service = TestService {
        image: format!("rsctf/diagnostic/{name}:active"),
        name,
        candidate: None,
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

    let archive = zip_entries(
        &[
            ("Dockerfile", b"FROM scratch\nCOPY source /source\n"),
            ("source", service.name.as_bytes()),
        ],
        zip::CompressionMethod::Stored,
    );
    let tar = zip_bytes_to_tar(&archive, None).unwrap();
    let complete = docker_io::build_candidate(&docker, &service.image, tar).await;
    let (log, receipt) = complete.into_parts();
    let built = receipt.unwrap_or_else(|e| panic!("{e}: {log}"));
    service.candidate = Some(built.clone());
    assert_ne!(built, active_id);
    let inspected = docker.inspect_image(&built).await.unwrap();
    assert!(inspected.repo_tags.unwrap_or_default().is_empty());
    assert_eq!(
        docker_cli(&["image", "inspect", "-f", "{{.Id}}", &service.image]),
        active_id
    );

    let receipt = |id: &str| {
        let mut progress = progress::BuildProgress::default();
        progress.record(
            serde_json::from_value(serde_json::json!({
                "id":"moby.image.id","aux":{"ID":id}
            }))
            .unwrap(),
        );
        progress
    };
    let mut interrupted = receipt(&built);
    interrupted.fail("transport interrupted after export".into());
    for (previous, progress) in [
        (Some(active_id.as_str()), progress::BuildProgress::default()),
        (Some(active_id.as_str()), interrupted),
        (Some(active_id.as_str()), receipt(&active_id)), // different owner's image
        (None, receipt(&built)),                         // a destination appeared during the build
    ] {
        let result =
            docker_io::finish_archive_build(&docker, &service.image, previous, progress).await;
        assert_eq!(result.status, ChallengeBuildStatus::Failed);
        assert!(result.image_digest.is_none());
    }
    assert_eq!(
        docker_cli(&["image", "inspect", "--format", "{{.Id}}", &service.image]),
        active_id
    );
    // Simulate reclaiming precisely this abandoned output. A stale receipt
    // cannot publish a collected image or displace the existing tag.
    docker
        .remove_image(
            &built,
            Some(bollard::image::RemoveImageOptions {
                force: false,
                noprune: true,
            }),
            None,
        )
        .await
        .unwrap();
    let collected =
        docker_io::finish_archive_build(&docker, &service.image, Some(&active_id), receipt(&built))
            .await;
    assert_eq!(collected.status, ChallengeBuildStatus::Failed);
    assert_eq!(
        docker_cli(&["image", "inspect", "-f", "{{.Id}}", &service.image]),
        active_id
    );
    let retry = build_from_context(&docker, &service.image, archive, None).await;
    assert_eq!(
        retry.status,
        ChallengeBuildStatus::Success,
        "{:?}",
        retry.log
    );
    let published = retry.image_digest.unwrap();
    assert_ne!(published, active_id);
    assert_eq!(
        docker_cli(&["image", "inspect", "-f", "{{.Id}}", &service.image]),
        published
    );
    assert_eq!(
        docker_cli(&["inspect", "-f", "{{.Image}}", &service.name]),
        active_id
    );
    assert_eq!(state(), before);
    assert_eq!(
        docker_cli(&["exec", &service.name, "redis-cli", "GET", "revision-test"]),
        "original"
    );
}
