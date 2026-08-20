//! Atomic container-access evidence ingestion.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::{GameAccess, InstanceAccess};
use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

const INSERT_ACCESS_EVENT_SQL: &str = r#"
    INSERT INTO "ContainerAccessEvents"
        (game_id, challenge_id, container_owner_participation_id,
         container_id, accessing_user_id, accessing_user_name,
         accessing_participation_id, remote_ip, remote_ip_hash, user_agent,
         is_monitor, connected_at_utc)
    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
    RETURNING id
"#;

struct AccessAudit<'a> {
    game_id: i32,
    challenge_id: i32,
    owner_participation_id: i32,
    accessing_participation_id: i32,
    container_id: Uuid,
    accessing_user_id: Uuid,
    accessing_user_name: &'a str,
    remote_ip: &'a str,
    remote_ip_hash: Option<&'a [u8]>,
    user_agent: Option<&'a str>,
    is_monitor: bool,
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

#[cfg(test)]
async fn persist_access_audit(pool: &sqlx::PgPool, audit: AccessAudit<'_>) -> AppResult<()> {
    let mut transaction = pool.begin().await.map_err(database_error)?;
    crate::services::suspicion::lock_participation_suspicion_writes(
        &mut transaction,
        audit.accessing_participation_id,
    )
    .await
    .map_err(database_error)?;
    persist_access_audit_after_suspicion(&mut transaction, audit).await?;
    transaction.commit().await.map_err(database_error)
}

/// Stage one exact access source and any applicable suspicion intent after the
/// caller has taken the accessing participation's suspicion advisory. The
/// proxy-open path uses this on its retained roster transaction so it never
/// checks out a second database connection or inverts detector lock order.
async fn persist_access_audit_after_suspicion(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    audit: AccessAudit<'_>,
) -> AppResult<()> {
    let identities = [
        audit.owner_participation_id,
        audit.accessing_participation_id,
    ];
    let audit_scope_locked = crate::services::participation_evidence::lock_audit_insert_scope(
        transaction,
        audit.game_id,
        Some(audit.challenge_id),
        &identities,
    )
    .await?;
    let evidence_open = crate::services::participation_evidence::competitive_evidence_is_open(
        transaction,
        audit.game_id,
    )
    .await?;
    let competitive_scope = match (audit_scope_locked, evidence_open) {
        (true, Some(true)) => true,
        (false, Some(false)) => false,
        _ => {
            return Err(AppError::conflict(
                "Game evidence identity changed while recording container access",
            ));
        }
    };

    // Assign the source time only after the shared Game fence. Closed games
    // retain raw telemetry, but clamp it to the exclusive end boundary so a
    // backward wall-clock step cannot admit it to a final aggregate sweep.
    let observed_at: DateTime<Utc> = sqlx::query_scalar(
        r#"SELECT CASE
                     WHEN $2 THEN clock_timestamp()
                     ELSE GREATEST(clock_timestamp(), end_time_utc)
                   END
              FROM "Games"
             WHERE id = $1"#,
    )
    .bind(audit.game_id)
    .bind(competitive_scope)
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)?;

    let access_event_id: i32 = sqlx::query_scalar(INSERT_ACCESS_EVENT_SQL)
        .bind(audit.game_id)
        .bind(audit.challenge_id)
        .bind(audit.owner_participation_id)
        .bind(audit.container_id)
        .bind(audit.accessing_user_id)
        .bind(audit.accessing_user_name)
        .bind(audit.accessing_participation_id)
        .bind(audit.remote_ip)
        .bind(audit.remote_ip_hash)
        .bind(audit.user_agent)
        .bind(audit.is_monitor)
        .bind(observed_at)
        .fetch_one(&mut **transaction)
        .await
        .map_err(database_error)?;

    let cross_team =
        !audit.is_monitor && audit.accessing_participation_id != audit.owner_participation_id;
    if cross_team && competitive_scope {
        let game_is_live: bool = sqlx::query_scalar(
            r#"SELECT start_time_utc <= $2 AND end_time_utc > $2
                 FROM "Games"
                WHERE id = $1"#,
        )
        .bind(audit.game_id)
        .bind(observed_at)
        .fetch_one(&mut **transaction)
        .await
        .map_err(database_error)?;
        if game_is_live {
            let evidence_key = format!("challenge:{}", audit.challenge_id);
            let enqueued = crate::services::suspicion::enqueue_direct_suspicion_evaluation(
                transaction,
                crate::services::suspicion::EvaluationSourceKind::ContainerAccess,
                access_event_id,
                audit.game_id,
                audit.accessing_participation_id,
                Some(audit.challenge_id),
                crate::services::suspicion::SuspicionType::CrossTeamContainerAccess,
                &evidence_key,
                observed_at,
                serde_json::json!({
                    "containerId": audit.container_id,
                    "ownerParticipationId": audit.owner_participation_id,
                }),
            )
            .await?;
            if !enqueued {
                return Err(AppError::internal(
                    "container access source did not establish a new durable evaluation intent",
                ));
            }
        }
    }

    Ok(())
}

/// Stage the raw access row and any attributable cross-team suspicion intent
/// in the caller's final proxy-open transaction. An applicable live cross-team
/// source row can never commit without its durable job.
pub(super) async fn log_container_access_on(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    st: &SharedState,
    access: &InstanceAccess,
    game: &GameAccess,
    remote_ip: String,
    user_agent: Option<String>,
) -> AppResult<()> {
    let remote_ip_hash =
        crate::services::anti_cheat::hash_ip_identity(st.config.as_ref(), &remote_ip)
            .map(|identity| identity.exact);
    let audit = AccessAudit {
        game_id: game.game_id,
        challenge_id: game.challenge_id,
        owner_participation_id: game.owner_participation_id,
        accessing_participation_id: game.accessing_participation_id,
        container_id: access.container_id,
        accessing_user_id: access.accessing_user_id,
        accessing_user_name: &access.accessing_user_name,
        remote_ip: &remote_ip,
        remote_ip_hash: remote_ip_hash.as_deref(),
        user_agent: user_agent.as_deref(),
        is_monitor: game.is_monitor,
    };
    persist_access_audit_after_suspicion(transaction, audit).await
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::time::Duration as StdDuration;

    use chrono::{Duration, Utc};
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use uuid::Uuid;

    use super::{persist_access_audit, AccessAudit, INSERT_ACCESS_EVENT_SQL};

    #[test]
    fn access_insert_returns_source_identity_and_persists_immutable_request_context() {
        assert!(INSERT_ACCESS_EVENT_SQL.contains("remote_ip_hash"));
        assert!(INSERT_ACCESS_EVENT_SQL.contains("is_monitor"));
        assert!(INSERT_ACCESS_EVENT_SQL.contains("RETURNING id"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn raw_and_cross_team_outbox_faults_never_leave_one_sided_evidence() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect test database");
        let schema = format!("container_access_atomic_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .expect("create test schema");
        let options = PgConnectOptions::from_str(&database_url)
            .expect("parse test database URL")
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .expect("connect isolated pool");
        let rejected_container =
            Uuid::parse_str("22222222-2222-2222-2222-222222222222").expect("fixed UUID");
        sqlx::raw_sql(&format!(
            r#"
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY,
              start_time_utc TIMESTAMPTZ NOT NULL,
              end_time_utc TIMESTAMPTZ NOT NULL,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "Teams" (
              id INTEGER PRIMARY KEY,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
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
              is_enabled BOOLEAN NOT NULL DEFAULT TRUE,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "SuspicionReconciliationState" (
              game_id INTEGER PRIMARY KEY,
              evidence_closed_at_utc TIMESTAMPTZ
            );
            CREATE TABLE "ContainerAccessEvents" (
              id INTEGER GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
              game_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL,
              container_owner_participation_id INTEGER NOT NULL,
              container_id UUID NOT NULL,
              accessing_user_id UUID,
              accessing_user_name TEXT,
              accessing_participation_id INTEGER,
              remote_ip TEXT NOT NULL CHECK (remote_ip <> 'raw-failure'),
              remote_ip_hash BYTEA,
              user_agent TEXT,
              is_monitor BOOLEAN NOT NULL,
              connected_at_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "SuspicionEvaluationOutbox" (
              id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
              job_kind SMALLINT NOT NULL,
              source_kind SMALLINT NOT NULL,
              source_id INTEGER NOT NULL,
              game_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL,
              challenge_id INTEGER,
              rule_kind SMALLINT,
              evidence_key TEXT NOT NULL,
              observed_at_utc TIMESTAMPTZ NOT NULL,
              evidence_payload JSONB NOT NULL CHECK (
                evidence_payload->>'containerId' <> '{rejected_container}'
              ),
              evidence_version SMALLINT NOT NULL
            );
            "#,
        ))
        .execute(&pool)
        .await
        .expect("create fault-injection fixture");
        let now = Utc::now();
        sqlx::query(
            r#"INSERT INTO "Games" (id, start_time_utc, end_time_utc)
               VALUES (1, $1, $2)"#,
        )
        .bind(now - Duration::hours(1))
        .bind(now + Duration::hours(1))
        .execute(&pool)
        .await
        .expect("insert live game");
        sqlx::raw_sql(
            r#"
            INSERT INTO "Teams" (id) VALUES (2), (3);
            INSERT INTO "Participations" (id, game_id, team_id, status)
              VALUES (10, 1, 2, 1), (11, 1, 3, 1);
            INSERT INTO "GameChallenges" (id, game_id) VALUES (20, 1);
            "#,
        )
        .execute(&pool)
        .await
        .expect("insert evidence identities");

        let user_id = Uuid::new_v4();
        let hash = [7_u8; 32];
        let make_audit = |container_id, remote_ip, is_monitor| AccessAudit {
            game_id: 1,
            challenge_id: 20,
            owner_participation_id: 10,
            accessing_participation_id: 11,
            container_id,
            accessing_user_id: user_id,
            accessing_user_name: "accessor",
            remote_ip,
            remote_ip_hash: Some(&hash),
            user_agent: None,
            is_monitor,
        };

        // A finalizer that owns Games FOR UPDATE must drain this writer before
        // its source time is assigned. Durable closure remains authoritative
        // even if the wall clock later appears to move back into the window.
        let mut barrier = pool.begin().await.expect("begin finalization barrier");
        sqlx::query(r#"SELECT id FROM "Games" WHERE id = 1 FOR UPDATE"#)
            .fetch_one(&mut *barrier)
            .await
            .expect("lock game finalization barrier");
        let barrier_writer =
            persist_access_audit(&pool, make_audit(Uuid::new_v4(), "192.0.2.50", false));
        tokio::pin!(barrier_writer);
        assert!(
            tokio::time::timeout(StdDuration::from_millis(100), barrier_writer.as_mut())
                .await
                .is_err(),
            "writer bypassed the Game-row finalization barrier"
        );
        let barrier_closed_at: chrono::DateTime<Utc> =
            sqlx::query_scalar("SELECT clock_timestamp()")
                .fetch_one(&mut *barrier)
                .await
                .expect("capture evidence closure time");
        sqlx::query(
            r#"INSERT INTO "SuspicionReconciliationState"
                 (game_id, evidence_closed_at_utc) VALUES (1, $1)"#,
        )
        .bind(barrier_closed_at)
        .execute(&mut *barrier)
        .await
        .expect("close competitive evidence under barrier");
        sqlx::query(
            r#"UPDATE "Games"
                  SET end_time_utc = $2 + INTERVAL '1 hour'
                WHERE id = $1"#,
        )
        .bind(1_i32)
        .bind(barrier_closed_at)
        .execute(&mut *barrier)
        .await
        .expect("simulate a wall-clock reversal after closure");
        barrier
            .commit()
            .await
            .expect("release finalization barrier");
        tokio::time::timeout(StdDuration::from_secs(5), barrier_writer.as_mut())
            .await
            .expect("writer did not resume after barrier")
            .expect("persist post-barrier raw access");
        let (connected_at, game_end, barrier_jobs): (
            chrono::DateTime<Utc>,
            chrono::DateTime<Utc>,
            i64,
        ) = sqlx::query_as(
            r#"SELECT connected_at_utc,
                      (SELECT end_time_utc FROM "Games" WHERE id = 1),
                      (SELECT COUNT(*) FROM "SuspicionEvaluationOutbox")
                 FROM "ContainerAccessEvents""#,
        )
        .fetch_one(&pool)
        .await
        .expect("load post-barrier access");
        assert!(connected_at >= barrier_closed_at);
        assert_eq!(connected_at, game_end);
        assert_eq!(barrier_jobs, 0);
        sqlx::query(r#"DELETE FROM "ContainerAccessEvents""#)
            .execute(&pool)
            .await
            .expect("clear barrier access");
        sqlx::query(r#"DELETE FROM "SuspicionReconciliationState" WHERE game_id = 1"#)
            .execute(&pool)
            .await
            .expect("reopen competitive evidence fixture");
        sqlx::query(r#"UPDATE "Games" SET end_time_utc = $2 WHERE id = $1"#)
            .bind(1_i32)
            .bind(now + Duration::hours(1))
            .execute(&pool)
            .await
            .expect("reopen fixture game");

        let raw_failure =
            persist_access_audit(&pool, make_audit(Uuid::new_v4(), "raw-failure", false)).await;
        assert!(raw_failure.is_err());
        let outbox_failure =
            persist_access_audit(&pool, make_audit(rejected_container, "192.0.2.1", false)).await;
        assert!(outbox_failure.is_err());
        let empty: (i64, i64) = sqlx::query_as(
            r#"SELECT (SELECT COUNT(*) FROM "ContainerAccessEvents"),
                      (SELECT COUNT(*) FROM "SuspicionEvaluationOutbox")"#,
        )
        .fetch_one(&pool)
        .await
        .expect("count rolled-back evidence");
        assert_eq!(empty, (0, 0));

        // Force enqueue to return false after the raw insert. The source row
        // must roll back instead of committing without new durable intent.
        sqlx::query(
            r#"CREATE UNIQUE INDEX ux_fault_access_source_kind
                 ON "SuspicionEvaluationOutbox" (source_kind)"#,
        )
        .execute(&pool)
        .await
        .expect("create injected outbox collision");
        sqlx::query(
            r#"INSERT INTO "SuspicionEvaluationOutbox"
                 (job_kind, source_kind, source_id, game_id, participation_id,
                  challenge_id, rule_kind, evidence_key, observed_at_utc,
                  evidence_payload, evidence_version)
               VALUES (1, 2, -1, 1, 11, 20, $1, 'fault-placeholder',
                       $2, '{}'::jsonb, 1)"#,
        )
        .bind(crate::services::suspicion::SuspicionType::CrossTeamContainerAccess.kind())
        .bind(now)
        .execute(&pool)
        .await
        .expect("insert collision placeholder");
        let collision =
            persist_access_audit(&pool, make_audit(Uuid::new_v4(), "192.0.2.1", false)).await;
        assert!(collision.is_err());
        let collision_counts: (i64, i64) = sqlx::query_as(
            r#"SELECT (SELECT COUNT(*) FROM "ContainerAccessEvents"),
                      (SELECT COUNT(*) FROM "SuspicionEvaluationOutbox")"#,
        )
        .fetch_one(&pool)
        .await
        .expect("count collision rollback");
        assert_eq!(collision_counts, (0, 1));
        sqlx::query(r#"DELETE FROM "SuspicionEvaluationOutbox""#)
            .execute(&pool)
            .await
            .expect("remove collision placeholder");

        persist_access_audit(&pool, make_audit(Uuid::new_v4(), "192.0.2.1", false))
            .await
            .expect("persist paired access and cross-team job");
        let paired: (i64, i64) = sqlx::query_as(
            r#"SELECT (SELECT COUNT(*) FROM "ContainerAccessEvents"),
                      (SELECT COUNT(*) FROM "SuspicionEvaluationOutbox")"#,
        )
        .fetch_one(&pool)
        .await
        .expect("count paired evidence");
        assert_eq!(paired, (1, 1));
        let (event_id, stored_hash, is_monitor, source_kind, source_id): (
            i32,
            Vec<u8>,
            bool,
            i16,
            i32,
        ) = sqlx::query_as(
            r#"SELECT event.id, event.remote_ip_hash, event.is_monitor,
                          job.source_kind, job.source_id
                     FROM "ContainerAccessEvents" event
                     JOIN "SuspicionEvaluationOutbox" job
                       ON job.source_id = event.id"#,
        )
        .fetch_one(&pool)
        .await
        .expect("load paired access identity");
        assert_eq!(stored_hash, hash);
        assert!(!is_monitor);
        assert_eq!(source_kind, 2);
        assert_eq!(source_id, event_id);

        let monitor_container = Uuid::new_v4();
        persist_access_audit(&pool, make_audit(monitor_container, "192.0.2.2", true))
            .await
            .expect("persist monitor access as raw telemetry");
        let (monitor_snapshot, monitor_jobs): (bool, i64) = sqlx::query_as(
            r#"SELECT is_monitor,
                      (SELECT COUNT(*) FROM "SuspicionEvaluationOutbox")
                 FROM "ContainerAccessEvents"
                WHERE container_id = $1"#,
        )
        .bind(monitor_container)
        .fetch_one(&pool)
        .await
        .expect("load monitor provenance");
        assert!(monitor_snapshot);
        assert_eq!(monitor_jobs, 1);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .expect("drop test schema");
    }
}

#[cfg(test)]
#[path = "access_log_ordering_tests.rs"]
mod ordering_tests;
