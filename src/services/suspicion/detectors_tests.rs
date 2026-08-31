use super::{
    earliest_burst_completion, final_snapshot_ready, high_wrong_rate_hits, in_competitive_window,
    is_hoarded_submission, load_canonical_solves, load_competitive_game_window,
    persist_suspicion_event_with_weight_guarded, valid_evidence_key, CompetitiveGameWindow,
    ReconciliationSnapshot, SuspicionType, HIGH_WRONG_RATE_HITS_SQL, INSERT_SUSPICION_EVENT_SQL,
    MAX_EVIDENCE_KEY_BYTES,
};
use crate::services::suspicion::cheat_stat::collaboration_candidates;
use sqlx::postgres::PgPoolOptions;

#[allow(clippy::too_many_arguments)]
async fn persist_suspicion_event_with_weight(
    pool: &sqlx::PgPool,
    game_id: i32,
    participation_id: i32,
    challenge_id: Option<i32>,
    ty: SuspicionType,
    evidence_key: &str,
    weight: i32,
    description: &str,
) -> crate::utils::error::AppResult<bool> {
    persist_suspicion_event_with_weight_guarded(
        pool,
        game_id,
        participation_id,
        challenge_id,
        ty,
        evidence_key,
        weight,
        description,
        chrono::Utc::now(),
        None,
    )
    .await
}

#[test]
fn suspicion_write_is_conflict_gated_before_canonical_projection() {
    assert!(INSERT_SUSPICION_EVENT_SQL
        .contains("ON CONFLICT (game_id, participation_id, kind, evidence_key) DO NOTHING"));
    assert!(INSERT_SUSPICION_EVENT_SQL.contains("WITH participant AS MATERIALIZED"));
    assert!(INSERT_SUSPICION_EVENT_SQL.contains("FOR UPDATE"));
    assert!(INSERT_SUSPICION_EVENT_SQL.contains("SELECT 1 FROM \"SuspicionEvents\" existing"));
    assert!(INSERT_SUSPICION_EVENT_SQL.contains("existing.evidence_key = $5"));
    assert!(INSERT_SUSPICION_EVENT_SQL.contains("EXISTS (SELECT 1 FROM inserted)"));
    assert!(!INSERT_SUSPICION_EVENT_SQL.contains("suspicion_score +"));
}

#[tokio::test]
#[ignore = "requires migrated disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn postgres_detector_replay_does_not_redirty_reconciliation() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .unwrap();
    let (game_id, participation_id): (i32, i32) = sqlx::query_as(
        r#"SELECT game_id, id FROM "Participations"
            WHERE competitive_admitted_at_utc IS NOT NULL
            ORDER BY game_id, id LIMIT 1"#,
    )
    .fetch_one(&pool)
    .await
    .expect("the disposable database needs one competitively admitted participant");
    let evidence_key = format!("reconciliation-replay:{}", uuid::Uuid::new_v4().simple());
    let mut transaction = pool.begin().await.unwrap();
    sqlx::query(
        r#"SELECT 1 FROM "SuspicionReconciliationState"
            WHERE game_id = $1 FOR UPDATE"#,
    )
    .bind(game_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        r#"SELECT 1 FROM "AntiCheatReconciliationQueue"
            WHERE game_id = $1 FOR UPDATE"#,
    )
    .bind(game_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        r#"UPDATE "AntiCheatReconciliationSources"
              SET applied_version = dirty_version WHERE game_id = $1"#,
    )
    .bind(game_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        r#"UPDATE "AntiCheatReconciliationQueue"
              SET applied_generation = desired_generation WHERE game_id = $1"#,
    )
    .bind(game_id)
    .execute(&mut *transaction)
    .await
    .unwrap();

    let first: (bool, bool) = sqlx::query_as(INSERT_SUSPICION_EVENT_SQL)
        .bind(game_id)
        .bind(participation_id)
        .bind(None::<i32>)
        .bind(SuspicionType::SharedFingerprint.kind())
        .bind(&evidence_key)
        .bind(1_i32)
        .bind(chrono::Utc::now())
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
    assert_eq!(first, (true, true));
    sqlx::query(
        r#"UPDATE "AntiCheatReconciliationSources"
              SET applied_version = dirty_version
            WHERE game_id = $1 AND source_kind = 7"#,
    )
    .bind(game_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        r#"UPDATE "AntiCheatReconciliationQueue"
              SET applied_generation = desired_generation WHERE game_id = $1"#,
    )
    .bind(game_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    let clean_after_first: (i64, i64, i64, i64) = sqlx::query_as(
        r#"SELECT source.applied_version, source.dirty_version,
                  queue.applied_generation, queue.desired_generation
             FROM "AntiCheatReconciliationSources" source
             JOIN "AntiCheatReconciliationQueue" queue
               ON queue.game_id = source.game_id
            WHERE source.game_id = $1 AND source.source_kind = 7"#,
    )
    .bind(game_id)
    .fetch_one(&mut *transaction)
    .await
    .unwrap();
    assert_eq!(clean_after_first.0, clean_after_first.1);
    assert_eq!(clean_after_first.2, clean_after_first.3);

    let second: (bool, bool) = sqlx::query_as(INSERT_SUSPICION_EVENT_SQL)
        .bind(game_id)
        .bind(participation_id)
        .bind(None::<i32>)
        .bind(SuspicionType::SharedFingerprint.kind())
        .bind(&evidence_key)
        .bind(1_i32)
        .bind(chrono::Utc::now())
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
    assert_eq!(second, (true, false));
    let clean_after_replay: (i64, i64, i64, i64) = sqlx::query_as(
        r#"SELECT source.applied_version, source.dirty_version,
                  queue.applied_generation, queue.desired_generation
             FROM "AntiCheatReconciliationSources" source
             JOIN "AntiCheatReconciliationQueue" queue
               ON queue.game_id = source.game_id
            WHERE source.game_id = $1 AND source.source_kind = 7"#,
    )
    .bind(game_id)
    .fetch_one(&mut *transaction)
    .await
    .unwrap();
    assert_eq!(clean_after_replay, clean_after_first);
    transaction.rollback().await.unwrap();
    pool.close().await;
}

#[test]
fn game_challenge_type_queries_use_the_real_quoted_column() {
    let detectors = include_str!("detectors.rs");
    let abnormal = include_str!("cheat_checks.rs");
    let container = include_str!("container_access.rs");
    assert!(detectors.contains("challenge.\"Type\""));
    assert!(abnormal.contains("SELECT id, \"Type\""));
    assert!(container.contains("challenge.\"Type\" = ANY"));
    assert!(detectors.contains("challenge.challenge_type"));
    assert!(!abnormal.contains("SELECT id, challenge_type"));
    assert!(!container.contains("challenge.challenge_type"));
}

#[test]
fn evidence_keys_are_nonempty_and_bounded() {
    assert!(!valid_evidence_key(""));
    assert!(!valid_evidence_key("   "));
    assert!(valid_evidence_key("submission:500"));
    assert!(valid_evidence_key(&"x".repeat(MAX_EVIDENCE_KEY_BYTES)));
    assert!(!valid_evidence_key(&"x".repeat(MAX_EVIDENCE_KEY_BYTES + 1)));
}

#[test]
fn competitive_window_is_start_inclusive_and_end_exclusive() {
    let start = chrono::Utc::now();
    let window = CompetitiveGameWindow {
        start,
        end: start + chrono::Duration::minutes(10),
    };
    assert!(in_competitive_window(start, window));
    assert!(in_competitive_window(
        start + chrono::Duration::minutes(9),
        window
    ));
    assert!(!in_competitive_window(window.end, window));
}

#[test]
fn barrier_snapshot_authority_does_not_depend_on_the_application_clock() {
    assert!(!final_snapshot_ready(ReconciliationSnapshot::Live));
    assert!(final_snapshot_ready(
        ReconciliationSnapshot::BarrierBackedFinal
    ));
}

#[test]
fn high_wrong_query_is_challenge_local_canonical_and_matured() {
    assert!(HIGH_WRONG_RATE_HITS_SQL
        .contains("PARTITION BY submission.participation_id, submission.challenge_id"));
    assert!(HIGH_WRONG_RATE_HITS_SQL.contains("\"FirstSolves\""));
    assert!(HIGH_WRONG_RATE_HITS_SQL.contains("'60 seconds'::interval FOLLOWING"));
    assert!(HIGH_WRONG_RATE_HITS_SQL.contains("NTH_VALUE("));
    assert!(HIGH_WRONG_RATE_HITS_SQL.contains("MIN(wrong_window.threshold_time) AS observed_at"));
    assert!(HIGH_WRONG_RATE_HITS_SQL
        .contains("LEAST(\n               wrong_window.anchor_time + '5 minutes'::interval"));
    assert!(HIGH_WRONG_RATE_HITS_SQL.contains("solve.challenge_id = wrong_window.challenge_id"));
}

#[test]
fn hoarding_requires_a_complete_immutable_submit_snapshot() {
    let submitted_at = chrono::Utc::now();
    let old_operation = submitted_at - chrono::Duration::minutes(61);
    let boundary_operation = submitted_at - chrono::Duration::minutes(60);
    let just_over_boundary = boundary_operation - chrono::Duration::milliseconds(1);
    assert!(is_hoarded_submission(
        submitted_at,
        false,
        Some(old_operation),
        Some(false)
    ));
    assert!(!is_hoarded_submission(submitted_at, false, None, None));
    assert!(!is_hoarded_submission(
        submitted_at,
        false,
        Some(boundary_operation),
        Some(false)
    ));
    assert!(is_hoarded_submission(
        submitted_at,
        false,
        Some(just_over_boundary),
        Some(false)
    ));
    assert!(!is_hoarded_submission(
        submitted_at,
        true,
        Some(old_operation),
        Some(false)
    ));
    assert!(!is_hoarded_submission(
        submitted_at,
        false,
        Some(old_operation),
        Some(true)
    ));
}

#[test]
fn burst_provenance_is_canonical_and_replay_order_independent() {
    let start = chrono::Utc::now();
    let second = start + chrono::Duration::seconds(20);
    let threshold_solve = start + chrono::Duration::seconds(60);
    assert_eq!(
        earliest_burst_completion(vec![threshold_solve, start, second]),
        Some(threshold_solve)
    );
    assert_eq!(
        earliest_burst_completion(vec![
            start + chrono::Duration::seconds(60) + chrono::Duration::milliseconds(1),
            second,
            start,
        ]),
        None,
        "the 60-second threshold must not be widened by integer truncation"
    );
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn concurrent_rule_retries_insert_and_score_exactly_once() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect test database");
    let schema = format!("suspicion_write_{}", uuid::Uuid::new_v4().simple());
    let setup = format!(
        r#"
            CREATE SCHEMA "{schema}";
            CREATE TABLE "{schema}"."Games" (
                id INTEGER PRIMARY KEY,
                start_time_utc TIMESTAMPTZ NOT NULL,
                end_time_utc TIMESTAMPTZ NOT NULL,
                practice_mode BOOLEAN NOT NULL DEFAULT FALSE,
                deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "{schema}"."GameChallenges" (
                id INTEGER PRIMARY KEY,
                game_id INTEGER NOT NULL,
                is_enabled BOOLEAN NOT NULL DEFAULT TRUE,
                deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "{schema}"."Teams" (
                id INTEGER PRIMARY KEY,
                deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "{schema}"."Participations" (
                id INTEGER PRIMARY KEY,
                game_id INTEGER NOT NULL,
                team_id INTEGER NOT NULL,
                status SMALLINT NOT NULL,
                competitive_admitted_at_utc TIMESTAMPTZ,
                suspicion_score INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE "{schema}"."SuspicionEvents" (
                id BIGSERIAL PRIMARY KEY,
                game_id INTEGER NOT NULL,
                participation_id INTEGER NOT NULL,
                challenge_id INTEGER,
                kind SMALLINT NOT NULL,
                evidence_key TEXT NOT NULL,
                score_delta INTEGER,
                created_at TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "{schema}"."Submissions" (
                id INTEGER PRIMARY KEY,
                team_id INTEGER NOT NULL,
                participation_id INTEGER NOT NULL,
                challenge_id INTEGER NOT NULL,
                game_id INTEGER NOT NULL,
                status SMALLINT NOT NULL,
                submit_time_utc TIMESTAMPTZ NOT NULL,
                container_id UUID,
                container_last_operation_at_submit TIMESTAMPTZ,
                container_was_loaded_at_submit BOOLEAN,
                first_open_at_submit TIMESTAMPTZ,
                first_download_at_submit TIMESTAMPTZ,
                first_container_start_at_submit TIMESTAMPTZ
            );
            CREATE TABLE "{schema}"."FirstSolves" (
                participation_id INTEGER NOT NULL,
                challenge_id INTEGER NOT NULL,
                submission_id INTEGER NOT NULL,
                PRIMARY KEY (participation_id, challenge_id)
            );
            CREATE UNIQUE INDEX ux_suspicionevents_incident
              ON "{schema}"."SuspicionEvents"
                 (game_id, participation_id, kind, evidence_key);
            "#
    );
    sqlx::raw_sql(&setup)
        .execute(&admin)
        .await
        .expect("create isolated suspicion schema");

    let search_path_schema = schema.clone();
    let application_name = format!("rsctf_suspicion_{}", uuid::Uuid::new_v4().simple());
    let connect_application_name = application_name.clone();
    let pool = PgPoolOptions::new()
        .max_connections(16)
        .after_connect(move |connection, _metadata| {
            let application_statement =
                format!(r#"SET application_name TO '{connect_application_name}'"#);
            let search_path_statement = format!(r#"SET search_path TO "{search_path_schema}""#);
            Box::pin(async move {
                sqlx::query(&application_statement)
                    .execute(&mut *connection)
                    .await?;
                sqlx::query(&search_path_statement)
                    .execute(&mut *connection)
                    .await?;
                Ok(())
            })
        })
        .connect(&database_url)
        .await
        .expect("connect isolated suspicion pool");
    sqlx::query(
        r#"INSERT INTO "Games"
               (id, start_time_utc, end_time_utc, practice_mode)
           VALUES (1, CURRENT_TIMESTAMP - INTERVAL '2 hours',
                   CURRENT_TIMESTAMP - INTERVAL '1 hour', TRUE)"#,
    )
    .execute(&pool)
    .await
    .expect("insert game");
    let configured_window: (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as(r#"SELECT start_time_utc, end_time_utc FROM "Games" WHERE id = 1"#)
            .fetch_one(&pool)
            .await
            .expect("read configured competitive window");
    let loaded_window = load_competitive_game_window(&pool, 1)
        .await
        .expect("load configured competitive window")
        .expect("game window exists");
    assert_eq!(loaded_window.start, configured_window.0);
    assert_eq!(loaded_window.end, configured_window.1);
    assert!(loaded_window.end < chrono::Utc::now());
    sqlx::query(r#"INSERT INTO "GameChallenges" (id, game_id) VALUES (20, 1), (21, 1)"#)
        .execute(&pool)
        .await
        .expect("insert challenges");
    sqlx::query(r#"INSERT INTO "Teams" (id) VALUES (30), (31), (32), (33), (34)"#)
        .execute(&pool)
        .await
        .expect("insert team");
    sqlx::query(
        r#"INSERT INTO "Participations"
                 (id, game_id, team_id, status,
                  competitive_admitted_at_utc, suspicion_score)
               VALUES (10, 1, 30, 1, CURRENT_TIMESTAMP - INTERVAL '90 minutes', 0),
                      (11, 1, 31, 3, CURRENT_TIMESTAMP - INTERVAL '90 minutes', 0),
                      (12, 1, 32, 1, CURRENT_TIMESTAMP - INTERVAL '90 minutes', 0),
                      (13, 1, 33, 1, CURRENT_TIMESTAMP - INTERVAL '90 minutes', 0),
                      (14, 1, 34, 1, NULL, 0)"#,
    )
    .execute(&pool)
    .await
    .expect("insert participation");

    let tasks = (0..64)
        .map(|_| {
            let pool = pool.clone();
            tokio::spawn(async move {
                persist_suspicion_event_with_weight(
                    &pool,
                    1,
                    10,
                    Some(20),
                    SuspicionType::StolenFlag,
                    "submission:500",
                    100,
                    "concurrent test",
                )
                .await
                .expect("persist concurrent suspicion event")
            })
        })
        .collect::<Vec<_>>();

    let mut inserted = 0usize;
    for task in tasks {
        inserted += usize::from(task.await.expect("join suspicion writer"));
    }

    assert_eq!(inserted, 1);

    let second_incident = persist_suspicion_event_with_weight(
        &pool,
        1,
        10,
        Some(20),
        SuspicionType::StolenFlag,
        "submission:501",
        100,
        "distinct incident",
    )
    .await
    .expect("persist distinct suspicion event");
    assert!(second_incident);

    let event_count: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "SuspicionEvents""#)
        .fetch_one(&pool)
        .await
        .expect("count suspicion events");
    let score: i32 =
        sqlx::query_scalar(r#"SELECT suspicion_score FROM "Participations" WHERE id = 10"#)
            .fetch_one(&pool)
            .await
            .expect("read suspicion score");
    let deltas: Vec<i32> =
        sqlx::query_scalar(r#"SELECT score_delta FROM "SuspicionEvents" ORDER BY evidence_key"#)
            .fetch_all(&pool)
            .await
            .expect("read persisted score deltas");
    assert_eq!(event_count, 2);
    assert_eq!(deltas, vec![100, 100]);
    assert_eq!(score, 200);

    // Reproduce the production lock stack: a submit for challenge 20 owns
    // the participation score scope and shared participation row before
    // its late counter update. A detector for a *different* challenge must
    // wait on the score scope without retaining any audit row lock. Its
    // distinct pair key cannot accidentally make this test pass.
    let mut submit = pool.begin().await.expect("begin simulated submit");
    super::lock_participation_suspicion_writes(&mut submit, 10)
        .await
        .expect("lock simulated suspicion score scope");
    sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
        .bind(10_i32)
        .bind(20_i32)
        .execute(&mut *submit)
        .await
        .expect("lock simulated submission pair");
    sqlx::query(r#"SELECT id FROM "Games" WHERE id = 1 FOR SHARE"#)
        .execute(&mut *submit)
        .await
        .expect("lock simulated submission game");
    sqlx::query(r#"SELECT id FROM "Participations" WHERE id = 10 FOR SHARE"#)
        .execute(&mut *submit)
        .await
        .expect("lock simulated submission participation");

    let detector_pool = pool.clone();
    let detector = tokio::spawn(async move {
        persist_suspicion_event_with_weight(
            &detector_pool,
            1,
            10,
            Some(21),
            SuspicionType::StolenFlag,
            "submission:502",
            100,
            "submit lock-order test",
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                r#"SELECT EXISTS(
                         SELECT 1
                           FROM pg_locks lock
                          JOIN pg_stat_activity activity ON activity.pid = lock.pid
                          WHERE lock.locktype = 'advisory'
                            AND lock.granted = FALSE AND lock.objsubid = 2
                            AND lock.objid::bigint = 10
                            AND activity.application_name = $1
                       )"#,
            )
            .bind(&application_name)
            .fetch_one(&pool)
            .await
            .expect("inspect detector advisory wait");
            if waiting {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("detector did not wait outside the submit row-lock stack");

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        sqlx::query(r#"UPDATE "GameChallenges" SET is_enabled = is_enabled WHERE id = 20"#)
            .execute(&mut *submit),
    )
    .await
    .expect("challenge counter update deadlocked")
    .expect("update simulated challenge counter");
    submit.commit().await.expect("commit simulated submit");
    assert!(detector
        .await
        .expect("join post-submit detector")
        .expect("persist post-submit suspicion event"));

    let final_score: i32 =
        sqlx::query_scalar(r#"SELECT suspicion_score FROM "Participations" WHERE id = 10"#)
            .fetch_one(&pool)
            .await
            .expect("read final suspicion score");
    assert_eq!(final_score, 300);

    // Canonical projection semantics must hold on the transactional writer,
    // not only in the pure scoring unit tests. Historical replay remains
    // attributable to a suspended participation and a disabled challenge.
    persist_suspicion_event_with_weight(
        &pool,
        1,
        11,
        None,
        SuspicionType::SharedFingerprint,
        "global",
        60,
        "context-only projection test",
    )
    .await
    .expect("persist context evidence for suspended participation");
    let context_score: i32 =
        sqlx::query_scalar(r#"SELECT suspicion_score FROM "Participations" WHERE id = 11"#)
            .fetch_one(&pool)
            .await
            .expect("read context-only score");
    assert_eq!(context_score, 0);

    sqlx::query(r#"UPDATE "GameChallenges" SET is_enabled = FALSE WHERE id = 20"#)
        .execute(&pool)
        .await
        .expect("disable historical challenge");
    for incident in 0..4 {
        persist_suspicion_event_with_weight(
            &pool,
            1,
            12,
            Some(20),
            SuspicionType::Burst,
            &format!("behavioral:{incident}"),
            100,
            "behavioral tier-ceiling test",
        )
        .await
        .expect("persist behavioral evidence");
    }
    let behavioral_score: i32 =
        sqlx::query_scalar(r#"SELECT suspicion_score FROM "Participations" WHERE id = 12"#)
            .fetch_one(&pool)
            .await
            .expect("read behavioral score");
    assert_eq!(behavioral_score, 25);

    for submission_id in 503..=510 {
        persist_suspicion_event_with_weight(
            &pool,
            1,
            10,
            Some(20),
            SuspicionType::StolenFlag,
            &format!("submission:{submission_id}"),
            100,
            "hard incident-cap test",
        )
        .await
        .expect("persist capped hard evidence");
    }
    let capped_hard_score: i32 =
        sqlx::query_scalar(r#"SELECT suspicion_score FROM "Participations" WHERE id = 10"#)
            .fetch_one(&pool)
            .await
            .expect("read capped hard score");
    assert_eq!(capped_hard_score, 1_000);

    let now = chrono::Utc::now();
    let mature_anchor = now - chrono::Duration::minutes(10);
    sqlx::query(
        r#"INSERT INTO "Submissions"
               (id, team_id, participation_id, challenge_id, game_id, status,
                submit_time_utc)
           SELECT 1000 + attempt, 32, 12, 21, 1, 2,
                  $1 + make_interval(secs => attempt)
             FROM generate_series(0, 39) AS attempt"#,
    )
    .bind(mature_anchor)
    .execute(&pool)
    .await
    .expect("insert mature wrong burst");
    sqlx::query(r#"UPDATE "Participations" SET status = 2 WHERE id = 12"#)
        .execute(&pool)
        .await
        .expect("reject competitor after immutable admission");
    let live_window = CompetitiveGameWindow {
        start: now - chrono::Duration::hours(1),
        end: now + chrono::Duration::hours(1),
    };
    sqlx::query(
        r#"UPDATE "Participations"
              SET competitive_admitted_at_utc = $1
            WHERE id = 14"#,
    )
    .bind(live_window.end)
    .execute(&pool)
    .await
    .expect("set exact-end post-competition admission");
    // Fifty unrelated three-solve teams must not hide a colluding pair beyond
    // the legacy application-side Take(50) boundary.
    sqlx::query(r#"INSERT INTO "Teams" (id) SELECT 1000 + participant FROM generate_series(100, 151) participant"#)
        .execute(&pool)
        .await
        .expect("insert collaboration candidate teams");
    sqlx::query(
        r#"INSERT INTO "Participations"
                 (id, game_id, team_id, status,
                  competitive_admitted_at_utc, suspicion_score)
           SELECT participant, 1, 1000 + participant, 1, $1, 0
             FROM generate_series(100, 151) participant"#,
    )
    .bind(live_window.start)
    .execute(&pool)
    .await
    .expect("insert collaboration candidate participations");
    sqlx::query(
        r#"INSERT INTO "Submissions"
               (id, team_id, participation_id, challenge_id, game_id, status,
                submit_time_utc)
           SELECT 7000 + (participant - 100) * 3 + solve_offset,
                  1000 + participant,
                  participant,
                  CASE WHEN participant < 150
                       THEN 10000 + (participant - 100) * 3 + solve_offset
                       ELSE 20000 + solve_offset END,
                  1, 1, $1 + (participant * INTERVAL '1 millisecond')
             FROM generate_series(100, 151) participant
            CROSS JOIN generate_series(0, 2) solve_offset"#,
    )
    .bind(live_window.start + chrono::Duration::minutes(1))
    .execute(&pool)
    .await
    .expect("insert collaboration candidate solves");
    sqlx::query(
        r#"INSERT INTO "FirstSolves"
               (participation_id, challenge_id, submission_id)
           SELECT participation_id, challenge_id, id
             FROM "Submissions"
            WHERE id >= 7000"#,
    )
    .execute(&pool)
    .await
    .expect("project collaboration candidate solves");
    let mut informative_challenge_ids: Vec<i32> = (10000..10150).collect();
    informative_challenge_ids.extend(20000..20003);
    assert_eq!(
        collaboration_candidates(&pool, 1, live_window, &informative_challenge_ids)
            .await
            .expect("load unbounded collaboration candidates"),
        vec![(150, 151)]
    );
    let sweep_hits = high_wrong_rate_hits(&pool, 1, live_window, None)
        .await
        .expect("evaluate sweep wrong-rate hits");
    let scoped_hits = high_wrong_rate_hits(&pool, 1, live_window, Some(12))
        .await
        .expect("evaluate live-scoped wrong-rate hits");
    let sweep_for_participation: Vec<_> = sweep_hits
        .into_iter()
        .filter(|(participation_id, _, _)| *participation_id == 12)
        .collect();
    assert_eq!(scoped_hits, sweep_for_participation);
    assert_eq!(scoped_hits.len(), 1);
    assert_eq!(scoped_hits[0].0, 12);
    assert_eq!(scoped_hits[0].1, 21);
    assert_eq!(
        scoped_hits[0].2.timestamp_micros(),
        (mature_anchor + chrono::Duration::seconds(39)).timestamp_micros(),
        "HighWrongRate is observed when the 40th attempt completes the window"
    );
    sqlx::query(r#"UPDATE "Participations" SET status = 3 WHERE id = 12"#)
        .execute(&pool)
        .await
        .expect("restore supported historical suspension state");

    let solve_time = mature_anchor + chrono::Duration::minutes(1);
    sqlx::query(
        r#"INSERT INTO "Submissions"
               (id, team_id, participation_id, challenge_id, game_id, status,
                submit_time_utc, container_last_operation_at_submit,
                container_was_loaded_at_submit)
           VALUES (2000, 32, 12, 21, 1, 1, $1, $2, FALSE),
                  (2001, 32, 12, 21, 1, 1, $1 + INTERVAL '1 minute',
                   NULL, NULL),
                  (2002, 34, 14, 21, 1, 1, $3,
                   NULL, NULL)"#,
    )
    .bind(solve_time)
    .bind(solve_time - chrono::Duration::hours(2))
    .bind(solve_time)
    .execute(&pool)
    .await
    .expect("insert canonical and duplicate accepted solves");
    sqlx::query(
        r#"INSERT INTO "FirstSolves"
               (participation_id, challenge_id, submission_id)
           VALUES (12, 21, 2000), (14, 21, 2002)"#,
    )
    .execute(&pool)
    .await
    .expect("project suppressing canonical solve");
    let canonical_solves = load_canonical_solves(&pool, 1, live_window)
        .await
        .expect("load canonical solves");
    let canonical_for_pair: Vec<_> = canonical_solves
        .iter()
        .filter(|solve| solve.participation_id == 12 && solve.challenge_id == 21)
        .collect();
    assert_eq!(canonical_for_pair.len(), 1);
    assert_eq!(
        canonical_for_pair[0].submit_time_utc.timestamp_micros(),
        solve_time.timestamp_micros()
    );
    assert_eq!(canonical_for_pair[0].container_id, None);
    assert_eq!(
        canonical_for_pair[0]
            .container_last_operation_at_submit
            .map(|time| time.timestamp_micros()),
        Some((solve_time - chrono::Duration::hours(2)).timestamp_micros())
    );
    assert_eq!(
        canonical_for_pair[0].container_was_loaded_at_submit,
        Some(false)
    );
    assert!(canonical_solves
        .iter()
        .all(|solve| solve.participation_id != 14));
    assert!(high_wrong_rate_hits(&pool, 1, live_window, Some(12))
        .await
        .expect("evaluate solved wrong-rate burst")
        .is_empty());
    let stale_high_wrong_insert = persist_suspicion_event_with_weight_guarded(
        &pool,
        1,
        12,
        Some(21),
        SuspicionType::HighWrongRate,
        "challenge:21",
        40,
        "stale high-wrong candidate",
        mature_anchor,
        Some(live_window),
    )
    .await
    .expect("recheck stale high-wrong candidate");
    assert!(!stale_high_wrong_insert);
    let stale_high_wrong_events: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)
             FROM "SuspicionEvents"
            WHERE participation_id = 12 AND kind = 24"#,
    )
    .fetch_one(&pool)
    .await
    .expect("count stale high-wrong events");
    assert_eq!(stale_high_wrong_events, 0);

    sqlx::query(
        r#"INSERT INTO "Submissions"
               (id, team_id, participation_id, challenge_id, game_id, status,
                submit_time_utc)
           SELECT 3000 + attempt, 31, 11,
                  CASE WHEN attempt < 20 THEN 20 ELSE 21 END,
                  1, 2, $1 + make_interval(secs => attempt % 20)
             FROM generate_series(0, 39) AS attempt"#,
    )
    .bind(mature_anchor)
    .execute(&pool)
    .await
    .expect("insert cross-challenge wrong attempts");
    assert!(high_wrong_rate_hits(&pool, 1, live_window, Some(11))
        .await
        .expect("evaluate challenge isolation")
        .is_empty());

    let immature_anchor = now - chrono::Duration::minutes(1);
    sqlx::query(
        r#"INSERT INTO "Submissions"
               (id, team_id, participation_id, challenge_id, game_id, status,
                submit_time_utc)
           SELECT 4000 + attempt, 33, 13, 20, 1, 2,
                  $1 + make_interval(secs => attempt)
             FROM generate_series(0, 39) AS attempt"#,
    )
    .bind(immature_anchor)
    .execute(&pool)
    .await
    .expect("insert immature wrong burst");
    assert!(high_wrong_rate_hits(&pool, 1, live_window, Some(13))
        .await
        .expect("evaluate immature wrong-rate burst")
        .is_empty());
    let ended_window = CompetitiveGameWindow {
        start: live_window.start,
        end: chrono::Utc::now(),
    };
    assert!(
        immature_anchor + chrono::Duration::minutes(5) > ended_window.end,
        "the last-five-minute anchor must mature at the competition end"
    );
    assert_eq!(
        high_wrong_rate_hits(&pool, 1, ended_window, Some(13))
            .await
            .expect("evaluate event-end maturity")
            .len(),
        1
    );
    let pre_practice_window = CompetitiveGameWindow {
        start: live_window.start,
        end: mature_anchor - chrono::Duration::seconds(1),
    };
    assert!(
        high_wrong_rate_hits(&pool, 1, pre_practice_window, Some(12))
            .await
            .expect("exclude submissions after game end")
            .is_empty()
    );

    pool.close().await;
    let teardown = format!(r#"DROP SCHEMA "{schema}" CASCADE"#);
    sqlx::query(&teardown)
        .execute(&admin)
        .await
        .expect("drop isolated suspicion schema");
}
