use super::*;
use sqlx::{Connection, PgConnection};

#[test]
fn deadline_limited_hill_retains_queue_and_marker_slack() {
    let now = tokio::time::Instant::now();
    let deadline = now + Duration::from_secs(20);
    let planned = deadline_limited_probe_budget(
        deadline,
        Duration::from_secs(30),
        KOTH_COMPLETION_MARGIN,
        now,
    )
    .unwrap();
    assert_eq!(planned, Duration::from_secs(8));
    assert!(checker_probe_can_start(
        deadline,
        planned,
        KOTH_COMPLETION_MARGIN,
        now + Duration::from_secs(1),
    ));
    assert!(!checker_probe_can_start(
        deadline,
        planned,
        KOTH_COMPLETION_MARGIN,
        now + Duration::from_secs(9),
    ));
}

#[test]
fn recovery_roster_excludes_already_committed_hills() {
    assert!(PENDING_KOTH_CHALLENGES_SQL.contains("NOT EXISTS"));
    assert!(PENDING_KOTH_CHALLENGES_SQL.contains("result.ad_round_id = $2"));
    assert!(PENDING_KOTH_CHALLENGES_SQL
        .contains("result.challenge_id = (frozen.item->>'challengeId')::integer"));
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn recovery_query_returns_only_unresolved_hills() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
    let mut connection = PgConnection::connect(&database_url).await.unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TEMP TABLE "KothOfficialConfigs" (
          game_id INTEGER PRIMARY KEY, hills_snapshot JSONB NOT NULL
        );
        CREATE TEMP TABLE "KothControlResults" (
          game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
          ad_round_id INTEGER NOT NULL
        );
        INSERT INTO "KothOfficialConfigs" VALUES
          (7, '[{"challengeId":9},{"challengeId":10}]');
        INSERT INTO "KothControlResults" VALUES
          (7, 9, 101), (7, 10, 100);
        "#,
    )
    .execute(&mut connection)
    .await
    .unwrap();
    let pending: Vec<i32> = sqlx::query_scalar(PENDING_KOTH_CHALLENGES_SQL)
        .bind(7_i32)
        .bind(101_i32)
        .fetch_all(&mut connection)
        .await
        .unwrap();
    assert_eq!(pending, vec![10]);
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn live_capability_field_excludes_a_banned_snapshot_team() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
    let mut connection = PgConnection::connect(&database_url).await.unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TEMP TABLE "AspNetUsers" (
          id UUID PRIMARY KEY, role SMALLINT NOT NULL
        );
        CREATE TEMP TABLE "Teams" (
          id INTEGER PRIMARY KEY, captain_id UUID NOT NULL,
          deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
        );
        CREATE TEMP TABLE "TeamMembers" (
          team_id INTEGER NOT NULL, user_id UUID NOT NULL
        );
        CREATE TEMP TABLE "Participations" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
          team_id INTEGER NOT NULL, status SMALLINT NOT NULL
        );
        CREATE TEMP TABLE "Games" (
          id INTEGER PRIMARY KEY, start_time_utc TIMESTAMPTZ NOT NULL,
          end_time_utc TIMESTAMPTZ NOT NULL
        );
        CREATE TEMP TABLE "KothOfficialConfigs" (
          game_id INTEGER PRIMARY KEY, claim_confirmation_ticks INTEGER NOT NULL,
          roster_snapshot JSONB NOT NULL
        );
        CREATE TEMP TABLE "AdRounds" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, number INTEGER NOT NULL,
          start_time_utc TIMESTAMPTZ NOT NULL,
          end_time_utc TIMESTAMPTZ NOT NULL, finalized BOOLEAN NOT NULL
        );
        CREATE TEMP TABLE "KothTargets" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
          challenge_id INTEGER NOT NULL, host TEXT NOT NULL, port INTEGER NOT NULL,
          container_id TEXT
        );
        CREATE TEMP TABLE "KothCrownCycles" (
          id BIGINT PRIMARY KEY, game_id INTEGER NOT NULL,
          challenge_id INTEGER NOT NULL, cycle_number INTEGER NOT NULL,
          planned_start_round INTEGER NOT NULL, planned_end_round INTEGER NOT NULL,
          replacement_container_id TEXT, old_container_id TEXT,
          reset_attempt INTEGER NOT NULL, phase TEXT NOT NULL
        );
        CREATE TEMP TABLE "KothTokens" (
          id INTEGER PRIMARY KEY, cycle_id BIGINT NOT NULL,
          challenge_id INTEGER NOT NULL, target_id INTEGER NOT NULL,
          reset_attempt INTEGER NOT NULL, participation_id INTEGER NOT NULL,
          revoked_at TIMESTAMPTZ
        );
        INSERT INTO "AspNetUsers" VALUES
          ('00000000-0000-0000-0000-000000000021', 1),
          ('00000000-0000-0000-0000-000000000022', 1),
          ('00000000-0000-0000-0000-000000000023', 0);
        INSERT INTO "Teams" (id, captain_id) VALUES
          (21, '00000000-0000-0000-0000-000000000021'),
          (22, '00000000-0000-0000-0000-000000000022'),
          (23, '00000000-0000-0000-0000-000000000023');
        INSERT INTO "Participations" VALUES
          (11, 7, 21, 1), (12, 7, 22, 1), (13, 7, 23, 1);
        INSERT INTO "Games" VALUES
          (7, clock_timestamp() - interval '1 hour',
              clock_timestamp() + interval '1 hour');
        INSERT INTO "KothOfficialConfigs" VALUES
          (7, 2, '[11,12,13]'::jsonb);
        INSERT INTO "AdRounds" VALUES
          (101, 7, 5, clock_timestamp() - interval '10 seconds',
              clock_timestamp() + interval '20 seconds', FALSE);
        INSERT INTO "KothTargets" VALUES
          (3, 7, 9, '127.0.0.1', 8080, 'runtime-1');
        INSERT INTO "KothCrownCycles" VALUES
          (41, 7, 9, 1, 1, 10, 'runtime-1', NULL, 1, 'Active');
        INSERT INTO "KothTokens" VALUES
          (101, 41, 9, 3, 1, 11, NULL),
          (102, 41, 9, 3, 1, 12, NULL),
          (103, 41, 9, 3, 1, 13, NULL);
        "#,
    )
    .execute(&mut connection)
    .await
    .unwrap();
    let now = Utc::now();
    let round = ad_round::Model {
        id: 101,
        game_id: 7,
        number: 5,
        start_time_utc: now - chrono::Duration::seconds(10),
        end_time_utc: now + chrono::Duration::seconds(20),
        finalized: false,
    };

    let live = load_live_hill(&mut connection, 7, 9, &round)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(live.eligible_roster, vec![11, 12]);
    assert_eq!((live.token_count, live.roster_count), (2, 2));
    assert!(live.has_complete_token_window());

    sqlx::query(
        r#"UPDATE "AspNetUsers" SET role = 0
            WHERE id = '00000000-0000-0000-0000-000000000022'"#,
    )
    .execute(&mut connection)
    .await
    .unwrap();
    let live = load_live_hill(&mut connection, 7, 9, &round)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(live.eligible_roster, vec![11]);
    assert_eq!((live.token_count, live.roster_count), (1, 1));
    assert!(!live.has_complete_token_window());
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn missing_cycle_still_records_one_platform_void() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
    let mut connection = PgConnection::connect(&database_url).await.unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TEMP TABLE "KothOfficialConfigs" (
          game_id INTEGER NOT NULL
        );
        CREATE TEMP TABLE "KothCrownCycles" (
          id BIGINT PRIMARY KEY, game_id INTEGER NOT NULL,
          challenge_id INTEGER NOT NULL,
          cycle_number INTEGER NOT NULL, planned_start_round INTEGER NOT NULL,
          planned_end_round INTEGER NOT NULL, replacement_container_id TEXT,
          old_container_id TEXT, reset_attempt INTEGER NOT NULL
        );
        CREATE TEMP TABLE "KothControlResults" (
          id BIGSERIAL PRIMARY KEY, game_id INTEGER NOT NULL,
          challenge_id INTEGER NOT NULL, ad_round_id INTEGER NOT NULL,
          controlling_participation_id INTEGER,
          responsible_participation_id INTEGER,
          marker_observed BOOLEAN NOT NULL, status SMALLINT NOT NULL,
          error_message TEXT, checked_at TIMESTAMPTZ NOT NULL,
          cycle_id BIGINT, container_id TEXT, confirmation_streak INTEGER,
          is_scorable BOOLEAN NOT NULL, void_reason TEXT,
          token_window_attempt INTEGER NOT NULL,
          UNIQUE (game_id, challenge_id, ad_round_id)
        );
        "#,
    )
    .execute(&mut connection)
    .await
    .unwrap();
    let now = Utc::now();
    let round = ad_round::Model {
        id: 101,
        game_id: 7,
        number: 4,
        start_time_utc: now,
        end_time_utc: now + chrono::Duration::seconds(30),
        finalized: false,
    };

    insert_missing_cycle_void(&mut connection, 7, 9, &round)
        .await
        .unwrap();
    let void: (Option<i64>, Option<String>, Option<i32>, bool, i16) = sqlx::query_as(
        r#"SELECT cycle_id, container_id, confirmation_streak, is_scorable, status
             FROM "KothControlResults" WHERE ad_round_id = 101"#,
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(
        void,
        (None, None, None, false, AdCheckStatus::InternalError as i16)
    );

    sqlx::query(r#"INSERT INTO "KothOfficialConfigs" VALUES (7)"#)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "KothCrownCycles" VALUES
             (41,7,9,2,4,6,'replacement-41','old-41',3)"#,
    )
    .execute(&mut connection)
    .await
    .unwrap();
    let scoped_round = ad_round::Model { id: 102, ..round };
    insert_missing_cycle_void(&mut connection, 7, 9, &scoped_round)
        .await
        .unwrap();
    insert_missing_cycle_void(&mut connection, 7, 9, &scoped_round)
        .await
        .unwrap();
    let scoped: (Option<i64>, Option<String>, Option<i32>, i32) = sqlx::query_as(
        r#"SELECT cycle_id, container_id, confirmation_streak, token_window_attempt
             FROM "KothControlResults" WHERE ad_round_id = 102"#,
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(
        scoped,
        (Some(41), Some("replacement-41".to_string()), Some(0), 3)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*) FROM "KothControlResults" WHERE ad_round_id = 102"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap(),
        1
    );
}
