//! Exercise registered role consumers against durable rows, including recovery.
use super::*;
use crate::{app_state::AppState, models::internal::configs::AppConfig};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

async fn queue(pool: &sqlx::PgPool, expired: bool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(r#"INSERT INTO "ControlPlaneJobs"
        (id, kind, scope_key, game_id, operation_id, fingerprint, status, lease_token, lease_expires_at_utc)
        VALUES ($1, 'BuildBatch', $2, 1, $1, $3, $4, $5, $6)"#)
        .bind(id).bind(id.to_string()).bind("a".repeat(64))
        .bind(if expired { 1_i16 } else { 0_i16 })
        .bind(expired.then(Uuid::new_v4))
        .bind(expired.then(|| chrono::Utc::now() - chrono::Duration::seconds(1)))
        .execute(pool).await.unwrap();
    id
}

async fn succeeded(pool: &sqlx::PgPool, id: Uuid) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let job = services::control_jobs::get(pool, id)
                .await
                .unwrap()
                .unwrap();
            assert_ne!(
                job.status,
                services::control_jobs::ControlJobStatus::Failed,
                "{:?}",
                job.error
            );
            if job.status == services::control_jobs::ControlJobStatus::Succeeded {
                assert_eq!(job.result.unwrap()["enqueued"], 0);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("eligible role must consume durable work");
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn registered_owners_consume_recover_and_stop_claiming_on_shutdown() {
    let db = crate::utils::test_database::TestDatabase::new("role_consumption").await;
    sqlx::raw_sql(r#"
        CREATE TABLE "Games" (id integer PRIMARY KEY);
        CREATE TABLE "GameChallenges" (id integer PRIMARY KEY, game_id integer, deletion_pending boolean, build_status smallint);
        CREATE TABLE "Participations" (id integer PRIMARY KEY);
        INSERT INTO "Games" VALUES (1);
    "#).execute(&db.pool).await.unwrap();
    sqlx::raw_sql(crate::migrations::CONTROL_JOBS_SQL)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::raw_sql(crate::migrations::CONTROL_JOB_CANCELLATION_SQL)
        .execute(&db.pool)
        .await
        .unwrap();
    for role in [
        RuntimeRole::All,
        RuntimeRole::Control,
        RuntimeRole::Engine,
        RuntimeRole::Development,
    ] {
        let mut config = AppConfig::default();
        config.runtime_role = role;
        let state = AppState::new(
            sea_orm::SqlxPostgresConnector::from_sqlx_postgres_pool(db.pool.clone()),
            Arc::new(config),
            Arc::new(services::cache::InMemoryCache::new()),
            Arc::new(crate::storage::LocalBlobStorage::new(
                std::env::temp_dir().join(format!("unused-role-{}", Uuid::new_v4())),
            )),
            services::token::TokenService::new("0123456789abcdef0123456789abcdef", 60),
            Arc::new(services::container::NoopContainerManager),
        );
        let consumer = *for_role(role)
            .iter()
            .find(|kind| **kind == DurableWorker::ControlJobs)
            .unwrap();
        let fresh = queue(&db.pool, false).await;
        let recovered = queue(&db.pool, true).await;
        let (stop, signal) = watch::channel(false);
        let handle = consumer.start(state.clone(), signal);
        succeeded(&db.pool, fresh).await;
        succeeded(&db.pool, recovered).await;
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .unwrap()
            .unwrap();
        let after_stop = queue(&db.pool, false).await;
        services::control_jobs::kick(state.clone());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            services::control_jobs::get(&db.pool, after_stop)
                .await
                .unwrap()
                .unwrap()
                .status,
            services::control_jobs::ControlJobStatus::Queued
        );
        let (stop, signal) = watch::channel(false);
        let restarted = consumer.start(state, signal);
        succeeded(&db.pool, after_stop).await;
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), restarted)
            .await
            .unwrap()
            .unwrap();
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT count(*) FROM "ControlPlaneJobs" WHERE status = 2"#)
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        12
    );
    db.close().await;
}
