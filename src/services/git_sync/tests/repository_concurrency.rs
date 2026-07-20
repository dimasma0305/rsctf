use super::super::*;
use super::import_with_game_lock;

use crate::models::data::submission;
use crate::services::git_sync;
use crate::utils::enums::{AnswerResult, RepoWatchStatus};

pub(super) async fn assert_submission_evidence_fence(
    state: &SharedState,
    root: &Path,
    binding_id: i32,
    game_id: i32,
    user_id: uuid::Uuid,
    team_id: i32,
    participation_id: i32,
) -> i32 {
    // The import starts pre-work while a submit-side shared lock is held. A
    // wrong attempt committed before the exclusive importer proceeds is enough
    // to make the old grading policy durable, even before game start.
    let race_dir = root
        .join("repos")
        .join(binding_id.to_string())
        .join("event/web/race");
    tokio::fs::create_dir_all(&race_dir).await.unwrap();
    let manifest = race_dir.join("challenge.yaml");
    tokio::fs::write(
        &manifest,
        "name: Race fence\ntype: StaticAttachment\ncategory: Web\nflags:\n  - flag{race_old}\n",
    )
    .await
    .unwrap();
    let challenge_id = import_with_game_lock(state, game_id, &manifest)
        .await
        .unwrap()
        .challenge_id;
    sqlx::query(r#"UPDATE "GameChallenges" SET is_enabled = TRUE WHERE id = $1"#)
        .bind(challenge_id)
        .execute(state.pg())
        .await
        .unwrap();
    tokio::fs::write(
        &manifest,
        "name: Race fence updated\ntype: StaticAttachment\ncategory: Web\nsubmissionLimit: 44\nflags:\n  - flag{race_new}\n",
    )
    .await
    .unwrap();
    let mut submit_fence = crate::utils::database::begin_sqlx_transaction(state.pg())
        .await
        .unwrap();
    crate::utils::scoring::lock_jeopardy_flags_shared(&mut submit_fence, challenge_id)
        .await
        .unwrap();
    let task_state = state.clone();
    let task_manifest = manifest.clone();
    let mut import_task =
        tokio::spawn(
            async move { import_with_game_lock(&task_state, game_id, &task_manifest).await },
        );
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut import_task)
            .await
            .is_err()
    );
    submission::ActiveModel {
        answer: Set("wrong attempt".to_string()),
        status: Set(AnswerResult::WrongAnswer),
        submit_time_utc: Set(Utc::now()),
        user_id: Set(Some(user_id)),
        team_id: Set(team_id),
        participation_id: Set(participation_id),
        game_id: Set(game_id),
        challenge_id: Set(challenge_id),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .unwrap();
    submit_fence.commit().await.unwrap();
    let result = import_task.await.unwrap().unwrap();
    assert!(result.grading_update_deferred);
    assert_eq!(
        sqlx::query_scalar::<_, i32>(
            r#"SELECT submission_limit FROM "GameChallenges" WHERE id = $1"#,
        )
        .bind(challenge_id)
        .fetch_one(state.pg())
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            r#"SELECT flag FROM "FlagContexts" WHERE challenge_id = $1"#,
        )
        .bind(challenge_id)
        .fetch_all(state.pg())
        .await
        .unwrap(),
        vec!["flag{race_old}".to_string()]
    );
    challenge_id
}

pub(super) async fn assert_cleanup_reenable_transition(state: &SharedState, challenge_id: i32) {
    // Model cleanup paused immediately before runtime destruction. The second
    // connection cannot make false -> true visible until cleanup relinquishes
    // the exact outer fence used by production reconciliation.
    let cleanup = crate::services::challenge_workloads::acquire_runtime_transition_lock(
        state.pg(),
        challenge_id,
    )
    .await
    .unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (acquired_tx, mut acquired_rx) = tokio::sync::oneshot::channel();
    let pool = state.pg().clone();
    let mut reenable = tokio::spawn(async move {
        started_tx.send(()).unwrap();
        let guard = crate::services::challenge_workloads::acquire_runtime_transition_lock(
            &pool,
            challenge_id,
        )
        .await
        .unwrap();
        acquired_tx.send(()).unwrap();
        sqlx::query(r#"UPDATE "GameChallenges" SET is_enabled = TRUE WHERE id = $1"#)
            .bind(challenge_id)
            .execute(&pool)
            .await
            .unwrap();
        guard.release().await.unwrap();
    });
    started_rx.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut acquired_rx)
            .await
            .is_err()
    );
    assert!(!sqlx::query_scalar::<_, bool>(
        r#"SELECT is_enabled FROM "GameChallenges" WHERE id = $1"#,
    )
    .bind(challenge_id)
    .fetch_one(state.pg())
    .await
    .unwrap());
    cleanup.release().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), &mut reenable)
        .await
        .expect("re-enable proceeds after cleanup fence")
        .unwrap();
    assert!(sqlx::query_scalar::<_, bool>(
        r#"SELECT is_enabled FROM "GameChallenges" WHERE id = $1"#,
    )
    .bind(challenge_id)
    .fetch_one(state.pg())
    .await
    .unwrap());
    sqlx::query(r#"UPDATE "GameChallenges" SET is_enabled = FALSE WHERE id = $1"#)
        .bind(challenge_id)
        .execute(state.pg())
        .await
        .unwrap();
}

pub(super) async fn assert_queued_pushback_remote_head(
    state: &SharedState,
    root: &Path,
    binding_id: i32,
    game_id: i32,
    challenge_id: i32,
) {
    let checkout = root.join("repos").join(binding_id.to_string());
    let remote = root.join("pushback-remote.git");
    git_at(
        root,
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            remote.to_str().unwrap(),
        ],
    )
    .await;
    git_at(&checkout, &["init", "--initial-branch=main"]).await;
    git_at(&checkout, &["config", "user.name", "rsctf test"]).await;
    git_at(&checkout, &["config", "user.email", "rsctf@example.test"]).await;
    git_at(&checkout, &["add", "."]).await;
    git_at(&checkout, &["commit", "-m", "baseline"]).await;
    git_at(
        &checkout,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    )
    .await;
    git_at(&checkout, &["push", "-u", "origin", "main"]).await;
    sqlx::query(
        r#"UPDATE "RepoBindings"
              SET push_on_edit = TRUE, github_token = 'test-only'
            WHERE id = $1"#,
    )
    .bind(binding_id)
    .execute(state.pg())
    .await
    .unwrap();

    let checkout_fence = git_sync::lock_checkout_distributed(state.pg(), &checkout)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "GameChallenges" SET title = 'queued stale' WHERE id = $1"#)
        .bind(challenge_id)
        .execute(state.pg())
        .await
        .unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let first_state = state.clone();
    let mut first = tokio::spawn(async move {
        crate::controllers::edit::commit_latest_to_checkout_for_test(
            &first_state,
            game_id,
            challenge_id,
            Some(started_tx),
        )
        .await
    });
    started_rx.await.unwrap();
    sqlx::query(r#"UPDATE "GameChallenges" SET title = 'queued latest' WHERE id = $1"#)
        .bind(challenge_id)
        .execute(state.pg())
        .await
        .unwrap();
    let second_state = state.clone();
    let mut second = tokio::spawn(async move {
        crate::controllers::edit::commit_latest_to_checkout_for_test(
            &second_state,
            game_id,
            challenge_id,
            None,
        )
        .await
    });
    drop(checkout_fence);
    tokio::time::timeout(Duration::from_secs(5), &mut first)
        .await
        .expect("first queued push completes")
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), &mut second)
        .await
        .expect("second queued push completes")
        .unwrap()
        .unwrap();

    let relative = "event/pwn/static/challenge.yaml".to_string();
    let head = git_output(
        root,
        &[
            "--git-dir",
            remote.to_str().unwrap(),
            "show",
            &format!("refs/heads/main:{relative}"),
        ],
    )
    .await;
    assert!(head.contains("name: queued latest"));
    assert!(!head.contains("name: queued stale"));
    assert_eq!(
        sqlx::query_scalar::<_, String>(r#"SELECT title FROM "GameChallenges" WHERE id = $1"#,)
            .bind(challenge_id)
            .fetch_one(state.pg())
            .await
            .unwrap(),
        "queued latest"
    );
    sqlx::query(
        r#"UPDATE "RepoBindings"
              SET push_on_edit = FALSE, github_token = NULL
            WHERE id = $1"#,
    )
    .bind(binding_id)
    .execute(state.pg())
    .await
    .unwrap();
}

pub(super) async fn assert_ad_closeout_tombstone(
    state: &SharedState,
    game_id: i32,
    challenge_id: i32,
    seen_without_challenge: &[i32],
) {
    let round_id = sqlx::query_scalar::<_, i32>(
        r#"INSERT INTO "AdRounds"
             (game_id, number, start_time_utc, end_time_utc, finalized)
           VALUES ($1, 9001, clock_timestamp() - interval '2 minutes',
                   clock_timestamp() - interval '1 minute', FALSE)
           RETURNING id"#,
    )
    .bind(game_id)
    .fetch_one(state.pg())
    .await
    .unwrap();
    let game_lock = crate::services::ad_engine::acquire_ad_game_lock(&state.db, game_id)
        .await
        .unwrap();
    let blocked = tombstone_missing_challenges(state, game_id, seen_without_challenge).await;
    game_lock.release().await.unwrap();
    assert!(matches!(blocked, Err(AppError::Conflict(_))));
    assert!(
        game_challenge::Entity::find_by_id(challenge_id)
            .one(&state.db)
            .await
            .unwrap()
            .unwrap()
            .is_enabled
    );

    sqlx::query(r#"UPDATE "AdRounds" SET finalized = TRUE WHERE id = $1"#)
        .bind(round_id)
        .execute(state.pg())
        .await
        .unwrap();
    let game_lock = crate::services::ad_engine::acquire_ad_game_lock(&state.db, game_id)
        .await
        .unwrap();
    let tombstoned = tombstone_missing_challenges(state, game_id, seen_without_challenge)
        .await
        .unwrap();
    game_lock.release().await.unwrap();
    assert_eq!(tombstoned, vec![challenge_id]);
    assert!(
        !game_challenge::Entity::find_by_id(challenge_id)
            .one(&state.db)
            .await
            .unwrap()
            .unwrap()
            .is_enabled
    );
}

pub(super) async fn assert_definition_contention_is_nonblocking(
    state: &SharedState,
    game_id: i32,
    challenge_id: i32,
    manifest: &Path,
) {
    // A scan takes game -> definition. An interactive definition writer that
    // already owns the inverse edge must cause a quick retry, never a cycle.
    let definition = crate::services::challenge_workloads::acquire_definition_lock(
        state.pg(),
        game_id,
        challenge_id,
    )
    .await
    .unwrap();
    let blocked = tokio::time::timeout(
        Duration::from_secs(1),
        import_with_game_lock(state, game_id, manifest),
    )
    .await
    .expect("definition contention must not block");
    assert!(matches!(blocked, Err(AppError::Conflict(_))));
    definition.release().await.unwrap();
}

pub(super) async fn assert_pending_deletion_rejects_repository_mutation(
    state: &SharedState,
    game_id: i32,
    challenge_id: i32,
    manifest: &Path,
    seen_without_challenge: &[i32],
) {
    let original_manifest = tokio::fs::read_to_string(manifest).await.unwrap();
    let original_title: String =
        sqlx::query_scalar(r#"SELECT title FROM "GameChallenges" WHERE id = $1"#)
            .bind(challenge_id)
            .fetch_one(state.pg())
            .await
            .unwrap();
    let mut replaced_name = false;
    let mut mutated = original_manifest
        .lines()
        .map(|line| {
            if !replaced_name && line.trim_start().starts_with("name:") {
                replaced_name = true;
                "name: deletion pending overwrite".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    mutated.push('\n');
    assert!(replaced_name);
    tokio::fs::write(manifest, mutated).await.unwrap();
    sqlx::query(
        r#"UPDATE "GameChallenges"
              SET deletion_pending = TRUE, is_enabled = FALSE
            WHERE id = $1"#,
    )
    .bind(challenge_id)
    .execute(state.pg())
    .await
    .unwrap();

    let import = import_with_game_lock(state, game_id, manifest).await;
    assert!(matches!(import, Err(AppError::Conflict(_))));
    let state_after_import: (String, bool, bool) = sqlx::query_as(
        r#"SELECT title, is_enabled, deletion_pending
             FROM "GameChallenges" WHERE id = $1"#,
    )
    .bind(challenge_id)
    .fetch_one(state.pg())
    .await
    .unwrap();
    assert_eq!(state_after_import, (original_title, false, true));

    let game_lock = crate::services::ad_engine::acquire_ad_game_lock(&state.db, game_id)
        .await
        .unwrap();
    let tombstone = tombstone_missing_challenges(state, game_id, seen_without_challenge).await;
    game_lock.release().await.unwrap();
    assert!(matches!(tombstone, Err(AppError::Conflict(_))));
    assert!(sqlx::query_scalar::<_, bool>(
        r#"SELECT deletion_pending FROM "GameChallenges" WHERE id = $1"#,
    )
    .bind(challenge_id)
    .fetch_one(state.pg())
    .await
    .unwrap());

    sqlx::query(r#"UPDATE "GameChallenges" SET deletion_pending = FALSE WHERE id = $1"#)
        .bind(challenge_id)
        .execute(state.pg())
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "Games" SET deletion_pending = TRUE WHERE id = $1"#)
        .bind(game_id)
        .execute(state.pg())
        .await
        .unwrap();
    assert!(matches!(
        import_with_game_lock(state, game_id, manifest).await,
        Err(AppError::Conflict(_))
    ));
    let game_lock = crate::services::ad_engine::acquire_ad_game_lock(&state.db, game_id)
        .await
        .unwrap();
    let tombstone = tombstone_missing_challenges(state, game_id, seen_without_challenge).await;
    game_lock.release().await.unwrap();
    assert!(matches!(tombstone, Err(AppError::Conflict(_))));
    sqlx::query(r#"UPDATE "Games" SET deletion_pending = FALSE WHERE id = $1"#)
        .bind(game_id)
        .execute(state.pg())
        .await
        .unwrap();
    tokio::fs::write(manifest, original_manifest).await.unwrap();
}

async fn git_at(cwd: &Path, args: &[&str]) {
    let output = tokio::process::Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn git_output(cwd: &Path, args: &[&str]) -> String {
    let output = tokio::process::Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

pub(super) async fn assert_binding_update_and_delete_fences(
    state: &SharedState,
    root: &Path,
    binding_id: i32,
    game_id: i32,
    challenge_id: i32,
) {
    let checkout = root.join("repos").join(binding_id.to_string());
    let scan_fence = git_sync::lock_checkout_distributed(state.pg(), &checkout)
        .await
        .unwrap();
    let update_state = state.clone();
    let mut update = tokio::spawn(async move {
        crate::controllers::admin::update_repo_binding_record(
            &update_state,
            binding_id,
            crate::controllers::admin::RepoBindingUpdateModel {
                r#ref: Some("release/latest".to_string()),
                interval_seconds: Some(17),
                status: Some("Paused".to_string()),
                github_token: Some("new-token".to_string()),
                push_on_edit: Some(true),
            },
        )
        .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut update)
            .await
            .is_err()
    );
    let scan_time = Utc::now();
    crate::controllers::admin::record_scan_completion(
        state,
        binding_id,
        scan_time,
        Some("scan-commit".to_string()),
        "scan complete".to_string(),
        scan_time + chrono::Duration::seconds(60),
    )
    .await
    .unwrap();
    drop(scan_fence);
    let updated = tokio::time::timeout(Duration::from_secs(3), &mut update)
        .await
        .expect("binding update proceeds after scan")
        .unwrap()
        .unwrap();
    assert_eq!(updated.git_ref.as_deref(), Some("release/latest"));
    assert_eq!(updated.github_token.as_deref(), Some("new-token"));
    assert_eq!(updated.status, RepoWatchStatus::Paused);
    assert!(updated.push_on_edit);
    assert_eq!(updated.last_commit_sha.as_deref(), Some("scan-commit"));

    sqlx::query(
        r#"INSERT INTO "RepoBindingScans"
             (binding_id, ran_at_utc, games_created, games_updated,
              challenges_imported, challenges_updated, failures)
           VALUES ($1, clock_timestamp(), 0, 0, 0, 0, 0)"#,
    )
    .bind(binding_id)
    .execute(state.pg())
    .await
    .unwrap();
    let delete_fence = git_sync::lock_checkout_distributed(state.pg(), &checkout)
        .await
        .unwrap();
    let delete_state = state.clone();
    let mut delete = tokio::spawn(async move {
        crate::controllers::admin::delete_repo_binding_record(&delete_state, binding_id).await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut delete)
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, Option<i32>>(
            r#"SELECT repo_binding_id FROM "Games" WHERE id = $1"#,
        )
        .bind(game_id)
        .fetch_one(state.pg())
        .await
        .unwrap(),
        Some(binding_id)
    );
    drop(delete_fence);
    assert!(tokio::time::timeout(Duration::from_secs(3), &mut delete)
        .await
        .expect("binding delete proceeds after scan")
        .unwrap()
        .unwrap());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "RepoBindings" WHERE id = $1"#)
            .bind(binding_id)
            .fetch_one(state.pg())
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*) FROM "RepoBindingScans" WHERE binding_id = $1"#,
        )
        .bind(binding_id)
        .fetch_one(state.pg())
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, Option<i32>>(
            r#"SELECT repo_binding_id FROM "Games" WHERE id = $1"#,
        )
        .bind(game_id)
        .fetch_one(state.pg())
        .await
        .unwrap(),
        None
    );
    assert!(sqlx::query_scalar::<_, Option<String>>(
        r#"SELECT source_yaml_path FROM "GameChallenges" WHERE id = $1"#,
    )
    .bind(challenge_id)
    .fetch_one(state.pg())
    .await
    .unwrap()
    .is_some());
}
