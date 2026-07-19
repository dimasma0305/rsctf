use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::sync::oneshot;
use uuid::Uuid;

use super::{submit_caller_is_live, AdSubmitCaller};
use crate::models::data::participation;
use crate::utils::enums::{ParticipationStatus, Role};
use crate::utils::single_flight::PgAdvisoryLock;

const TEAM_ID: i32 = 7;
const GAME_ID: i32 = 11;
const PARTICIPATION_ID: i32 = 13;

struct Fixture {
    admin: sqlx::PgPool,
    pool: sqlx::PgPool,
    schema: String,
    member: Uuid,
    token: String,
    participation: participation::Model,
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
        let schema = format!("rsctf_ad_submit_fence_{}", Uuid::new_v4().simple());
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
            CREATE TABLE "AspNetUsers" (
              id UUID PRIMARY KEY,
              role SMALLINT NOT NULL
            );
            CREATE TABLE "Teams" (
              id INTEGER PRIMARY KEY,
              captain_id UUID NOT NULL,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "TeamMembers" (
              team_id INTEGER NOT NULL,
              user_id UUID NOT NULL
            );
            CREATE TABLE "Participations" (
              id INTEGER PRIMARY KEY,
              status SMALLINT NOT NULL,
              token TEXT NOT NULL,
              writeup_id INTEGER,
              game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL,
              division_id INTEGER,
              suspicion_score INTEGER NOT NULL
            );
            CREATE TABLE "UserParticipations" (
              user_id UUID NOT NULL,
              game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL
            );
            CREATE TABLE "AdTeamApiTokens" (
              id SERIAL PRIMARY KEY,
              participation_id INTEGER NOT NULL,
              token_hash TEXT NOT NULL,
              last_used_at_utc TIMESTAMPTZ
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let captain = Uuid::new_v4();
        let member = Uuid::new_v4();
        for id in [captain, member] {
            sqlx::query(r#"INSERT INTO "AspNetUsers" (id, role) VALUES ($1, $2)"#)
                .bind(id)
                .bind(Role::User as i16)
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query(r#"INSERT INTO "Teams" (id, captain_id) VALUES ($1, $2)"#)
            .bind(TEAM_ID)
            .bind(captain)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES ($1, $2)"#)
            .bind(TEAM_ID)
            .bind(member)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "Participations"
                 (id, status, token, game_id, team_id, suspicion_score)
               VALUES ($1, $2, 'participation-token', $3, $4, 0)"#,
        )
        .bind(PARTICIPATION_ID)
        .bind(ParticipationStatus::Accepted as i16)
        .bind(GAME_ID)
        .bind(TEAM_ID)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "UserParticipations"
                 (user_id, game_id, team_id, participation_id)
               VALUES ($1, $2, $3, $4)"#,
        )
        .bind(member)
        .bind(GAME_ID)
        .bind(TEAM_ID)
        .bind(PARTICIPATION_ID)
        .execute(&pool)
        .await
        .unwrap();
        let token = format!("ad_{}", "a".repeat(43));
        sqlx::query(
            r#"INSERT INTO "AdTeamApiTokens" (participation_id, token_hash)
               VALUES ($1, $2)"#,
        )
        .bind(PARTICIPATION_ID)
        .bind(crate::services::ad::api_token::hash(&token))
        .execute(&pool)
        .await
        .unwrap();

        Self {
            admin,
            pool,
            schema,
            member,
            token,
            participation: participation::Model {
                id: PARTICIPATION_ID,
                status: ParticipationStatus::Accepted,
                token: "participation-token".to_string(),
                writeup_id: None,
                game_id: GAME_ID,
                team_id: TEAM_ID,
                division_id: None,
                suspicion_score: 0,
            },
        }
    }

    fn roster_key(&self) -> String {
        // Advisory locks are database-global rather than schema-scoped. Include
        // the disposable schema so these independently isolated tests may run
        // in parallel without contending on the same synthetic team id.
        format!("{}:team-roster:{TEAM_ID}", self.schema)
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
async fn token_revocation_waits_for_the_complete_fenced_batch() {
    let fixture = Fixture::create().await;
    let caller = AdSubmitCaller::TeamToken(fixture.token.clone());
    let mut reader = PgAdvisoryLock::try_acquire_shared(&fixture.pool, &fixture.roster_key())
        .await
        .unwrap()
        .unwrap();
    assert!(
        submit_caller_is_live(reader.transaction_mut(), &caller, &fixture.participation,)
            .await
            .unwrap()
    );

    let (started_tx, started_rx) = oneshot::channel();
    let pool = fixture.pool.clone();
    let roster_key = fixture.roster_key();
    let mut revoker = tokio::spawn(async move {
        let _ = started_tx.send(());
        let mut writer = PgAdvisoryLock::acquire(&pool, &roster_key).await.unwrap();
        sqlx::query(r#"DELETE FROM "AdTeamApiTokens" WHERE participation_id = $1"#)
            .bind(PARTICIPATION_ID)
            .execute(&mut **writer.transaction_mut())
            .await
            .unwrap();
        writer.release().await.unwrap();
    });
    started_rx.await.unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(75), &mut revoker)
            .await
            .is_err(),
        "token revocation crossed a live submit read fence"
    );
    reader.release().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), revoker)
        .await
        .unwrap()
        .unwrap();

    let mut stale = PgAdvisoryLock::try_acquire_shared(&fixture.pool, &fixture.roster_key())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !submit_caller_is_live(stale.transaction_mut(), &caller, &fixture.participation,)
            .await
            .unwrap(),
        "a middleware-authenticated token survived authoritative revocation"
    );
    stale.release().await.unwrap();
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn session_batch_locks_account_role_and_rejects_an_orphaned_link() {
    let fixture = Fixture::create().await;
    let caller = AdSubmitCaller::Session(fixture.member);
    let mut reader = PgAdvisoryLock::try_acquire_shared(&fixture.pool, &fixture.roster_key())
        .await
        .unwrap()
        .unwrap();
    assert!(
        submit_caller_is_live(reader.transaction_mut(), &caller, &fixture.participation,)
            .await
            .unwrap()
    );

    let (started_tx, started_rx) = oneshot::channel();
    let pool = fixture.pool.clone();
    let member = fixture.member;
    let mut banner = tokio::spawn(async move {
        let _ = started_tx.send(());
        sqlx::query(r#"UPDATE "AspNetUsers" SET role = $1 WHERE id = $2"#)
            .bind(Role::Banned as i16)
            .bind(member)
            .execute(&pool)
            .await
            .unwrap();
    });
    started_rx.await.unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(75), &mut banner)
            .await
            .is_err(),
        "account ban crossed the submit transaction's roster row locks"
    );
    reader.release().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), banner)
        .await
        .unwrap()
        .unwrap();

    let mut banned = PgAdvisoryLock::try_acquire_shared(&fixture.pool, &fixture.roster_key())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !submit_caller_is_live(banned.transaction_mut(), &caller, &fixture.participation,)
            .await
            .unwrap()
    );
    banned.release().await.unwrap();

    sqlx::query(r#"UPDATE "AspNetUsers" SET role = $1 WHERE id = $2"#)
        .bind(Role::User as i16)
        .bind(fixture.member)
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query(r#"DELETE FROM "TeamMembers" WHERE team_id = $1 AND user_id = $2"#)
        .bind(TEAM_ID)
        .bind(fixture.member)
        .execute(&fixture.pool)
        .await
        .unwrap();

    let mut orphan = PgAdvisoryLock::try_acquire_shared(&fixture.pool, &fixture.roster_key())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !submit_caller_is_live(orphan.transaction_mut(), &caller, &fixture.participation,)
            .await
            .unwrap(),
        "a stale UserParticipations row restored interactive submit authority"
    );
    orphan.release().await.unwrap();
    fixture.cleanup().await;
}
