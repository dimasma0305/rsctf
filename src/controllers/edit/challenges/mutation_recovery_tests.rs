use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

use super::{
    claim_challenge_create_operation, claim_challenge_update_operation,
    complete_challenge_create_operation, complete_challenge_update_operation, ChallengeUpdateClaim,
};
use crate::controllers::edit::seed_division_configs;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn failed_commit_rolls_back_challenge_policy_and_operation_before_exact_replay() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("challenge_recovery_{}", Uuid::new_v4().simple());
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
        r#"CREATE TABLE "GameChallenges" (
             id SERIAL PRIMARY KEY, game_id INTEGER NOT NULL, title TEXT NOT NULL
           );
           CREATE TABLE "Divisions" (
             id SERIAL PRIMARY KEY, game_id INTEGER NOT NULL,
             default_permissions INTEGER NOT NULL
           );
           CREATE TABLE "DivisionChallengeConfigs" (
             division_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
             permissions INTEGER NOT NULL,
             PRIMARY KEY (division_id, challenge_id)
           );
           CREATE TABLE "ChallengeCreateOperations" (
             actor_id UUID NOT NULL, game_id INTEGER NOT NULL,
             operation_id UUID NOT NULL, request_digest TEXT NOT NULL,
             challenge_id INTEGER, created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
             completed_at_utc TIMESTAMPTZ,
             PRIMARY KEY (actor_id, game_id, operation_id)
           );
           INSERT INTO "Divisions" (game_id, default_permissions)
             VALUES (7, 1), (7, 3);
           CREATE FUNCTION reject_policy_commit()
           RETURNS trigger LANGUAGE plpgsql AS $$
           BEGIN RAISE EXCEPTION 'synthetic policy commit failure'; END $$;
           CREATE CONSTRAINT TRIGGER reject_policy_commit
             AFTER INSERT ON "DivisionChallengeConfigs"
             DEFERRABLE INITIALLY DEFERRED
             FOR EACH ROW EXECUTE FUNCTION reject_policy_commit();"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let actor = Uuid::new_v4();
    let operation = Uuid::new_v4();
    let digest = crate::utils::codec::sha256_str("challenge definition");
    let mut transaction = pool.begin().await.unwrap();
    assert_eq!(
        claim_challenge_create_operation(&mut transaction, actor, 7, operation, &digest,)
            .await
            .unwrap(),
        None
    );
    let challenge_id: i32 = sqlx::query_scalar(
        r#"INSERT INTO "GameChallenges" (game_id, title)
           VALUES (7, 'atomic') RETURNING id"#,
    )
    .fetch_one(&mut *transaction)
    .await
    .unwrap();
    seed_division_configs(&mut transaction, 7, challenge_id)
        .await
        .unwrap();
    complete_challenge_create_operation(&mut transaction, actor, 7, operation, challenge_id)
        .await
        .unwrap();
    assert!(transaction.commit().await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*)::BIGINT FROM "GameChallenges""#)
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*)::BIGINT FROM "ChallengeCreateOperations""#)
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );

    sqlx::raw_sql(
        r#"DROP TRIGGER reject_policy_commit ON "DivisionChallengeConfigs";
           DROP FUNCTION reject_policy_commit();"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut transaction = pool.begin().await.unwrap();
    claim_challenge_create_operation(&mut transaction, actor, 7, operation, &digest)
        .await
        .unwrap();
    let challenge_id: i32 = sqlx::query_scalar(
        r#"INSERT INTO "GameChallenges" (game_id, title)
           VALUES (7, 'atomic') RETURNING id"#,
    )
    .fetch_one(&mut *transaction)
    .await
    .unwrap();
    seed_division_configs(&mut transaction, 7, challenge_id)
        .await
        .unwrap();
    complete_challenge_create_operation(&mut transaction, actor, 7, operation, challenge_id)
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    let mut replay = pool.begin().await.unwrap();
    assert_eq!(
        claim_challenge_create_operation(&mut replay, actor, 7, operation, &digest)
            .await
            .unwrap(),
        Some(challenge_id)
    );
    replay.commit().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*)::BIGINT FROM "DivisionChallengeConfigs"
                WHERE challenge_id = $1"#,
        )
        .bind(challenge_id)
        .fetch_one(&pool)
        .await
        .unwrap(),
        2
    );

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn challenge_update_result_is_atomic_exact_and_actor_scoped() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("challenge_update_recovery_{}", Uuid::new_v4().simple());
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
    sqlx::raw_sql(crate::migrations::CHALLENGE_UPDATE_OPERATIONS_SQL)
        .execute(&pool)
        .await
        .unwrap();

    let actor = Uuid::new_v4();
    let operation = Uuid::new_v4();
    let digest = crate::utils::codec::sha256_str("update-a");
    let mut rolled_back = pool.begin().await.unwrap();
    assert_eq!(
        claim_challenge_update_operation(&mut rolled_back, actor, 7, 9, operation, 4, &digest)
            .await
            .unwrap(),
        ChallengeUpdateClaim::Pending
    );
    complete_challenge_update_operation(&mut rolled_back, actor, operation, 5)
        .await
        .unwrap();
    rolled_back.rollback().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*)::BIGINT FROM "ChallengeUpdateOperations""#)
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );

    let mut committed = pool.begin().await.unwrap();
    claim_challenge_update_operation(&mut committed, actor, 7, 9, operation, 4, &digest)
        .await
        .unwrap();
    complete_challenge_update_operation(&mut committed, actor, operation, 5)
        .await
        .unwrap();
    committed.commit().await.unwrap();

    let mut replay = pool.begin().await.unwrap();
    assert_eq!(
        claim_challenge_update_operation(&mut replay, actor, 7, 9, operation, 4, &digest)
            .await
            .unwrap(),
        ChallengeUpdateClaim::Completed(5)
    );
    replay.commit().await.unwrap();
    let mut conflict = pool.begin().await.unwrap();
    assert!(claim_challenge_update_operation(
        &mut conflict,
        actor,
        7,
        9,
        operation,
        4,
        &crate::utils::codec::sha256_str("update-b"),
    )
    .await
    .is_err());
    conflict.rollback().await.unwrap();

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
