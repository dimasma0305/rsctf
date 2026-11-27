use std::io::{Cursor, Read};

use super::super::{import_manifest, import_repository_snapshot_manifest, ImportPolicy};
use super::repository_fixture::{repository_fixture, RepositoryFixture};

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn repository_registers_starter_from_src_and_pins_initial_image() {
    let RepositoryFixture {
        test_db,
        state,
        root,
        binding,
        game,
        ..
    } = repository_fixture().await;
    let package = root
        .join("repos")
        .join(binding.id.to_string())
        .join("event/starter");
    let src = package.join("src");
    tokio::fs::create_dir_all(&src).await.unwrap();
    let manifest = package.join("challenge.yaml");
    let source = format!("binding/{}/event/starter/challenge.yaml", binding.id);
    tokio::fs::write(
        &manifest,
        "name: Hosted starter fixture\ntype: AttackDefense\ncontainer:\n  exposePort: 8080\nad:\n  starterZip: src\n",
    )
    .await
    .unwrap();
    tokio::fs::write(src.join("Dockerfile"), b"FROM scratch\n")
        .await
        .unwrap();
    let banner = src.join("banner.txt");
    tokio::fs::write(&banner, b"original").await.unwrap();
    let import = || {
        import_repository_snapshot_manifest(
            &state,
            game.id,
            &manifest,
            ImportPolicy::Trusted,
            binding.id,
            &source,
        )
    };
    assert!(
        import_manifest(&state, game.id, &manifest, ImportPolicy::Trusted)
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
    let (initial, image, awaiting) = read_starter().await.unwrap();
    assert!(image.is_none() && awaiting);
    let mut zip = zip::ZipArchive::new(Cursor::new(&initial)).unwrap();
    let mut contents = Vec::new();
    zip.by_name("banner.txt")
        .unwrap()
        .read_to_end(&mut contents)
        .unwrap();
    assert_eq!(contents, b"original");
    assert!(zip.by_name("Dockerfile").is_ok());
    tokio::fs::write(&banner, b"new").await.unwrap();
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
    drop(state);
    test_db.close().await;
    tokio::fs::remove_dir_all(root).await.unwrap();
}
