//! Runs in the migrated repository fixture; no Docker/Kata launch is simulated.
use super::super::*;
use super::import_with_game_lock;

pub(super) async fn assert_runtime_profile_sync(state: &SharedState, game_id: i32, root: &Path) {
    let dir = root.join("runtime-profiles");
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let manifest = dir.join("challenge.yml");
    let base = "name: Runtime selection\ntype: DynamicContainer\nflagTemplate: 'flag{[GUID]}'\ncontainer:\n  exposePort: 8080\n";
    tokio::fs::write(&manifest, base).await.unwrap();
    let first = import_with_game_lock(state, game_id, &manifest)
        .await
        .unwrap();
    let id = first.challenge_id;
    let read = || {
        sqlx::query_scalar::<_, Option<String>>(
            r#"SELECT runtime_profile FROM "GameChallenges" WHERE id = $1"#,
        )
        .bind(id)
        .fetch_one(state.pg())
    };
    assert_eq!(
        read().await.unwrap(),
        None,
        "omission retains the deployment default (Docker in the examples)"
    );
    for profile in ["docker", "microvm"] {
        tokio::fs::write(&manifest, format!("{base}  runtimeProfile: {profile}\n"))
            .await
            .unwrap();
        import_with_game_lock(state, game_id, &manifest)
            .await
            .unwrap();
        assert_eq!(read().await.unwrap().as_deref(), Some(profile));
        tokio::fs::write(&manifest, base).await.unwrap();
        import_with_game_lock(state, game_id, &manifest)
            .await
            .unwrap();
        assert_eq!(
            read().await.unwrap().as_deref(),
            Some(profile),
            "sparse rescan preserves an explicit selection"
        );
    }
    sqlx::query(r#"UPDATE "GameChallenges" SET is_enabled = TRUE WHERE id = $1"#)
        .bind(id)
        .execute(state.pg())
        .await
        .unwrap();
    tokio::fs::write(&manifest, format!("{base}  runtimeProfile: docker\n"))
        .await
        .unwrap();
    let live = import_with_game_lock(state, game_id, &manifest)
        .await
        .unwrap();
    assert!(live.runtime_update_deferred);
    assert_eq!(read().await.unwrap().as_deref(), Some("microvm"));
    sqlx::query(r#"UPDATE "GameChallenges" SET is_enabled = FALSE WHERE id = $1"#)
        .bind(id)
        .execute(state.pg())
        .await
        .unwrap();
    tokio::fs::write(&manifest, format!("{base}  runtimeProfile: null\n"))
        .await
        .unwrap();
    import_with_game_lock(state, game_id, &manifest)
        .await
        .unwrap();
    assert_eq!(read().await.unwrap(), None);
    tokio::fs::write(&manifest, format!("{base}  runtimeProfile: missing\n"))
        .await
        .unwrap();
    assert!(import_with_game_lock(state, game_id, &manifest)
        .await
        .is_err());
    assert_eq!(
        read().await.unwrap(),
        None,
        "invalid selection cannot change persistence"
    );
    sqlx::query(r#"DELETE FROM "GameChallenges" WHERE id = $1"#)
        .bind(id)
        .execute(state.pg())
        .await
        .unwrap();
    tokio::fs::remove_dir_all(dir).await.unwrap();
}
