use super::*;

use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use crate::utils::enums::Role;

async fn attempt_leave(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    security_stamp: &str,
    game_id: i32,
    team_id: i32,
    participation_id: i32,
) -> AppResult<()> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let result = leave_game_membership_locked(
        &mut transaction,
        user_id,
        security_stamp,
        game_id,
        team_id,
        participation_id,
    )
    .await;
    match result {
        Ok(()) => transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string())),
        Err(error) => {
            transaction
                .rollback()
                .await
                .map_err(|rollback| AppError::internal(rollback.to_string()))?;
            Err(error)
        }
    }
}

async fn assert_historical_link(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    game_id: i32,
    participation_id: i32,
) {
    let stored: Option<i32> = sqlx::query_scalar(
        r#"SELECT participation_id FROM "UserParticipations"
            WHERE user_id = $1 AND game_id = $2"#,
    )
    .bind(user_id)
    .bind(game_id)
    .fetch_optional(pool)
    .await
    .unwrap();
    assert_eq!(stored, Some(participation_id));
}

#[test]
fn leave_contract_revalidates_account_roster_and_evidence_before_delete() {
    let source = include_str!("membership.rs");
    assert!(source.contains("lock_live_request_account"));
    assert!(source.contains("participation_caller_is_live_on"));
    assert!(source.contains("has_competition_evidence"));
    assert!(source.contains("Cannot leave after competition evidence has been recorded"));
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn stale_account_roster_removal_and_evidence_preserve_the_link() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("leave_fence_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();

    sqlx::raw_sql(
        r#"
        CREATE TABLE "Games" (
          id INTEGER PRIMARY KEY,
          start_time_utc TIMESTAMPTZ NOT NULL,
          end_time_utc TIMESTAMPTZ NOT NULL,
          deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
        );
        CREATE TABLE "Teams" (
          id INTEGER PRIMARY KEY,
          captain_id UUID NOT NULL,
          deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
        );
        CREATE TABLE "TeamMembers" (
          team_id INTEGER NOT NULL,
          user_id UUID NOT NULL,
          PRIMARY KEY (team_id, user_id)
        );
        CREATE TABLE "AspNetUsers" (
          id UUID PRIMARY KEY,
          email_confirmed BOOLEAN NOT NULL,
          role SMALLINT NOT NULL,
          security_stamp TEXT
        );
        CREATE TABLE "Participations" (
          id INTEGER PRIMARY KEY,
          status SMALLINT NOT NULL,
          game_id INTEGER NOT NULL,
          team_id INTEGER NOT NULL,
          writeup_id INTEGER
        );
        CREATE TABLE "UserParticipations" (
          user_id UUID NOT NULL,
          game_id INTEGER NOT NULL,
          team_id INTEGER NOT NULL,
          participation_id INTEGER NOT NULL,
          PRIMARY KEY (user_id, game_id)
        );
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    crate::services::participation_evidence::create_test_evidence_tables(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        ALTER TABLE "IdentityObservations" ADD COLUMN user_id UUID;
        ALTER TABLE "IdentityObservations" ADD COLUMN game_id INTEGER;
        ALTER TABLE "IdentityObservations" ADD COLUMN team_id INTEGER;
        ALTER TABLE "IdentityObservations" ADD COLUMN observed_at_utc TIMESTAMPTZ;
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let user_id = Uuid::new_v4();
    let captain_id = Uuid::new_v4();
    let game_id = 71;
    let team_id = 72;
    let participation_id = 73;
    sqlx::query(
        r#"INSERT INTO "Games" (id, start_time_utc, end_time_utc)
           VALUES ($1, now() - interval '1 minute', now() + interval '1 hour')"#,
    )
    .bind(game_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "Teams" (id, captain_id) VALUES ($1, $2)"#)
        .bind(team_id)
        .bind(captain_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "AspNetUsers" (id, email_confirmed, role, security_stamp)
           VALUES ($1, TRUE, $2, 'live-stamp')"#,
    )
    .bind(user_id)
    .bind(Role::User as i16)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES ($1, $2)"#)
        .bind(team_id)
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "Participations" (id, status, game_id, team_id)
           VALUES ($1, $2, $3, $4)"#,
    )
    .bind(participation_id)
    .bind(ParticipationStatus::Pending as i16)
    .bind(game_id)
    .bind(team_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "UserParticipations"
             (user_id, game_id, team_id, participation_id)
           VALUES ($1, $2, $3, $4)"#,
    )
    .bind(user_id)
    .bind(game_id)
    .bind(team_id)
    .bind(participation_id)
    .execute(&pool)
    .await
    .unwrap();

    let stale = attempt_leave(
        &pool,
        user_id,
        "stale-stamp",
        game_id,
        team_id,
        participation_id,
    )
    .await
    .unwrap_err();
    assert_eq!(stale.status(), axum::http::StatusCode::UNAUTHORIZED);
    assert_historical_link(&pool, user_id, game_id, participation_id).await;

    sqlx::query(r#"UPDATE "AspNetUsers" SET role = $2 WHERE id = $1"#)
        .bind(user_id)
        .bind(Role::Banned as i16)
        .execute(&pool)
        .await
        .unwrap();
    let banned = attempt_leave(
        &pool,
        user_id,
        "live-stamp",
        game_id,
        team_id,
        participation_id,
    )
    .await
    .unwrap_err();
    assert_eq!(banned.status(), axum::http::StatusCode::UNAUTHORIZED);
    assert_historical_link(&pool, user_id, game_id, participation_id).await;

    sqlx::query(r#"UPDATE "AspNetUsers" SET role = $2 WHERE id = $1"#)
        .bind(user_id)
        .bind(Role::User as i16)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"DELETE FROM "TeamMembers" WHERE team_id = $1 AND user_id = $2"#)
        .bind(team_id)
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    let removed = attempt_leave(
        &pool,
        user_id,
        "live-stamp",
        game_id,
        team_id,
        participation_id,
    )
    .await
    .unwrap_err();
    assert_eq!(removed.status(), axum::http::StatusCode::FORBIDDEN);
    assert_historical_link(&pool, user_id, game_id, participation_id).await;

    sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES ($1, $2)"#)
        .bind(team_id)
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Submissions" (participation_id) VALUES ($1)"#)
        .bind(participation_id)
        .execute(&pool)
        .await
        .unwrap();
    let evidenced = attempt_leave(
        &pool,
        user_id,
        "live-stamp",
        game_id,
        team_id,
        participation_id,
    )
    .await
    .unwrap_err();
    assert_eq!(
        evidenced.status(),
        axum::http::StatusCode::BAD_REQUEST,
        "{evidenced:?}"
    );
    assert_eq!(
        evidenced.to_string(),
        "Cannot leave after competition evidence has been recorded"
    );
    assert_historical_link(&pool, user_id, game_id, participation_id).await;

    sqlx::query(r#"DELETE FROM "Submissions" WHERE participation_id = $1"#)
        .bind(participation_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "Participations" SET status = $2 WHERE id = $1"#)
        .bind(participation_id)
        .bind(ParticipationStatus::Rejected as i16)
        .execute(&pool)
        .await
        .unwrap();
    attempt_leave(
        &pool,
        user_id,
        "live-stamp",
        game_id,
        team_id,
        participation_id,
    )
    .await
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "UserParticipations""#)
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "Participations""#)
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}
