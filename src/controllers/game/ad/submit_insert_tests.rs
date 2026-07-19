use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

use super::{insert_accepted_attack_on, AcceptedAttack};
use crate::utils::enums::{ChallengeReviewStatus, ChallengeType, ParticipationStatus};

const GAME_ID: i32 = 1;
const ATTACKER_ID: i32 = 10;

struct Fixture {
    admin: sqlx::PgPool,
    pool: sqlx::PgPool,
    schema: String,
}

impl Fixture {
    async fn create() -> Self {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_ad_submit_insert_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY,
              hidden BOOLEAN NOT NULL,
              freeze_time_utc TIMESTAMPTZ,
              start_time_utc TIMESTAMPTZ NOT NULL,
              end_time_utc TIMESTAMPTZ NOT NULL,
              ad_scoring_paused BOOLEAN NOT NULL,
              ad_flag_lifetime_ticks INTEGER
            );
            CREATE TABLE "Teams" (
              id INTEGER PRIMARY KEY,
              name TEXT NOT NULL
            );
            CREATE TABLE "Participations" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL,
              status SMALLINT NOT NULL
            );
            CREATE TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              title TEXT NOT NULL,
              is_enabled BOOLEAN NOT NULL,
              review_status SMALLINT NOT NULL,
              "Type" SMALLINT NOT NULL
            );
            CREATE TABLE "AdTeamServices" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL
            );
            CREATE TABLE "AdRounds" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              number INTEGER NOT NULL,
              finalized BOOLEAN NOT NULL
            );
            CREATE TABLE "AdFlags" (
              id INTEGER PRIMARY KEY,
              round_id INTEGER NOT NULL,
              team_service_id INTEGER NOT NULL
            );
            CREATE TABLE "AdAttacks" (
              id SERIAL PRIMARY KEY,
              round_id INTEGER NOT NULL,
              attacker_participation_id INTEGER NOT NULL,
              victim_team_service_id INTEGER NOT NULL,
              flag_id INTEGER NOT NULL,
              submitted_at TIMESTAMPTZ NOT NULL,
              UNIQUE (attacker_participation_id, flag_id)
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            r#"INSERT INTO "Games"
                 (id, hidden, freeze_time_utc, start_time_utc, end_time_utc,
                  ad_scoring_paused, ad_flag_lifetime_ticks)
               VALUES ($1, FALSE, NULL, now() - interval '1 hour',
                       now() + interval '1 hour', FALSE, 5)"#,
        )
        .bind(GAME_ID)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "Teams" (id, name)
               VALUES (1, 'attacker'), (2, 'victim-a'), (3, 'victim-b')"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "Participations" (id, game_id, team_id, status)
               VALUES ($1, $2, 1, $5), ($3, $2, 2, $5), ($4, $2, 3, $5)"#,
        )
        .bind(ATTACKER_ID)
        .bind(GAME_ID)
        .bind(20_i32)
        .bind(30_i32)
        .bind(ParticipationStatus::Accepted as i16)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "GameChallenges"
                 (id, game_id, title, is_enabled, review_status, "Type")
               VALUES (100, $1, 'service-a', TRUE, $2, $3),
                      (101, $1, 'service-b', TRUE, $2, $3)"#,
        )
        .bind(GAME_ID)
        .bind(ChallengeReviewStatus::Active as i16)
        .bind(ChallengeType::AttackDefense as i16)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(
            r#"
            INSERT INTO "AdTeamServices" (id, game_id, participation_id, challenge_id)
              VALUES (200, 1, 20, 100), (201, 1, 30, 100), (202, 1, 20, 101);
            INSERT INTO "AdRounds" (id, game_id, number, finalized)
              VALUES (300, 1, 7, FALSE);
            INSERT INTO "AdFlags" (id, round_id, team_service_id)
              VALUES (400, 300, 200), (401, 300, 200),
                     (402, 300, 201), (403, 300, 202),
                     (404, 300, 202), (405, 300, 202),
                     (406, 300, 200);
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        Self {
            admin,
            pool,
            schema,
        }
    }

    async fn insert(&self, service_id: i32, flag_id: i32) -> Option<AcceptedAttack> {
        let mut connection = self.pool.acquire().await.unwrap();
        insert_accepted_attack_on(&mut connection, ATTACKER_ID, service_id, flag_id, GAME_ID)
            .await
            .unwrap()
    }

    async fn cleanup(self) {
        self.pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn accepted_insert_returns_metadata_and_scopes_sequential_first_blood_to_the_victim() {
    let fixture = Fixture::create().await;

    let first = fixture.insert(200, 400).await.unwrap();
    assert!(first.broadcast_ok);
    assert_eq!(first.attacker_team, "attacker");
    assert_eq!(first.victim_team.as_deref(), Some("victim-a"));
    assert_eq!(first.challenge_title, "service-a");
    assert!(first.first_blood);

    assert!(fixture.insert(200, 400).await.is_none(), "dedup lost");
    assert!(!fixture.insert(200, 401).await.unwrap().first_blood);
    assert!(
        fixture.insert(201, 402).await.unwrap().first_blood,
        "another victim must get its own FirstBlood"
    );
    assert!(
        fixture.insert(202, 403).await.unwrap().first_blood,
        "another challenge must get its own FirstBlood"
    );

    sqlx::query(r#"UPDATE "Games" SET hidden = TRUE WHERE id = $1"#)
        .bind(GAME_ID)
        .execute(&fixture.pool)
        .await
        .unwrap();
    let hidden = fixture.insert(202, 404).await.unwrap();
    assert!(!hidden.broadcast_ok);
    assert!(!hidden.first_blood);

    sqlx::query(
        r#"UPDATE "Games"
              SET hidden = FALSE, freeze_time_utc = now() - interval '1 minute'
            WHERE id = $1"#,
    )
    .bind(GAME_ID)
    .execute(&fixture.pool)
    .await
    .unwrap();
    let frozen = fixture.insert(202, 405).await.unwrap();
    assert!(!frozen.broadcast_ok);
    assert!(!frozen.first_blood);

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn concurrent_duplicate_insert_has_exactly_one_winner() {
    let fixture = Fixture::create().await;
    let left_pool = fixture.pool.clone();
    let right_pool = fixture.pool.clone();
    let insert = |pool: sqlx::PgPool| async move {
        let mut connection = pool.acquire().await.unwrap();
        insert_accepted_attack_on(&mut connection, ATTACKER_ID, 200, 406, GAME_ID)
            .await
            .unwrap()
    };
    let (left, right) = tokio::join!(insert(left_pool), insert(right_pool));
    assert_eq!(
        usize::from(left.is_some()) + usize::from(right.is_some()),
        1
    );
    let count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint FROM "AdAttacks"
            WHERE attacker_participation_id = $1 AND flag_id = 406"#,
    )
    .bind(ATTACKER_ID)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(count, 1);

    fixture.cleanup().await;
}
