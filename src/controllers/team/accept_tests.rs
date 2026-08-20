use super::*;

use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn deconfirmation_and_invite_accept_have_a_locked_stamp_handoff() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect test database");
    let schema = format!("rsctf_accept_fence_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin_pool)
        .await
        .expect("create isolated test schema");
    let options = PgConnectOptions::from_str(&database_url)
        .expect("parse test database URL")
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .expect("connect isolated test pool");
    sqlx::raw_sql(
        r#"
        CREATE TABLE "AspNetUsers" (
          id UUID PRIMARY KEY,
          role SMALLINT NOT NULL,
          email_confirmed BOOLEAN NOT NULL,
          security_stamp TEXT
        );
        "#,
    )
    .execute(&pool)
    .await
    .expect("create account fixture table");

    let user_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "AspNetUsers" (id, role, email_confirmed, security_stamp)
           VALUES ($1, $2, TRUE, 'old-stamp')"#,
    )
    .bind(user_id)
    .bind(crate::utils::enums::Role::User as i16)
    .execute(&pool)
    .await
    .unwrap();

    let mut accepting = pool.begin().await.unwrap();
    lock_live_roster_account(&mut accepting, user_id, "old-stamp")
        .await
        .expect("live account should pass invite authorization");
    let mut deleting = tokio::spawn({
        let pool = pool.clone();
        async move {
            sqlx::query(
                r#"UPDATE "AspNetUsers"
                      SET email_confirmed = FALSE, security_stamp = 'new-stamp'
                    WHERE id = $1"#,
            )
            .bind(user_id)
            .execute(&pool)
            .await
        }
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut deleting)
            .await
            .is_err(),
        "account deconfirmation passed an invite accept retaining FOR SHARE"
    );
    accepting.commit().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), deleting)
        .await
        .expect("deletion remained blocked after accept committed")
        .expect("deletion task failed")
        .expect("deletion update failed");

    let mut rejected = pool.begin().await.unwrap();
    let error = lock_live_roster_account(&mut rejected, user_id, "old-stamp")
        .await
        .expect_err("a post-fence invite accept must fail");
    assert_eq!(error.status(), axum::http::StatusCode::FORBIDDEN);
    rejected.rollback().await.unwrap();

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin_pool)
        .await
        .expect("drop isolated test schema");
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn policy_writer_cannot_deadlock_team_accept_against_game_join() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("rsctf_accept_order_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin_pool)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(6)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TABLE "Configs" (config_key TEXT PRIMARY KEY, value TEXT, cache_keys TEXT);
        CREATE TABLE "Games" (
          id INTEGER PRIMARY KEY, end_time_utc TIMESTAMPTZ NOT NULL,
          ad_scoring_start_round INTEGER, koth_scoring_start_round INTEGER
        );
        CREATE TABLE "Teams" (
          id INTEGER PRIMARY KEY, captain_id UUID NOT NULL,
          locked BOOLEAN NOT NULL DEFAULT FALSE
        );
        CREATE TABLE "TeamMembers" (team_id INTEGER NOT NULL, user_id UUID NOT NULL);
        CREATE TABLE "Participations" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
          team_id INTEGER NOT NULL, status SMALLINT NOT NULL
        );
        CREATE TABLE "AdRounds" (
          game_id INTEGER NOT NULL, finalized BOOLEAN NOT NULL DEFAULT FALSE
        );
        CREATE TABLE "IdentityObservations" (
          id BIGSERIAL PRIMARY KEY, user_id UUID NOT NULL,
          team_id INTEGER, game_id INTEGER, participation_id INTEGER,
          kind TEXT NOT NULL, value_hash BYTEA NOT NULL,
          subnet_group_hash BYTEA, broad_network_hash BYTEA,
          value_hint TEXT NOT NULL, source TEXT NOT NULL,
          observed_at_utc TIMESTAMPTZ NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let game_id = (Uuid::new_v4().as_u128() % 1_000_000_000) as i32 + 1;
    let team_id = game_id + 1;
    let captain = Uuid::new_v4();
    let joining_user = Uuid::new_v4();
    let game_join_user = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "Games" (id,end_time_utc)
           VALUES ($1,clock_timestamp()+interval '1 hour')"#,
    )
    .bind(game_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "Teams" (id,captain_id) VALUES ($1,$2)"#)
        .bind(team_id)
        .bind(captain)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "Participations" (id,game_id,team_id,status)
           VALUES ($1,$2,$3,$4)"#,
    )
    .bind(game_id + 2)
    .bind(game_id)
    .bind(team_id)
    .bind(crate::utils::enums::ParticipationStatus::Pending as i16)
    .execute(&pool)
    .await
    .unwrap();
    let mut config = crate::models::internal::configs::AppConfig::from_env();
    config.identity_hash_key = "identity-lock-order-test-key-012345".to_string();

    // T1 models game registration: shared policy + identity locks are held,
    // but it has not yet acquired the target Game advisory lock.
    let mut game_join = pool.begin().await.unwrap();
    let _scope = crate::services::anti_cheat::lock_game_join_identity_scope(
        &mut game_join,
        &config,
        game_join_user,
        Some("192.0.2.61"),
        None,
    )
    .await
    .unwrap();

    // Queue an exclusive policy writer behind T1's shared policy lock.
    let (writer_acquired_tx, writer_acquired_rx) = tokio::sync::oneshot::channel();
    let (release_writer_tx, release_writer_rx) = tokio::sync::oneshot::channel();
    let writer_pool = pool.clone();
    let writer = tokio::spawn(async move {
        let mut transaction = writer_pool.begin().await.unwrap();
        crate::services::anti_cheat::lock_policy_update(&mut transaction)
            .await
            .unwrap();
        writer_acquired_tx.send(()).unwrap();
        release_writer_rx.await.unwrap();
        transaction.commit().await.unwrap();
    });
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;

    // T2 is the production accept helper. It must wait for shared policy
    // access before it can take the Game lock; the old Games->policy order
    // deadlocked this exact three-actor schedule.
    let accept_pool = pool.clone();
    let accept_config = config.clone();
    let accept = tokio::spawn(async move {
        let mut transaction = accept_pool.begin().await.unwrap();
        let outcome = admit_team_member_with_roster_fence(
            &mut transaction,
            &accept_config,
            joining_user,
            Some("joining"),
            team_id,
            Some("198.51.100.61"),
            None,
        )
        .await?;
        transaction.commit().await.unwrap();
        Ok::<_, AppError>(outcome)
    });
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    assert!(
        !accept.is_finished(),
        "accept bypassed the queued policy writer"
    );

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        crate::utils::single_flight::acquire_transaction_advisory_lock(
            &mut game_join,
            &crate::services::ad_engine::game_lock_key(game_id),
        ),
    )
    .await
    .expect("game join deadlocked behind team accept")
    .unwrap();
    game_join.commit().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), writer_acquired_rx)
        .await
        .expect("policy writer did not acquire after game join")
        .unwrap();
    release_writer_tx.send(()).unwrap();
    writer.await.unwrap();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(2), accept)
            .await
            .expect("team accept remained blocked")
            .unwrap()
            .unwrap(),
        crate::services::anti_cheat::AdmissionOutcome::Accepted
    );

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin_pool)
        .await
        .unwrap();
}
