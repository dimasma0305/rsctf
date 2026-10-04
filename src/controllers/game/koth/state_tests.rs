use sqlx::{Connection, PgConnection};

use super::{KothHillBaseRow, KOTH_HILL_BASE_SQL};

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn hill_state_query_returns_only_the_requested_game_and_challenge_endpoint() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
    let mut connection = PgConnection::connect(&database_url).await.unwrap();
    sqlx::raw_sql(
        r#"CREATE TEMP TABLE "Games" (id INTEGER PRIMARY KEY);
           CREATE TEMP TABLE "KothTargets" (
             id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
             challenge_id INTEGER NOT NULL, container_id TEXT,
             host TEXT, port INTEGER, holder_participation_id INTEGER
           );
           CREATE TEMP TABLE "KothOfficialConfigs" (
             game_id INTEGER NOT NULL, hills_snapshot JSONB NOT NULL
           );
           CREATE TEMP TABLE "KothApiObservers" (
             game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL
           );
           CREATE TEMP TABLE "KothCrownCycles" (
             game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL
           );
           CREATE TEMP TABLE "Participations" (
             id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
             status SMALLINT NOT NULL, team_id INTEGER NOT NULL
           );
           CREATE TEMP TABLE "Teams" (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
           CREATE TEMP TABLE "KothControlResults" (
             id BIGINT PRIMARY KEY, game_id INTEGER NOT NULL,
             challenge_id INTEGER NOT NULL, container_id TEXT,
             status SMALLINT, is_scorable BOOLEAN NOT NULL,
             checked_at TIMESTAMPTZ, ad_round_id INTEGER NOT NULL
           );"#,
    )
    .execute(&mut connection)
    .await
    .unwrap();

    sqlx::raw_sql(
        r#"INSERT INTO "Games" VALUES (10), (11);
           INSERT INTO "Teams" VALUES (20, 'red'), (21, 'blue');
           INSERT INTO "Participations" VALUES
             (30, 10, 1, 20), (31, 11, 1, 21);
           INSERT INTO "KothTargets" VALUES
             (40, 10, 2, 'container-requested', '10.0.0.2', 31337, 30),
             (41, 10, 3, 'container-unrelated', '10.0.0.3', 31338, 30),
             (42, 11, 2, 'container-other-game', '10.0.1.2', 41337, 31);
           INSERT INTO "KothCrownCycles" VALUES (10, 2), (11, 2);
           -- A successful functional check remains player-visible even when
           -- this particular leaderboard round is not scoreable.
           INSERT INTO "KothControlResults" VALUES
             (50, 10, 2, 'container-requested', 0, FALSE, NOW(), 7),
             (51, 10, 3, 'container-unrelated', 2, FALSE, NOW(), 7),
             (52, 11, 2, 'container-other-game', 1, TRUE, NOW(), 7);"#,
    )
    .execute(&mut connection)
    .await
    .unwrap();

    let requested = sqlx::query_as::<_, KothHillBaseRow>(KOTH_HILL_BASE_SQL)
        .bind(10)
        .bind(2)
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(
        requested.container_id.as_deref(),
        Some("container-requested")
    );
    assert_eq!(requested.ip.as_deref(), Some("10.0.0.2"));
    assert_eq!(requested.port, Some(31337));
    assert_eq!(requested.holder_team_name.as_deref(), Some("red"));
    assert_eq!(
        requested.evidence_container_id.as_deref(),
        Some("container-requested")
    );
    assert_eq!(requested.status_raw, Some(0));
    assert!(requested.managed_crown_cycle);

    // A challenge absent from game 10 returns no target projection even though
    // another game has a target with the same challenge id.
    let absent = sqlx::query_as::<_, KothHillBaseRow>(KOTH_HILL_BASE_SQL)
        .bind(10)
        .bind(99)
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert!(absent.container_id.is_none());
    assert!(absent.ip.is_none());
    assert!(absent.port.is_none());
    assert!(absent.holder_participation_id.is_none());
}
