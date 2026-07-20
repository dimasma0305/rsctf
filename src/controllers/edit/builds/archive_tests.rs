use super::*;
use crate::models::internal::configs::RuntimeRole;
use crate::services::container::ContainerBackendKind;

fn zip_entry(name: &str, data: &[u8]) -> Vec<u8> {
    zip_entries(&[(name, data)], zip::CompressionMethod::Deflated)
}

fn zip_entries(entries: &[(&str, &[u8])], method: zip::CompressionMethod) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(method);
    for (name, data) in entries {
        writer.start_file(*name, options).unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn patch_uncompressed_size(archive: &mut [u8], declared_size: u32) {
    let declared_size = declared_size.to_le_bytes();
    for index in 0..archive.len().saturating_sub(4) {
        match archive[index..index + 4] {
            [0x50, 0x4b, 0x03, 0x04] if index + 26 <= archive.len() => {
                archive[index + 22..index + 26].copy_from_slice(&declared_size);
            }
            [0x50, 0x4b, 0x01, 0x02] if index + 28 <= archive.len() => {
                archive[index + 24..index + 28].copy_from_slice(&declared_size);
            }
            _ => {}
        }
    }
}

fn tar_field(field: &[u8]) -> &str {
    let end = field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len());
    std::str::from_utf8(&field[..end]).unwrap()
}

fn tar_path(tar: &[u8], offset: usize) -> String {
    let name = tar_field(&tar[offset..offset + 100]);
    let prefix = tar_field(&tar[offset + 345..offset + 500]);
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}/{name}")
    }
}

#[test]
fn image_lock_identity_normalizes_implicit_latest_and_docker_hub_aliases() {
    let expected = "docker.io/library/alpine:latest";
    assert_eq!(canonical_image_reference(Some("alpine")), expected);
    assert_eq!(canonical_image_reference(Some("alpine:latest")), expected);
    assert_eq!(canonical_image_reference(Some("library/alpine")), expected);
    assert_eq!(
        canonical_image_reference(Some("index.docker.io/library/alpine")),
        expected
    );
    assert_eq!(
        canonical_image_reference(Some("registry.example:5000/team/app")),
        "registry.example:5000/team/app:latest"
    );
    assert_eq!(
        canonical_image_reference(Some("ghcr.io/team/app:Release")),
        "ghcr.io/team/app:Release"
    );
}

#[test]
fn archive_build_policy_requires_one_explicit_split_docker_daemon() {
    assert_eq!(
        archive_build_rejection(RuntimeRole::All, ContainerBackendKind::Docker, false, false,),
        None
    );
    assert!(
        archive_build_rejection(RuntimeRole::Web, ContainerBackendKind::Docker, false, false,)
            .is_some()
    );
    assert_eq!(
        archive_build_rejection(RuntimeRole::Web, ContainerBackendKind::Docker, true, false,),
        None
    );
    assert!(archive_build_rejection(
        RuntimeRole::All,
        ContainerBackendKind::Kubernetes,
        true,
        false,
    )
    .is_some());
    assert!(
        archive_build_rejection(RuntimeRole::All, ContainerBackendKind::Docker, true, true,)
            .is_some()
    );
}

#[test]
fn terminal_publish_is_compare_and_swap_on_the_complete_definition() {
    assert!(PUBLISH_BUILD_OUTCOME_SQL.contains("build_image_digest = $4"));
    assert!(PUBLISH_BUILD_OUTCOME_SQL.contains("container_image IS NOT DISTINCT FROM $5"));
    assert!(
        PUBLISH_BUILD_OUTCOME_SQL.contains("original_archive_blob_path IS NOT DISTINCT FROM $6")
    );
    assert!(PUBLISH_BUILD_OUTCOME_SQL.contains("build_context_subdir IS NOT DISTINCT FROM $7"));
}

#[test]
fn build_read_and_publication_reject_both_deletion_fences() {
    for sql in [BUILD_FINGERPRINT_SQL, PUBLISH_BUILD_OUTCOME_SQL] {
        assert!(sql.contains("challenge.deletion_pending = FALSE"));
        assert!(sql.contains("game.deletion_pending = FALSE"));
    }
}

#[test]
fn pulled_images_prefer_the_matching_portable_repository_digest() {
    let id = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let expected =
        "ghcr.io/team/app@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let inspected = bollard::models::ImageInspect {
        id: Some(id.to_string()),
        repo_digests: Some(vec![
            "ghcr.io/other/app@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
                .to_string(),
            expected.to_string(),
        ]),
        ..Default::default()
    };
    assert_eq!(
        immutable_image_reference(
            "ghcr.io/team/app:latest",
            &inspected,
            ImageOperation::RegistryPull,
            true,
        )
        .unwrap(),
        expected
    );
}

#[test]
fn portable_topology_never_falls_back_to_a_local_image_id() {
    let id = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let inspected = bollard::models::ImageInspect {
        id: Some(id.to_string()),
        ..Default::default()
    };
    assert!(immutable_image_reference(
        "registry.example/team/app:latest",
        &inspected,
        ImageOperation::RegistryPull,
        true,
    )
    .is_err());
    assert_eq!(
        immutable_image_reference(
            "rsctf/game/app:latest",
            &inspected,
            ImageOperation::ArchiveBuild,
            false,
        )
        .unwrap(),
        id
    );
}

#[test]
fn definition_edits_return_to_the_matching_unbuilt_state() {
    assert_eq!(
        invalidated_build_status(
            Some("registry.example/app:v2"),
            Some("archive-hash"),
            Some(".")
        ),
        ChallengeBuildStatus::Queued
    );
    assert_eq!(
        invalidated_build_status(Some("registry.example/app:v2"), None, Some(".")),
        ChallengeBuildStatus::Queued
    );
    assert_eq!(
        invalidated_build_status(Some("  "), Some("archive-hash"), Some(".")),
        ChallengeBuildStatus::NotApplicable
    );
    assert_eq!(
        invalidated_build_status(
            Some("registry.example/app:v2"),
            Some("audit-only-hash"),
            None
        ),
        ChallengeBuildStatus::Queued
    );
}

#[test]
fn build_archive_accepts_a_small_safe_zip() {
    let archive = zip_entry("Dockerfile", b"FROM scratch\n");
    let tar = zip_bytes_to_tar(&archive, None).unwrap();
    assert!(tar.len() >= 2 * 512);
    assert!(zip_bytes_to_tar(&archive, Some(".")).is_ok());
    assert!(zip_bytes_to_tar(&archive, Some("../escape")).is_err());
}

#[test]
fn build_archive_selects_reviewed_context_subdirectory() {
    let archive = zip_entries(
        &[
            ("challenge.yml", b"name: reviewed\n"),
            ("checker/run.py", b"print('check')\n"),
            ("src/Dockerfile", b"FROM scratch\n"),
            ("src/app", b"payload\n"),
        ],
        zip::CompressionMethod::Stored,
    );
    let tar = zip_bytes_to_tar(&archive, Some("src")).unwrap();
    assert_eq!(tar_path(&tar, 0), "Dockerfile");
    assert_eq!(tar_path(&tar, 1024), "app");
}

#[test]
fn build_archive_rejects_traversal_and_zip_bombs() {
    let traversal = zip_entry("../Dockerfile", b"FROM scratch\n");
    assert!(zip_bytes_to_tar(&traversal, None).is_err());

    let backslash_traversal = zip_entry("..\\Dockerfile", b"FROM scratch\n");
    assert!(zip_bytes_to_tar(&backslash_traversal, None).is_err());

    let parent_alias = zip_entry("context/../Dockerfile", b"FROM scratch\n");
    assert!(zip_bytes_to_tar(&parent_alias, None).is_err());

    let compressed_bomb = zip_entry("Dockerfile", &vec![0u8; 1024 * 1024]);
    assert!(zip_bytes_to_tar(&compressed_bomb, None)
        .unwrap_err()
        .contains("compression ratio"));
}

#[test]
fn build_archive_preserves_distinct_long_ustar_paths() {
    let prefix = "context".repeat(15);
    let first = format!("{prefix}/Dockerfile-a");
    let second = format!("{prefix}/Dockerfile-b");
    assert_eq!(&first.as_bytes()[..100], &second.as_bytes()[..100]);

    let archive = zip_entries(
        &[
            ("Dockerfile", b"FROM scratch\n"),
            (first.as_str(), b"a"),
            (second.as_str(), b"b"),
        ],
        zip::CompressionMethod::Stored,
    );
    let tar = zip_bytes_to_tar(&archive, None).unwrap();
    assert_eq!(tar_path(&tar, 1024), first);
    assert_eq!(tar_path(&tar, 2048), second);

    let unrepresentable = zip_entry(&"x".repeat(101), b"x");
    assert!(zip_bytes_to_tar(&unrepresentable, None)
        .unwrap_err()
        .contains("cannot be represented"));
}

#[test]
fn build_archive_rejects_forged_uncompressed_size() {
    let mut archive = zip_entries(
        &[("Dockerfile", b"FROM scratch\n")],
        zip::CompressionMethod::Stored,
    );
    patch_uncompressed_size(&mut archive, 1);
    assert!(zip_bytes_to_tar(&archive, None).is_err());
}
