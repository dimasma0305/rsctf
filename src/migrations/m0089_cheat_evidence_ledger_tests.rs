use std::str::FromStr;

use sea_orm_migration::sea_orm::{ConnectionTrait, SqlxPostgresConnector};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::UP_SQL;
use crate::migrations::{test_process_application_name, Migrator, MigratorTrait};

#[test]
fn ledger_contract_is_immutable_consistent_and_replayable() {
    assert!(UP_SQL.contains("ux_cheatinfo_submission_id"));
    assert!(UP_SQL.contains("fk_cheatinfo_submission_provenance"));
    assert!(UP_SQL.contains("fk_cheatinfo_source_participation"));
    assert!(UP_SQL.contains("trg_cheatinfo_immutable"));
    assert!(UP_SQL.contains("SuspicionEvaluationOutbox"));
    assert!(UP_SQL.contains("FOR EACH ROW EXECUTE FUNCTION"));
    assert!(UP_SQL.contains("ux_suspicion_outbox_source"));
    assert!(UP_SQL.contains("source_kind = 2 AND rule_kind = 33"));
    assert!(!UP_SQL.contains("source_kind IN (1, 2)"));
    assert!(UP_SQL.contains("SET accepted_count ="));
    assert!(UP_SQL.contains("submission.status = 1"));
    assert!(UP_SQL.contains("trg_games_seed_suspicion_reconciliation"));
    assert!(UP_SQL.contains("FOR SHARE;"));
    assert!(
        !UP_SQL.contains("submission.answer")
            || !UP_SQL.contains("evidence_payload = submission.answer")
    );
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn upgrades_the_real_schema_and_is_idempotent() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let application_name = test_process_application_name();
    let admin_options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .application_name(application_name);
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(admin_options)
        .await
        .unwrap();
    let schema = format!("rsctf_m0089_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .application_name(application_name)
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .unwrap();
    let db = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());

    Migrator::up(&db, Some(88)).await.unwrap();
    // Strip entity-derived m0089 fields to emulate the last shipped schema.
    db.execute_unprepared(
        r#"
        ALTER TABLE "CheatInfo"
          DROP COLUMN IF EXISTS submit_participation_id,
          DROP COLUMN IF EXISTS source_participation_id,
          DROP COLUMN IF EXISTS challenge_id,
          DROP COLUMN IF EXISTS evidence_key,
          DROP COLUMN IF EXISTS observed_at_utc,
          DROP COLUMN IF EXISTS evidence_payload,
          DROP COLUMN IF EXISTS evidence_version;
        ALTER TABLE "Submissions"
          DROP COLUMN IF EXISTS submit_remote_ip_hash,
          DROP COLUMN IF EXISTS container_id,
          DROP COLUMN IF EXISTS container_last_operation_at_submit,
          DROP COLUMN IF EXISTS container_was_loaded_at_submit,
          DROP COLUMN IF EXISTS first_open_at_submit,
          DROP COLUMN IF EXISTS first_download_at_submit,
          DROP COLUMN IF EXISTS first_container_start_at_submit;
        ALTER TABLE "Participations"
          DROP COLUMN IF EXISTS competitive_admitted_at_utc;
        ALTER TABLE "ContainerAccessEvents"
          DROP COLUMN IF EXISTS remote_ip_hash,
          DROP COLUMN IF EXISTS is_monitor;
        DROP TABLE IF EXISTS "SuspicionEvaluationOutbox";
        DROP TABLE IF EXISTS "SuspicionReconciliationState";
        "#,
    )
    .await
    .unwrap();

    // Corrupt legacy provenance must roll back intact until explicitly repaired.
    sqlx::raw_sql(
        r#"
        INSERT INTO "AspNetUsers"
          (id, user_name, email_confirmed, phone_number_confirmed,
           two_factor_enabled, lockout_enabled, access_failed_count,
           role, ip, last_signed_in_utc, last_visited_utc,
           register_time_utc, bio, real_name, std_number,
           exercise_visible)
        VALUES
          ('00000000-0000-0000-0000-000000000009', 'repair-user',
           FALSE, FALSE, FALSE, FALSE, 0, 0, '', clock_timestamp(),
           clock_timestamp(), clock_timestamp(), '', '', '', FALSE);
        INSERT INTO "Teams" (id, name, locked, invite_token, captain_id)
        VALUES
          (90, 'repair-submit', FALSE, 'repair-submit-token',
           '00000000-0000-0000-0000-000000000009'),
          (91, 'repair-source', FALSE, 'repair-source-token',
           '00000000-0000-0000-0000-000000000009');
        INSERT INTO "Games"
          (id, title, public_key, private_key, hidden, practice_mode,
           summary, content, accept_without_review,
           allow_user_submissions, writeup_required,
           team_member_count_limit, container_count_limit,
           start_time_utc, end_time_utc, writeup_deadline,
           writeup_note, blood_bonus_value, ad_allow_snapshot_download,
           ad_scoring_paused, ad_epoch_ticks, koth_epoch_ticks,
           koth_cycle_ticks, koth_champion_cooldown_ticks,
           koth_claim_confirmation_ticks)
        VALUES
          (9, 'repair-game', 'repair-public', 'repair-private', FALSE, FALSE,
           '', '', FALSE, FALSE, FALSE, 4, 4,
           clock_timestamp() - INTERVAL '1 hour',
           clock_timestamp() + INTERVAL '1 hour',
           clock_timestamp() + INTERVAL '2 hours', '', 0, FALSE,
           FALSE, 8, 12, 3, 1, 2);
        INSERT INTO "Participations"
          (id, status, token, game_id, team_id, suspicion_score)
        VALUES
          (920, 2, 'repair-submit-participation', 9, 90, 0),
          (921, 1, 'repair-source-participation', 9, 91, 0);
        INSERT INTO "GameChallenges"
          (id, game_id, title, content, category, "Type", is_enabled,
           submission_limit, accepted_count, submission_count,
           review_status, build_status, enable_traffic_capture,
           enable_shared_container, disable_blood_bonus, original_score,
           min_score_rate, difficulty, score_curve, ad_allow_egress,
           ad_allow_self_reset, ad_ssh_requires_flag, ad_self_hosted)
        VALUES
          (930, 9, 'repair-challenge', '', 0, 2, TRUE, 0, 0, 0, 0, 0,
           FALSE, FALSE, FALSE, 100, 0.2, 1.0, 0, FALSE, FALSE,
           FALSE, FALSE);
        INSERT INTO "Submissions"
          (id, answer, status, submit_time_utc, user_id, team_id,
           participation_id, game_id, challenge_id)
        VALUES
          (940, 'flag{not-cheat}', 1,
           clock_timestamp() - INTERVAL '10 minutes',
           '00000000-0000-0000-0000-000000000009', 90, 920, 9, 930);
        INSERT INTO "FirstSolves"
          (participation_id, challenge_id, submission_id)
        VALUES (920, 930, 940);
        INSERT INTO "CheatInfo"
          (game_id, submit_team_id, source_team_id, submission_id)
        VALUES (9, 90, 91, 940);
        INSERT INTO "ContainerAccessEvents"
          (id, game_id, challenge_id, container_owner_participation_id,
           container_id, accessing_user_id, accessing_user_name,
           accessing_participation_id, remote_ip, user_agent,
           connected_at_utc)
        VALUES
          (950, 9, 930, 921,
           '00000000-0000-0000-0000-000000000950',
           '00000000-0000-0000-0000-000000000009', 'repair-user',
           920, '192.0.2.9', 'legacy-agent', clock_timestamp());
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let migration_error = db
        .execute_unprepared(UP_SQL)
        .await
        .expect_err("non-cheat legacy provenance must fail closed");
    assert!(migration_error
        .to_string()
        .contains("without a CheatDetected submission"));
    let preserved: (i16, i64) = sqlx::query_as(
        r#"SELECT submission.status, COUNT(cheat.id)::bigint
             FROM "Submissions" submission
             JOIN "CheatInfo" cheat ON cheat.submission_id = submission.id
            WHERE submission.id = 940
            GROUP BY submission.status"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(preserved, (1, 1));
    sqlx::query(r#"UPDATE "Submissions" SET status = 3 WHERE id = 940"#)
        .execute(&pool)
        .await
        .unwrap();
    let malformed_solve_error = db
        .execute_unprepared(UP_SQL)
        .await
        .expect_err("malformed legacy FirstSolves must fail closed");
    assert!(malformed_solve_error
        .to_string()
        .contains("cannot freeze malformed FirstSolves provenance"));
    let malformed_solve_preserved: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint FROM "FirstSolves"
            WHERE participation_id = 920 AND challenge_id = 930
              AND submission_id = 940"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(malformed_solve_preserved, 1);
    sqlx::query(
        r#"DELETE FROM "FirstSolves"
            WHERE participation_id = 920 AND challenge_id = 930"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    // SeaORM's `steps` is a count, not a target migration number. Apply only
    // m0089 here so the pre-m0091 event below is genuinely quarantined later.
    Migrator::up(&db, Some(1)).await.unwrap();
    db.execute_unprepared(UP_SQL).await.unwrap();
    let rejected_cohort_is_submission_time: bool = sqlx::query_scalar(
        r#"SELECT participation.competitive_admitted_at_utc
                         IS NOT DISTINCT FROM submission.submit_time_utc
                 FROM "Participations" participation
                 JOIN "Submissions" submission ON submission.participation_id = participation.id
                WHERE participation.id = 920 AND submission.id = 940"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(rejected_cohort_is_submission_time);
    let legacy_replay_jobs: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint
             FROM "SuspicionEvaluationOutbox"
            WHERE source_kind = 0 AND source_id = 940
              AND game_id = 9 AND participation_id = 920
              AND challenge_id = 930 AND rule_kind IS NULL
              AND evidence_key = 'submission:940'"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(legacy_replay_jobs, 1);
    let legacy_container_jobs: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint
             FROM "SuspicionEvaluationOutbox"
            WHERE source_kind = 2 AND source_id = 950
              AND game_id = 9 AND participation_id = 920
              AND challenge_id = 930 AND rule_kind = 33
              AND evidence_key = 'challenge:930'"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        legacy_container_jobs, 0,
        "legacy cross-owner access may be an unrecorded monitor session"
    );
    let raw_only_honeypot = sqlx::query(
        r#"INSERT INTO "SuspicionEvaluationOutbox"
             (job_kind, source_kind, source_id, game_id, participation_id,
              challenge_id, rule_kind, evidence_key, observed_at_utc,
              evidence_payload, evidence_version)
           VALUES (1, 1, 950, 9, 920, NULL, 28, 'raw-only',
                   clock_timestamp(), '{}'::jsonb, 1)"#,
    )
    .execute(&pool)
    .await
    .expect_err("raw-only honeypot telemetry cannot enter the score queue");
    assert!(raw_only_honeypot
        .to_string()
        .contains("ck_suspicion_outbox_kind"));

    // m0091 deliberately quarantines every pre-cutover detector row. The m89
    // job seeded from immutable CheatInfo must then recreate the canonical hard
    // event once, without restoring the untrusted row's contribution.
    sqlx::query(
        r#"INSERT INTO "SuspicionEvents"
             (game_id, participation_id, challenge_id, kind, evidence_key,
              score_delta, created_at)
           VALUES
             (9, 920, 930, 0, 'submission:940', 100, clock_timestamp()),
             (9, 920, 930, 33, 'challenge:930', 120, clock_timestamp())"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    Migrator::up(&db, Some(2)).await.unwrap();
    assert_eq!(
        crate::services::suspicion::reconcile_evaluation_outbox(&db, 8)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        crate::services::suspicion::reconcile_evaluation_outbox(&db, 8)
            .await
            .unwrap(),
        0
    );
    let restored: (i64, i64, i32) = sqlx::query_as(
        r#"SELECT
             COUNT(*) FILTER (
               WHERE evidence_key = 'submission:940' AND score_delta > 0
             )::bigint,
             COUNT(*) FILTER (
               WHERE evidence_key LIKE 'legacy-untrusted:%' AND score_delta = 0
             )::bigint,
             (SELECT suspicion_score FROM "Participations" WHERE id = 920)
           FROM "SuspicionEvents"
          WHERE game_id = 9 AND participation_id = 920 AND kind = 0"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(restored.0, 1);
    assert_eq!(restored.1, 1);
    assert!(restored.2 > 0);
    let replay_state: (bool, Option<String>) = sqlx::query_as(
        r#"SELECT completed_at_utc IS NOT NULL, last_error
             FROM "SuspicionEvaluationOutbox" WHERE source_kind = 0 AND source_id = 940"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(replay_state, (true, None));
    sqlx::query(
        r#"UPDATE "Games" SET end_time_utc =
             (SELECT submit_time_utc + INTERVAL '1 minute' FROM "Submissions" WHERE id = 940)
           WHERE id = 9"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        crate::services::suspicion::seal_reconciled_game_for_test(&pool, 9, 1)
            .await
            .unwrap()
    );
    let sealed: bool = sqlx::query_scalar(
        r#"SELECT sealed_at_utc IS NOT NULL
             FROM "SuspicionReconciliationState" WHERE game_id = 9"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(sealed);
    let ambiguous_container: (i64, i64, i64) = sqlx::query_as(
        r#"SELECT
             (SELECT COUNT(*)::bigint FROM "ContainerAccessEvents" WHERE id = 950),
             COUNT(*) FILTER (
               WHERE evidence_key = 'challenge:930' AND score_delta > 0
             )::bigint,
             COUNT(*) FILTER (
               WHERE evidence_key LIKE 'legacy-untrusted:%' AND score_delta = 0
             )::bigint
           FROM "SuspicionEvents"
          WHERE game_id = 9 AND participation_id = 920 AND kind = 33"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(ambiguous_container, (1, 0, 1));
    let columns: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint
             FROM information_schema.columns
            WHERE table_schema = current_schema()
              AND table_name = 'CheatInfo'
              AND column_name = ANY($1)"#,
    )
    .bind([
        "submit_participation_id",
        "source_participation_id",
        "challenge_id",
        "evidence_key",
        "observed_at_utc",
        "evidence_payload",
        "evidence_version",
    ])
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(columns, 7);
    let outbox_exists: bool =
        sqlx::query_scalar(r#"SELECT to_regclass('"SuspicionEvaluationOutbox"') IS NOT NULL"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(outbox_exists);

    // Compatibility triggers canonicalize a previous binary's legacy insert
    // and create its durable evaluation in the same transaction.
    sqlx::raw_sql(
        r#"
        INSERT INTO "AspNetUsers"
          (id, user_name, email_confirmed, phone_number_confirmed,
           two_factor_enabled, lockout_enabled, access_failed_count,
           role, ip, last_signed_in_utc, last_visited_utc,
           register_time_utc, bio, real_name, std_number,
           exercise_visible)
        VALUES
          ('00000000-0000-0000-0000-000000000001', 'legacy-user',
           FALSE, FALSE, FALSE, FALSE, 0, 0, '', clock_timestamp(),
           clock_timestamp(), clock_timestamp(), '', '', '', FALSE);
        INSERT INTO "Teams"
          (id, name, locked, invite_token, captain_id)
        VALUES
          (10, 'submit-team', FALSE, 'submit-token',
           '00000000-0000-0000-0000-000000000001'),
          (11, 'source-team', FALSE, 'source-token',
           '00000000-0000-0000-0000-000000000001');
        INSERT INTO "Games"
          (id, title, public_key, private_key, hidden, practice_mode,
           summary, content, accept_without_review,
           allow_user_submissions, writeup_required,
           team_member_count_limit, container_count_limit,
           start_time_utc, end_time_utc, writeup_deadline,
           writeup_note, blood_bonus_value, ad_allow_snapshot_download,
           ad_scoring_paused, ad_epoch_ticks, koth_epoch_ticks,
           koth_cycle_ticks, koth_champion_cooldown_ticks,
           koth_claim_confirmation_ticks)
        VALUES
          (1, 'game', 'public', 'private', FALSE, FALSE, '', '', FALSE,
           FALSE, FALSE, 4, 4, clock_timestamp() - INTERVAL '1 hour',
           clock_timestamp() + INTERVAL '1 hour',
           clock_timestamp() + INTERVAL '2 hours', '', 0, FALSE,
           FALSE, 8, 12, 3, 1, 2);
        INSERT INTO "Participations"
          (id, status, token, game_id, team_id, suspicion_score)
        VALUES
          (20, 1, 'submit-participation', 1, 10, 0),
          (21, 1, 'source-participation', 1, 11, 0);
        INSERT INTO "GameChallenges"
          (id, game_id, title, content, category, "Type", is_enabled,
           submission_limit, accepted_count, submission_count,
           review_status, build_status, enable_traffic_capture,
           enable_shared_container, disable_blood_bonus, original_score,
           min_score_rate, difficulty, score_curve, ad_allow_egress,
           ad_allow_self_reset, ad_ssh_requires_flag, ad_self_hosted)
        VALUES
          (30, 1, 'challenge', '', 0, 2, TRUE, 0, 0, 0, 0, 0,
           FALSE, FALSE, FALSE, 100, 0.2, 1.0, 0, FALSE, FALSE,
           FALSE, FALSE);
        INSERT INTO "Submissions"
          (id, answer, status, submit_time_utc, user_id, team_id,
           participation_id, game_id, challenge_id)
        VALUES
          (40, 'flag{legacy}', 3, clock_timestamp(),
           '00000000-0000-0000-0000-000000000001', 10, 20, 1, 30);
        INSERT INTO "CheatInfo"
          (game_id, submit_team_id, source_team_id, submission_id,
           evidence_payload)
        VALUES
          (1, 10, 11, 40,
           '{"challengeTitle":"forged","submitTeamName":"forged","sourceTeamName":"forged","submitUserName":"forged"}'::jsonb);
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let canonical: (i32, i32, i32, String, i16, String, String, String, String) = sqlx::query_as(
        r#"SELECT submit_participation_id, source_participation_id,
                  challenge_id, evidence_key, evidence_version,
                  evidence_payload ->> 'submitTeamName',
                  evidence_payload ->> 'sourceTeamName',
                  evidence_payload ->> 'challengeTitle',
                  evidence_payload ->> 'submitUserName'
             FROM "CheatInfo" WHERE submission_id = 40"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        canonical,
        (
            20,
            21,
            30,
            "submission:40".to_owned(),
            1,
            "submit-team".to_owned(),
            "source-team".to_owned(),
            "challenge".to_owned(),
            "legacy-user".to_owned(),
        )
    );
    sqlx::raw_sql(
        r#"
        UPDATE "Teams" SET name = 'renamed-submit-team' WHERE id = 10;
        UPDATE "Teams" SET name = 'renamed-source-team' WHERE id = 11;
        UPDATE "GameChallenges" SET title = 'renamed-challenge' WHERE id = 30;
        UPDATE "AspNetUsers" SET user_name = 'renamed-user'
          WHERE id = '00000000-0000-0000-0000-000000000001';
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let frozen_names: (String, String, String, String) = sqlx::query_as(
        r#"SELECT evidence_payload ->> 'submitTeamName',
                  evidence_payload ->> 'sourceTeamName',
                  evidence_payload ->> 'challengeTitle',
                  evidence_payload ->> 'submitUserName'
             FROM "CheatInfo" WHERE submission_id = 40"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        frozen_names,
        (
            "submit-team".to_owned(),
            "source-team".to_owned(),
            "challenge".to_owned(),
            "legacy-user".to_owned(),
        )
    );
    let submission_mutation = sqlx::query(
        r#"UPDATE "Submissions"
              SET answer = 'forged-answer', status = 1,
                  submit_time_utc = submit_time_utc + INTERVAL '1 second'
            WHERE id = 40"#,
    )
    .execute(&pool)
    .await
    .expect_err("CheatInfo-referenced submission is immutable");
    assert!(submission_mutation
        .to_string()
        .contains("submission evidence is immutable"));
    let submission_delete = sqlx::query(r#"DELETE FROM "Submissions" WHERE id = 40"#)
        .execute(&pool)
        .await
        .expect_err("CheatInfo-referenced submission cannot be deleted");
    assert!(submission_delete
        .to_string()
        .contains("submission evidence is immutable"));
    let durable_evaluations: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint
             FROM "SuspicionEvaluationOutbox"
            WHERE source_kind = 0 AND source_id = 40
              AND game_id = 1 AND participation_id = 20
              AND challenge_id = 30 AND rule_kind IS NULL
              AND evidence_key = 'submission:40'
              AND observed_at_utc = (
                    SELECT submit_time_utc FROM "Submissions" WHERE id = 40
                  )"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(durable_evaluations, 1);

    let invalid_first_solve = sqlx::query(
        r#"INSERT INTO "FirstSolves"
             (participation_id, challenge_id, submission_id)
           VALUES (20, 30, 40)"#,
    )
    .execute(&pool)
    .await
    .expect_err("a CheatDetected submission cannot become a canonical solve");
    assert!(invalid_first_solve
        .to_string()
        .contains("FirstSolves requires an Accepted submission tuple"));
    sqlx::query(
        r#"INSERT INTO "Submissions"
             (id, answer, status, submit_time_utc, user_id, team_id,
              participation_id, game_id, challenge_id)
           VALUES
             (41, 'flag{accepted}', 1, clock_timestamp(),
              '00000000-0000-0000-0000-000000000001', 10, 20, 1, 30)"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "FirstSolves"
             (participation_id, challenge_id, submission_id)
           VALUES (20, 30, 41)"#,
    )
    .execute(&pool)
    .await
    .expect("an exact Accepted tuple remains a legitimate FirstSolve insert");
    let first_solve_update = sqlx::query(
        r#"UPDATE "FirstSolves" SET submission_id = submission_id
            WHERE participation_id = 20 AND challenge_id = 30"#,
    )
    .execute(&pool)
    .await
    .expect_err("FirstSolves rows are append-only");
    assert!(first_solve_update
        .to_string()
        .contains("evidence rows are append-only"));
    let first_solve_delete = sqlx::query(
        r#"DELETE FROM "FirstSolves"
            WHERE participation_id = 20 AND challenge_id = 30"#,
    )
    .execute(&pool)
    .await
    .expect_err("FirstSolves rows cannot be deleted");
    assert!(first_solve_delete
        .to_string()
        .contains("evidence rows are append-only"));

    let access_update = sqlx::query(
        r#"UPDATE "ContainerAccessEvents" SET user_agent = 'rewritten'
            WHERE id = 950"#,
    )
    .execute(&pool)
    .await
    .expect_err("container access evidence is append-only");
    assert!(access_update
        .to_string()
        .contains("evidence rows are append-only"));
    let access_delete = sqlx::query(r#"DELETE FROM "ContainerAccessEvents" WHERE id = 950"#)
        .execute(&pool)
        .await
        .expect_err("container access evidence cannot be deleted");
    assert!(access_delete
        .to_string()
        .contains("evidence rows are append-only"));

    let operational_update = sqlx::query(
        r#"UPDATE "SuspicionEvaluationOutbox"
              SET available_at_utc = clock_timestamp(), attempts = attempts + 1,
                  lease_token = NULL, lease_expires_at_utc = NULL,
                  completed_at_utc = NULL, last_error = 'retryable'
            WHERE source_kind = 0 AND source_id = 40"#,
    )
    .execute(&pool)
    .await
    .expect("lease/retry/completion bookkeeping remains mutable");
    assert_eq!(operational_update.rows_affected(), 1);
    let outbox_identity_update = sqlx::query(
        r#"UPDATE "SuspicionEvaluationOutbox"
              SET evidence_key = 'forged'
            WHERE source_kind = 0 AND source_id = 40"#,
    )
    .execute(&pool)
    .await
    .expect_err("durable job identity is immutable");
    assert!(outbox_identity_update
        .to_string()
        .contains("suspicion evaluation identity is immutable"));
    let outbox_delete = sqlx::query(
        r#"DELETE FROM "SuspicionEvaluationOutbox"
            WHERE source_kind = 0 AND source_id = 40"#,
    )
    .execute(&pool)
    .await
    .expect_err("durable jobs cannot be silently erased");
    assert!(outbox_delete
        .to_string()
        .contains("suspicion evaluation jobs cannot be deleted"));

    let competitive_admission: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        r#"SELECT competitive_admitted_at_utc
             FROM "Participations" WHERE id = 20"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(competitive_admission.is_some());

    // Once the configured competition is over, neither a direct Accepted
    // practice join nor a later Pending->Accepted admin transition can enter
    // the immutable final cohort.
    sqlx::raw_sql(
        r#"
        UPDATE "Games"
           SET end_time_utc = clock_timestamp() - INTERVAL '1 second'
         WHERE id = 1;
        INSERT INTO "Teams" (id, name, locked, invite_token, captain_id)
        VALUES
          (12, 'post-end-direct', FALSE, 'post-end-direct-token',
           '00000000-0000-0000-0000-000000000001'),
          (13, 'post-end-transition', FALSE, 'post-end-transition-token',
           '00000000-0000-0000-0000-000000000001');
        INSERT INTO "Participations"
          (id, status, token, game_id, team_id, suspicion_score)
        VALUES
          (22, 1, 'post-end-direct-participation', 1, 12, 0),
          (23, 0, 'post-end-transition-participation', 1, 13, 0);
        UPDATE "Participations" SET status = 1 WHERE id = 23;
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let post_end_cohort_rows: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint FROM "Participations"
            WHERE id IN (22, 23) AND competitive_admitted_at_utc IS NOT NULL"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(post_end_cohort_rows, 0);
    let mutation_error = sqlx::query(
        r#"UPDATE "Participations"
              SET competitive_admitted_at_utc = clock_timestamp()
            WHERE id = 20"#,
    )
    .execute(&pool)
    .await
    .expect_err("competitive cohort admission is immutable");
    assert!(mutation_error.to_string().contains("immutable"));

    // A queued writer must observe durable closure even if game end moves ahead.
    sqlx::query(
        r#"INSERT INTO "Teams" (id, name, locked, invite_token, captain_id)
           VALUES (17, 'queued-after-close', FALSE, 'queued-after-close-token',
                   '00000000-0000-0000-0000-000000000001')"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut closure = pool.begin().await.unwrap();
    sqlx::query(
        r#"UPDATE "Games"
              SET end_time_utc = clock_timestamp() + INTERVAL '1 hour'
            WHERE id = 1"#,
    )
    .execute(&mut *closure)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "SuspicionReconciliationState"
               (game_id, evidence_closed_at_utc, attempts)
           VALUES (1, clock_timestamp(), 0)
           ON CONFLICT (game_id) DO UPDATE
             SET evidence_closed_at_utc = EXCLUDED.evidence_closed_at_utc"#,
    )
    .execute(&mut *closure)
    .await
    .unwrap();
    let writer_pool = pool.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let queued_writer = async move {
        let mut transaction = writer_pool.begin().await.unwrap();
        started_tx.send(()).unwrap();
        sqlx::query(
            r#"INSERT INTO "Participations"
                 (id, status, token, game_id, team_id, suspicion_score)
               VALUES (27, 1, 'queued-after-close-participation', 1, 17, 0)"#,
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        transaction.commit().await.unwrap();
    };
    let release_closure = async move {
        started_rx.await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        closure.commit().await.unwrap();
    };
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        tokio::join!(queued_writer, release_closure)
    })
    .await
    .expect("queued cohort writer resumes after durable closure");
    let queued_admission: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        r#"SELECT competitive_admitted_at_utc
             FROM "Participations" WHERE id = 27"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(queued_admission.is_none());
    sqlx::query(
        r#"UPDATE "Games"
              SET end_time_utc = clock_timestamp() - INTERVAL '1 second'
            WHERE id = 1"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    // Exercise the ended-game legacy backfill on an idempotent rerun. Current
    // Rejected status cannot erase a proven competitor: exact start is
    // included, while pre-start, exact-end, and post-end rows remain excluded.
    sqlx::raw_sql(
        r#"
        INSERT INTO "Teams" (id, name, locked, invite_token, captain_id)
        VALUES
          (14, 'pre-start', FALSE, 'pre-start-token',
           '00000000-0000-0000-0000-000000000001'),
          (15, 'at-start', FALSE, 'at-start-token',
           '00000000-0000-0000-0000-000000000001'),
          (16, 'at-end', FALSE, 'at-end-token',
           '00000000-0000-0000-0000-000000000001');
        INSERT INTO "Participations"
          (id, status, token, game_id, team_id, suspicion_score)
        VALUES
          (24, 2, 'pre-start-participation', 1, 14, 0),
          (25, 2, 'at-start-participation', 1, 15, 0),
          (26, 2, 'at-end-participation', 1, 16, 0);
        INSERT INTO "Submissions"
          (id, answer, status, submit_time_utc, team_id,
           participation_id, game_id, challenge_id)
        SELECT 42, 'pre-start', 2,
               game.start_time_utc - INTERVAL '1 microsecond',
               14, 24, 1, 30
          FROM "Games" game WHERE game.id = 1
        UNION ALL
        SELECT 43, 'at-start', 2, game.start_time_utc,
               15, 25, 1, 30
          FROM "Games" game WHERE game.id = 1
        UNION ALL
        SELECT 44, 'at-end', 2, game.end_time_utc,
               16, 26, 1, 30
          FROM "Games" game WHERE game.id = 1
        UNION ALL
        SELECT 45, 'post-end', 2,
               game.end_time_utc + INTERVAL '1 microsecond',
               14, 24, 1, 30
          FROM "Games" game WHERE game.id = 1;
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let ordinary_update =
        sqlx::query(r#"UPDATE "Submissions" SET answer = 'pre-start-edited' WHERE id = 42"#)
            .execute(&pool)
            .await
            .expect_err("all graded submission evidence is immutable");
    assert!(ordinary_update
        .to_string()
        .contains("submission evidence is immutable"));
    let unchanged_answer: String =
        sqlx::query_scalar(r#"SELECT answer FROM "Submissions" WHERE id = 42"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(unchanged_answer, "pre-start");
    db.execute_unprepared(UP_SQL).await.unwrap();
    let backfilled_cohort: Vec<(i32, bool)> = sqlx::query_as(
        r#"SELECT id, competitive_admitted_at_utc IS NOT NULL
             FROM "Participations" WHERE id IN (24, 25, 26) ORDER BY id"#,
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        backfilled_cohort,
        vec![(24, false), (25, true), (26, false)]
    );

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn online_upgrade_drains_old_submit_before_locking_children() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let application_name = test_process_application_name();
    let admin_options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .application_name(application_name);
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(admin_options)
        .await
        .unwrap();
    let schema = format!("rsctf_m0089_lock_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .application_name(application_name)
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TABLE "Games" (id INTEGER PRIMARY KEY);
        CREATE TABLE "Teams" (id INTEGER PRIMARY KEY);
        CREATE TABLE "AspNetUsers" (id INTEGER PRIMARY KEY);
        CREATE TABLE "Participations" (id INTEGER PRIMARY KEY);
        CREATE TABLE "GameChallenges" (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
        CREATE TABLE "GameInstances" (id INTEGER PRIMARY KEY);
        CREATE TABLE "FlagContexts" (id INTEGER PRIMARY KEY);
        CREATE TABLE "GameEvents" (id INTEGER PRIMARY KEY);
        CREATE TABLE "Submissions" (id INTEGER PRIMARY KEY);
        CREATE TABLE "FirstSolves" (id INTEGER PRIMARY KEY);
        CREATE TABLE "CheatInfo" (id INTEGER PRIMARY KEY);
        CREATE TABLE "ContainerAccessEvents" (id INTEGER PRIMARY KEY);
        CREATE TABLE "HoneypotHits" (id INTEGER PRIMARY KEY);
        CREATE TABLE "SuspicionEvents" (id INTEGER PRIMARY KEY);
        INSERT INTO "Games" (id) VALUES (1);
        INSERT INTO "Participations" (id) VALUES (1);
        INSERT INTO "GameChallenges" (id, revision) VALUES (1, 0);
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let old_options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .application_name("legacy-rsctf-web")
        .options([("search_path", schema.as_str())]);
    let old_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(old_options)
        .await
        .unwrap();
    let guard_end = UP_SQL
        .find("-- Drain in-flight gameplay writers")
        .expect("exclusive guard marker");
    let guard_error = sqlx::raw_sql(&UP_SQL[..guard_end])
        .execute(&pool)
        .await
        .expect_err("a legacy client must block m0089 before DDL");
    assert!(guard_error
        .to_string()
        .contains("exclusive schema cutover refused"));
    old_pool.close().await;

    // Model an already-running old submit: it owns Games/Participations row
    // locks and has inserted its child row, but still needs its late
    // GameChallenges counter update before commit.
    let mut old_submit = pool.begin().await.unwrap();
    sqlx::query(r#"SELECT id FROM "Games" WHERE id = 1 FOR SHARE"#)
        .execute(&mut *old_submit)
        .await
        .unwrap();
    sqlx::query(r#"SELECT id FROM "Participations" WHERE id = 1 FOR SHARE"#)
        .execute(&mut *old_submit)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Submissions" (id) VALUES (1)"#)
        .execute(&mut *old_submit)
        .await
        .unwrap();

    let lock_end = UP_SQL
        .find("-- A manual idempotence/recovery rerun")
        .expect("lock prelude marker");
    let lock_sql = UP_SQL[..lock_end].to_owned();
    let migration_pool = pool.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let migration = async move {
        let mut transaction = migration_pool.begin().await.unwrap();
        sqlx::query("SET LOCAL lock_timeout = '3s'")
            .execute(&mut *transaction)
            .await
            .unwrap();
        started_tx.send(()).unwrap();
        sqlx::raw_sql(&lock_sql)
            .execute(&mut *transaction)
            .await
            .unwrap();
        transaction.rollback().await.unwrap();
    };
    let finish_old_submit = async move {
        started_rx.await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            sqlx::query(r#"UPDATE "GameChallenges" SET revision = revision + 1 WHERE id = 1"#)
                .execute(&mut *old_submit),
        )
        .await
        .expect("migration must not hold a child lock while waiting for Games")
        .unwrap();
        old_submit.commit().await.unwrap();
    };
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        tokio::join!(migration, finish_old_submit)
    })
    .await
    .expect("migration lock drain completes after old submit");

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}
