use std::net::Ipv4Addr;

use ipnet::Ipv4Net;
use sea_orm::DatabaseConnection;

use super::{enabled, ensure_hub_and_sync, service_route_cidrs, sync_is_dirty, SYNC_DIRTY};
use crate::utils::error::{AppError, AppResult};

/// Remove a platform service endpoint from the routable capability set before
/// its backing container is destroyed and its IP can be reused. Keep the
/// backend identity until the traffic-capture owner acknowledges that its old
/// filter is gone; `stop_container_capture` clears this retained pointer.
pub async fn deactivate_team_service(db: &DatabaseConnection, service_id: i32) -> AppResult<()> {
    if enabled() {
        SYNC_DIRTY.store(true, std::sync::atomic::Ordering::Release);
    }
    sqlx::query(
        r#"
        UPDATE "AdTeamServices"
           SET host = '', port = 0, status = 2
         WHERE id = $1
        "#,
    )
    .bind(service_id)
    .execute(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    ensure_hub_and_sync(db).await
}

fn unique_backend_ids(backend_ids: &[String]) -> Vec<String> {
    let mut unique = backend_ids
        .iter()
        .filter(|backend_id| !backend_id.is_empty())
        .cloned()
        .collect::<Vec<_>>();
    unique.sort_unstable();
    unique.dedup();
    unique
}

async fn deactivate_backend_endpoints_in(
    connection: &mut sqlx::PgConnection,
    backend_ids: &[String],
) -> AppResult<u64> {
    let services = sqlx::query(
        r#"
        UPDATE "AdTeamServices"
           SET host = '', port = 0, status = 2
         WHERE container_id = ANY($1)
        "#,
    )
    .bind(backend_ids)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    let hills = sqlx::query(
        r#"
        UPDATE "KothTargets"
           SET host = '', port = 0, container_id = NULL,
               holder_participation_id = NULL, held_since = NULL
         WHERE container_id = ANY($1)
        "#,
    )
    .bind(backend_ids)
    .execute(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    Ok(services + hills)
}

/// Clear every A&D/KotH endpoint backed by the supplied runtime identities and
/// reconcile the resulting policy exactly once. Duplicate identities are
/// collapsed before the atomic database update.
pub async fn deactivate_backend_endpoints(
    db: &DatabaseConnection,
    backend_ids: &[String],
) -> AppResult<u64> {
    let backend_ids = unique_backend_ids(backend_ids);
    if backend_ids.is_empty() {
        return Ok(0);
    }
    if enabled() {
        // Set this before the write so a failed reconciliation remains
        // explicitly retryable even when the next UPDATE is already a no-op.
        SYNC_DIRTY.store(true, std::sync::atomic::Ordering::Release);
    }
    let mut transaction =
        crate::utils::database::begin_sqlx_transaction(db.get_postgres_connection_pool())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    let changed = deactivate_backend_endpoints_in(&mut transaction, &backend_ids).await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    ensure_hub_and_sync(db).await?;
    Ok(changed)
}

/// Clear any A&D/KotH endpoint backed by one generic Containers runtime id.
/// Kept as the stable single-runtime API for existing teardown call sites.
pub async fn deactivate_backend_endpoint(
    db: &DatabaseConnection,
    backend_id: &str,
) -> AppResult<u64> {
    deactivate_backend_endpoints(db, &[backend_id.to_string()]).await
}

/// Revoke every service owned by participations before team/roster deletion.
/// Return backend ids only after the new empty endpoint set reaches the kernel.
pub async fn deactivate_participation_services(
    db: &DatabaseConnection,
    participation_ids: &[i32],
) -> AppResult<Vec<String>> {
    if participation_ids.is_empty() {
        return Ok(Vec::new());
    }
    let backend_ids = sqlx::query_scalar::<_, Option<String>>(
        r#"
        SELECT container_id
          FROM "AdTeamServices"
         WHERE participation_id = ANY($1)
           AND container_id IS NOT NULL
        "#,
    )
    .bind(participation_ids)
    .fetch_all(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if enabled() {
        SYNC_DIRTY.store(true, std::sync::atomic::Ordering::Release);
    }
    let changed = sqlx::query(
        r#"
        UPDATE "AdTeamServices"
           SET host = '', port = 0, status = 2
         WHERE participation_id = ANY($1)
        "#,
    )
    .bind(participation_ids)
    .execute(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    if changed > 0 || (enabled() && sync_is_dirty()) {
        ensure_hub_and_sync(db).await?;
    }
    Ok(backend_ids)
}

/// In-process relay listeners do not survive an application restart. Remove
/// their persisted service-CIDR endpoints before the first firewall build so an
/// ephemeral port reused by another local listener is never exposed as BYOC.
pub async fn clear_stale_local_relays(db: &DatabaseConnection) -> AppResult<u64> {
    let service_networks: Vec<Ipv4Net> = service_route_cidrs()
        .map_err(AppError::internal)?
        .iter()
        .filter_map(|cidr| cidr.parse().ok())
        .collect();
    let rows = sqlx::query_as::<_, (i32, String)>(
        r#"
        SELECT service.id, service.host
          FROM "AdTeamServices" service
          JOIN "GameChallenges" challenge ON challenge.id = service.challenge_id
         WHERE service.container_id IS NULL
           AND challenge.ad_self_hosted = TRUE
           AND service.host <> ''
        "#,
    )
    .fetch_all(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let stale_ids: Vec<i32> = rows
        .into_iter()
        .filter_map(|(id, host)| {
            host.parse::<Ipv4Addr>()
                .ok()
                .filter(|address| {
                    service_networks
                        .iter()
                        .any(|network| network.contains(address))
                })
                .map(|_| id)
        })
        .collect();
    if stale_ids.is_empty() {
        return Ok(0);
    }
    let result = sqlx::query(
        r#"
        UPDATE "AdTeamServices"
           SET host = '', port = 0, status = 2
         WHERE id = ANY($1)
           AND container_id IS NULL
        "#,
    )
    .bind(&stale_ids)
    .execute(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::{deactivate_backend_endpoints_in, unique_backend_ids};

    #[test]
    fn backend_identity_batch_drops_empty_values_and_duplicates() {
        assert_eq!(
            unique_backend_ids(&[
                "runtime-b".to_string(),
                String::new(),
                "runtime-a".to_string(),
                "runtime-b".to_string(),
            ]),
            ["runtime-a".to_string(), "runtime-b".to_string()]
        );
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn backend_identity_batch_retains_identity_until_capture_fence() {
        use sqlx::{Connection, PgConnection};

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
        let mut connection = PgConnection::connect(&database_url).await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "AdTeamServices" (
              id INTEGER PRIMARY KEY, host TEXT NOT NULL, port INTEGER NOT NULL,
              container_id TEXT, status SMALLINT NOT NULL
            );
            CREATE TEMP TABLE "KothTargets" (
              id INTEGER PRIMARY KEY, host TEXT NOT NULL, port INTEGER NOT NULL,
              container_id TEXT, holder_participation_id INTEGER,
              held_since TIMESTAMPTZ
            );
            INSERT INTO "AdTeamServices" VALUES
              (1, '10.0.0.1', 80, 'runtime-a', 1),
              (2, '10.0.0.2', 80, 'runtime-c', 1);
            INSERT INTO "KothTargets" VALUES
              (1, '10.0.0.3', 80, 'runtime-b', 9, clock_timestamp()),
              (2, '10.0.0.4', 80, 'runtime-c', 10, clock_timestamp());
            "#,
        )
        .execute(&mut connection)
        .await
        .unwrap();

        let changed = deactivate_backend_endpoints_in(
            &mut connection,
            &[
                "runtime-b".to_string(),
                "runtime-a".to_string(),
                "runtime-b".to_string(),
            ],
        )
        .await
        .unwrap();
        assert_eq!(changed, 2);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"SELECT COUNT(*) FROM "AdTeamServices"
                    WHERE container_id = 'runtime-a'
                      AND host = '' AND port = 0 AND status = 2"#,
            )
            .fetch_one(&mut connection)
            .await
            .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, Option<String>>(
                r#"SELECT container_id FROM "AdTeamServices" WHERE id = 1"#,
            )
            .fetch_one(&mut connection)
            .await
            .unwrap()
            .as_deref(),
            Some("runtime-a"),
            "a failed or timed-out capture fence must leave a retryable identity"
        );

        let cleared = sqlx::query(
            r#"UPDATE "AdTeamServices"
                  SET container_id = NULL
                WHERE container_id = $1
                  AND NULLIF(BTRIM(host), '') IS NULL
                  AND port = 0"#,
        )
        .bind("runtime-a")
        .execute(&mut connection)
        .await
        .unwrap();
        assert_eq!(cleared.rows_affected(), 1);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"SELECT COUNT(*) FROM "AdTeamServices"
                    WHERE id = 1 AND container_id IS NULL"#,
            )
            .fetch_one(&mut connection)
            .await
            .unwrap(),
            1,
            "the successful capture fence may clear the inactive pointer"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"SELECT COUNT(*) FROM "KothTargets"
                    WHERE container_id IS NULL AND host = '' AND port = 0
                      AND holder_participation_id IS NULL AND held_since IS NULL"#,
            )
            .fetch_one(&mut connection)
            .await
            .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"SELECT COUNT(*) FROM (
                     SELECT container_id FROM "AdTeamServices"
                     UNION ALL SELECT container_id FROM "KothTargets"
                   ) endpoint WHERE endpoint.container_id = 'runtime-c'"#,
            )
            .fetch_one(&mut connection)
            .await
            .unwrap(),
            2
        );
    }
}
