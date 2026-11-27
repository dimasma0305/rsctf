use std::io::{Cursor, Write};
use std::path::Path;

use super::super::{import_manifest, import_repository_snapshot_manifest, ImportPolicy};
use crate::app_state::SharedState;

fn zip_with_dockerfile(dockerfile: &[u8]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("Dockerfile", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(dockerfile).unwrap();
    zip.finish().unwrap().into_inner()
}

pub(super) async fn assert_repository_starter_sync(
    state: &SharedState,
    game_id: i32,
    binding_id: i32,
    neighbor: &Path,
) {
    let package = neighbor.parent().unwrap().join("starter");
    tokio::fs::create_dir_all(&package).await.unwrap();
    let manifest = package.join("challenge.yaml");
    let archive = package.join("starter.zip");
    let source = format!("binding/{binding_id}/event/starter/challenge.yaml");
    tokio::fs::write(
        &manifest,
        "name: Hosted starter fixture\ntype: AttackDefense\ncontainer:\n  containerImage: ghcr.io/example/starter:1\n  exposePort: 8080\nad:\n  starterZip: starter.zip\n",
    )
    .await
    .unwrap();
    let initial = zip_with_dockerfile(b"FROM scratch\n");
    tokio::fs::write(&archive, &initial).await.unwrap();
    let import = || {
        import_repository_snapshot_manifest(
            state,
            game_id,
            &manifest,
            ImportPolicy::Trusted,
            binding_id,
            &source,
        )
    };
    assert!(
        import_manifest(state, game_id, &manifest, ImportPolicy::Trusted)
            .await
            .is_err()
    );
    let first = import().await.unwrap();
    assert!(first.created && first.build_queued);
    let read_starter = || {
        sqlx::query_as::<_, (Vec<u8>, Option<String>, bool)>(
            r#"SELECT archive,runtime_image_ref,awaiting_image_build FROM "AdPatchStarters" WHERE challenge_id=$1"#,
        )
        .bind(first.challenge_id)
        .fetch_one(state.pg())
    };
    assert_eq!(read_starter().await.unwrap(), (initial.clone(), None, true));

    let other = zip_with_dockerfile(b"FROM scratch\n# A different source tree\n");
    tokio::fs::write(&archive, other).await.unwrap();
    assert_eq!(import().await.unwrap().challenge_id, first.challenge_id);
    assert_eq!(read_starter().await.unwrap().0, initial);
    sqlx::query(r#"UPDATE "GameChallenges" SET build_status=$2,build_image_digest=$3 WHERE id=$1"#)
        .bind(first.challenge_id)
        .bind(crate::utils::enums::ChallengeBuildStatus::Success as i16)
        .bind("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .execute(state.pg())
        .await
        .unwrap();
    let (_, image, awaiting) = read_starter().await.unwrap();
    assert!(image.is_some() && !awaiting);
}
