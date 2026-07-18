//! Durable desired/observed state for outbound worker-plane agents.
//!
//! Resource reservations are derived from assigned workload rows. There are no
//! mutable aggregate counters to repair after a crash; schedulers serialize on
//! the selected `WorkerNodes` row and calculate the exact active reservation.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS "WorkerNodes" (
        id UUID PRIMARY KEY,
        name TEXT NOT NULL,
        administrative_state TEXT NOT NULL DEFAULT 'Enabled',
        platform_os TEXT NULL,
        platform_architecture TEXT NULL,
        runtime_kind TEXT NULL,
        runtime_version TEXT NULL,
        labels JSONB NOT NULL DEFAULT '{}'::jsonb,
        capabilities JSONB NOT NULL DEFAULT '{}'::jsonb,
        capacity_cpu_millis BIGINT NOT NULL DEFAULT 0,
        capacity_memory_bytes BIGINT NOT NULL DEFAULT 0,
        capacity_slots INTEGER NOT NULL DEFAULT 0,
        enrollment_token_hash BYTEA NULL,
        enrollment_token_expires_at TIMESTAMPTZ NULL,
        enrollment_token_used_at TIMESTAMPTZ NULL,
        certificate_fingerprint_sha256 BYTEA NULL,
        certificate_serial TEXT NULL,
        certificate_expires_at TIMESTAMPTZ NULL,
        session_id UUID NULL,
        session_epoch BIGINT NOT NULL DEFAULT 0,
        boot_id UUID NULL,
        connected_at TIMESTAMPTZ NULL,
        heartbeat_at TIMESTAMPTZ NULL,
        lease_expires_at TIMESTAMPTZ NULL,
        created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
        updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
        CONSTRAINT ck_workernodes_name CHECK (BTRIM(name) <> ''),
        CONSTRAINT ck_workernodes_administrative_state CHECK (
            administrative_state IN ('Enabled', 'Draining', 'Disabled')
        ),
        CONSTRAINT ck_workernodes_platform_os CHECK (
            platform_os IS NULL OR platform_os IN ('linux', 'windows')
        ),
        CONSTRAINT ck_workernodes_inventory_text CHECK (
            (platform_architecture IS NULL OR BTRIM(platform_architecture) <> '')
            AND (runtime_kind IS NULL OR BTRIM(runtime_kind) <> '')
            AND (runtime_version IS NULL OR BTRIM(runtime_version) <> '')
        ),
        CONSTRAINT ck_workernodes_labels_object CHECK (jsonb_typeof(labels) = 'object'),
        CONSTRAINT ck_workernodes_capabilities_object CHECK (
            jsonb_typeof(capabilities) = 'object'
        ),
        CONSTRAINT ck_workernodes_capacity CHECK (
            capacity_cpu_millis >= 0
            AND capacity_memory_bytes >= 0
            AND capacity_slots >= 0
        ),
        CONSTRAINT ck_workernodes_enrollment CHECK (
            (
                enrollment_token_hash IS NULL
                AND enrollment_token_expires_at IS NULL
            )
            OR (
                OCTET_LENGTH(enrollment_token_hash) = 32
                AND enrollment_token_expires_at IS NOT NULL
                AND enrollment_token_used_at IS NULL
            )
        ),
        CONSTRAINT ck_workernodes_certificate CHECK (
            (
                certificate_fingerprint_sha256 IS NULL
                AND certificate_serial IS NULL
                AND certificate_expires_at IS NULL
            )
            OR (
                OCTET_LENGTH(certificate_fingerprint_sha256) = 32
                AND NULLIF(BTRIM(certificate_serial), '') IS NOT NULL
                AND certificate_expires_at IS NOT NULL
            )
        ),
        CONSTRAINT ck_workernodes_session_epoch CHECK (session_epoch >= 0),
        CONSTRAINT ck_workernodes_session CHECK (
            (
                session_id IS NULL
                AND boot_id IS NULL
                AND connected_at IS NULL
                AND lease_expires_at IS NULL
            )
            OR (
                session_id IS NOT NULL
                AND boot_id IS NOT NULL
                AND connected_at IS NOT NULL
                AND heartbeat_at IS NOT NULL
                AND lease_expires_at IS NOT NULL
            )
        )
    );

    CREATE UNIQUE INDEX IF NOT EXISTS ux_workernodes_name
        ON "WorkerNodes" (LOWER(name));
    CREATE UNIQUE INDEX IF NOT EXISTS ux_workernodes_certificate_fingerprint
        ON "WorkerNodes" (certificate_fingerprint_sha256)
        WHERE certificate_fingerprint_sha256 IS NOT NULL;
    CREATE UNIQUE INDEX IF NOT EXISTS ux_workernodes_enrollment_token
        ON "WorkerNodes" (enrollment_token_hash)
        INCLUDE (enrollment_token_expires_at)
        WHERE enrollment_token_hash IS NOT NULL;
    CREATE INDEX IF NOT EXISTS ix_workernodes_schedulable
        ON "WorkerNodes" (
            administrative_state,
            platform_os,
            platform_architecture,
            runtime_kind,
            lease_expires_at DESC
        );

    CREATE TABLE IF NOT EXISTS "WorkerWorkloads" (
        id UUID PRIMARY KEY,
        owner_kind TEXT NOT NULL,
        owner_key TEXT NOT NULL,
        worker_id UUID NOT NULL
            REFERENCES "WorkerNodes" (id) ON DELETE RESTRICT,
        assignment_id UUID NOT NULL,
        generation BIGINT NOT NULL,
        spec_hash_sha256 BYTEA NOT NULL,
        spec JSONB NOT NULL,
        required_os TEXT NOT NULL,
        required_architecture TEXT NOT NULL,
        required_runtime TEXT NOT NULL,
        required_labels JSONB NOT NULL DEFAULT '{}'::jsonb,
        reserved_cpu_millis BIGINT NOT NULL,
        reserved_memory_bytes BIGINT NOT NULL,
        reserved_slots INTEGER NOT NULL,
        desired_state TEXT NOT NULL DEFAULT 'Present',
        observed_state TEXT NOT NULL DEFAULT 'Unknown',
        observed_session_epoch BIGINT NULL,
        observed_message TEXT NULL,
        observed_at TIMESTAMPTZ NULL,
        ready_at TIMESTAMPTZ NULL,
        created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
        updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
        CONSTRAINT ck_workerworkloads_generation CHECK (generation >= 1),
        CONSTRAINT ck_workerworkloads_owner CHECK (
            BTRIM(owner_kind) <> ''
            AND CHAR_LENGTH(owner_kind) <= 64
            AND BTRIM(owner_key) <> ''
            AND CHAR_LENGTH(owner_key) <= 512
        ),
        CONSTRAINT ck_workerworkloads_spec_hash CHECK (
            OCTET_LENGTH(spec_hash_sha256) = 32
        ),
        CONSTRAINT ck_workerworkloads_spec_object CHECK (jsonb_typeof(spec) = 'object'),
        CONSTRAINT ck_workerworkloads_platform CHECK (
            required_os IN ('linux', 'windows')
            AND BTRIM(required_architecture) <> ''
            AND BTRIM(required_runtime) <> ''
        ),
        CONSTRAINT ck_workerworkloads_required_labels CHECK (
            jsonb_typeof(required_labels) = 'object'
        ),
        CONSTRAINT ck_workerworkloads_reservation CHECK (
            reserved_cpu_millis >= 0
            AND reserved_memory_bytes >= 0
            AND reserved_slots > 0
        ),
        CONSTRAINT ck_workerworkloads_desired_state CHECK (
            desired_state IN ('Present', 'Absent')
        ),
        CONSTRAINT ck_workerworkloads_observed_state CHECK (
            observed_state IN (
                'Unknown', 'Reconciling', 'Ready', 'Degraded',
                'Failed', 'Absent', 'Lost'
            )
        ),
        CONSTRAINT ck_workerworkloads_observed_epoch CHECK (
            observed_session_epoch IS NULL OR observed_session_epoch > 0
        ),
        CONSTRAINT ck_workerworkloads_observed_message CHECK (
            observed_message IS NULL OR CHAR_LENGTH(observed_message) <= 4096
        )
    );

    CREATE UNIQUE INDEX IF NOT EXISTS ux_workerworkloads_assignment
        ON "WorkerWorkloads" (assignment_id);
    CREATE UNIQUE INDEX IF NOT EXISTS ux_workerworkloads_owner
        ON "WorkerWorkloads" (owner_kind, owner_key);
    CREATE INDEX IF NOT EXISTS ix_workerworkloads_reconcile
        ON "WorkerWorkloads" (worker_id, desired_state, observed_state, updated_at, id);
    CREATE INDEX IF NOT EXISTS ix_workerworkloads_active_reservations
        ON "WorkerWorkloads" (worker_id)
        INCLUDE (reserved_cpu_millis, reserved_memory_bytes, reserved_slots)
        WHERE desired_state = 'Present' OR observed_state <> 'Absent';
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                DROP TABLE IF EXISTS "WorkerWorkloads";
                DROP TABLE IF EXISTS "WorkerNodes";
                "#,
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;
    use crate::migrations::m0068_worker_workload_dimensions::UP_SQL as DIMENSIONS_SQL;

    #[test]
    fn creates_fenced_worker_state_and_exact_reservations_idempotently() {
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"WorkerNodes\""));
        assert!(UP_SQL.contains("certificate_fingerprint_sha256 BYTEA NULL"));
        assert!(UP_SQL.contains("OCTET_LENGTH(enrollment_token_hash) = 32"));
        assert!(UP_SQL.contains("session_epoch BIGINT NOT NULL DEFAULT 0"));
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"WorkerWorkloads\""));
        assert!(UP_SQL.contains("assignment_id UUID NOT NULL"));
        assert!(UP_SQL.contains("owner_kind TEXT NOT NULL"));
        assert!(UP_SQL.contains("generation BIGINT NOT NULL"));
        assert!(
            UP_SQL.contains("INCLUDE (reserved_cpu_millis, reserved_memory_bytes, reserved_slots)")
        );
        assert!(!UP_SQL.contains("reserved_cpu_millis_used"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn enrollment_sessions_and_reservations_are_fenced_end_to_end() {
        use std::time::Duration;

        use chrono::{Duration as ChronoDuration, Utc};
        use serde_json::{json, Value};
        use sqlx::postgres::PgPoolOptions;
        use uuid::Uuid;

        use crate::services::worker_store::{
            CreateWorker, DefinitionUpdateOutcome, DesiredUpdateOutcome, PlaceWorkload,
            PlacementOutcome, PlatformOs, ResourceReservation, StatusUpdateOutcome, UpdateWorkload,
            WorkerAdministrativeState, WorkerCertificate, WorkerInventory, WorkerStore,
            WorkloadDefinition, WorkloadObservedState, WorkloadStatus,
        };

        fn workload_spec(replicas: u16) -> Value {
            json!({
                "gameKind": "jeopardy",
                "platform": {"operatingSystem": "windows", "architecture": "amd64"},
                "services": [{
                    "name": "web",
                    "image": {
                        "type": "registryDigest",
                        "repository": "registry.example/challenge",
                        "digest": format!("sha256:{}", "a".repeat(64))
                    },
                    "resources": {"cpuMillis": 100, "memoryBytes": 1_000_000},
                    "replicas": replicas,
                    "stateless": true,
                    "environment": {},
                    "ports": [{"name": "service", "containerPort": 31337, "protocol": "tcp"}]
                }],
                "primaryEndpoint": {"service": "web", "port": "service"}
            })
        }

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect test database");
        let schema = format!("worker_plane_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();

        let search_path_schema = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .after_connect(move |connection, _metadata| {
                let statement = format!(r#"SET search_path TO "{search_path_schema}""#);
                Box::pin(async move {
                    sqlx::query(&statement).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await
            .unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(DIMENSIONS_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(DIMENSIONS_SQL).execute(&pool).await.unwrap();

        let store = WorkerStore::new(pool.clone());
        let worker_id = Uuid::new_v4();
        let token_hash = [1; 32];
        store
            .create_worker(CreateWorker {
                id: worker_id,
                name: "windows-builder".to_owned(),
                enrollment_token_hash: token_hash,
                enrollment_token_expires_at: Utc::now() + ChronoDuration::minutes(5),
            })
            .await
            .unwrap();
        assert_eq!(
            store.resolve_enrollment_token(token_hash).await.unwrap(),
            Some(worker_id)
        );
        let certificate_fingerprint = [2; 32];
        store
            .enroll_certificate(
                token_hash,
                WorkerCertificate {
                    fingerprint_sha256: certificate_fingerprint,
                    serial: "01".to_owned(),
                    expires_at: Utc::now() + ChronoDuration::days(1),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert!(store
            .resolve_enrollment_token(token_hash)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            store
                .authenticate_certificate(certificate_fingerprint)
                .await
                .unwrap()
                .unwrap()
                .id,
            worker_id
        );

        let inventory = WorkerInventory {
            platform_os: PlatformOs::Windows,
            architecture: "amd64".to_owned(),
            runtime_kind: "docker".to_owned(),
            runtime_version: "27".to_owned(),
            labels: json!({"isolation": "hyperv"}),
            capabilities: json!({
                "ensureWorkload": true,
                "writeFlag": true,
                "tcpProxy": true,
                "inventory": true,
                "maxDataLanes": 4,
                "maxWorkloadReplicas": 4
            }),
            capacity: ResourceReservation {
                cpu_millis: 4_000,
                memory_bytes: 8_000_000,
                slots: 4,
            },
        };
        assert!(store
            .open_session(
                worker_id,
                [9; 32],
                Uuid::new_v4(),
                Uuid::new_v4(),
                &inventory,
                Duration::from_secs(30),
            )
            .await
            .unwrap()
            .is_none());
        let session = store
            .open_session(
                worker_id,
                certificate_fingerprint,
                Uuid::new_v4(),
                Uuid::new_v4(),
                &inventory,
                Duration::from_secs(30),
            )
            .await
            .unwrap()
            .unwrap();

        let workload_id = Uuid::new_v4();
        let assignment_id = Uuid::new_v4();
        let definition = WorkloadDefinition {
            spec: workload_spec(2),
            spec_hash_sha256: [3; 32],
            required_os: PlatformOs::Windows,
            required_architecture: "amd64".to_owned(),
            required_runtime: "docker".to_owned(),
            reservation: ResourceReservation {
                cpu_millis: 1_000,
                memory_bytes: 2_000_000,
                slots: 1,
            },
        };
        let request = PlaceWorkload {
            id: workload_id,
            owner_kind: "Container".to_owned(),
            owner_key: "container-42".to_owned(),
            assignment_id,
            definition: definition.clone(),
            exact_worker_id: Some(worker_id),
            required_labels: json!({"isolation": "hyperv"}),
        };
        assert!(matches!(
            store.place_workload(&request).await.unwrap(),
            PlacementOutcome::Placed(_)
        ));
        assert!(matches!(
            store.place_workload(&request).await.unwrap(),
            PlacementOutcome::AlreadyExists(_)
        ));

        let ready = WorkloadStatus {
            session: session.fence,
            workload_id,
            assignment_id,
            generation: 1,
            spec_hash_sha256: [3; 32],
            state: WorkloadObservedState::Ready,
            message: None,
        };
        assert_eq!(
            store.record_workload_status(&ready).await.unwrap(),
            StatusUpdateOutcome::Applied
        );
        assert!(store
            .ready_workload_session(workload_id)
            .await
            .unwrap()
            .is_some());

        let replacement_session = store
            .open_session(
                worker_id,
                certificate_fingerprint,
                Uuid::new_v4(),
                Uuid::new_v4(),
                &inventory,
                Duration::from_secs(30),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(store
            .ready_workload_session(workload_id)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            store.record_workload_status(&ready).await.unwrap(),
            StatusUpdateOutcome::Stale
        );

        let unsupported_replicas = UpdateWorkload {
            id: workload_id,
            assignment_id,
            expected_generation: 1,
            definition: WorkloadDefinition {
                spec: workload_spec(5),
                ..definition.clone()
            },
        };
        assert_eq!(
            store
                .update_workload_definition(&unsupported_replicas)
                .await
                .unwrap(),
            DefinitionUpdateOutcome::WorkerNoLongerCompatible
        );
        let oversized = UpdateWorkload {
            definition: WorkloadDefinition {
                spec: workload_spec(3),
                reservation: ResourceReservation {
                    cpu_millis: 5_000,
                    ..definition.reservation
                },
                ..definition.clone()
            },
            ..unsupported_replicas
        };
        assert_eq!(
            store.update_workload_definition(&oversized).await.unwrap(),
            DefinitionUpdateOutcome::InsufficientCapacity
        );
        let scaled = UpdateWorkload {
            definition: WorkloadDefinition {
                spec: workload_spec(3),
                spec_hash_sha256: [4; 32],
                reservation: definition.reservation,
                ..definition
            },
            ..oversized
        };
        assert_eq!(
            store.update_workload_definition(&scaled).await.unwrap(),
            DefinitionUpdateOutcome::Updated { generation: 2 }
        );
        assert_eq!(
            store
                .mark_desired_absent(workload_id, assignment_id, 2)
                .await
                .unwrap(),
            DesiredUpdateOutcome::Updated { generation: 3 }
        );
        let absent = WorkloadStatus {
            session: replacement_session.fence,
            workload_id,
            assignment_id,
            generation: 3,
            spec_hash_sha256: [4; 32],
            state: WorkloadObservedState::Absent,
            message: None,
        };
        assert_eq!(
            store.record_workload_status(&absent).await.unwrap(),
            StatusUpdateOutcome::Applied
        );

        let second = PlaceWorkload {
            id: Uuid::new_v4(),
            owner_kind: "Container".to_owned(),
            owner_key: "container-43".to_owned(),
            assignment_id: Uuid::new_v4(),
            definition: WorkloadDefinition {
                spec: workload_spec(4),
                spec_hash_sha256: [5; 32],
                required_os: PlatformOs::Windows,
                required_architecture: "amd64".to_owned(),
                required_runtime: "docker".to_owned(),
                reservation: ResourceReservation {
                    cpu_millis: 4_000,
                    memory_bytes: 8_000_000,
                    slots: 1,
                },
            },
            exact_worker_id: Some(worker_id),
            required_labels: json!({"isolation": "hyperv"}),
        };
        assert!(matches!(
            store.place_workload(&second).await.unwrap(),
            PlacementOutcome::Placed(_)
        ));

        // Compromised-certificate rotation is performed while the worker is
        // disabled: the old credential stays fenced, enrollment can replace
        // it, and only the replacement becomes usable after explicit enable.
        assert!(store
            .set_administrative_state(worker_id, WorkerAdministrativeState::Disabled)
            .await
            .unwrap());
        let replacement_token = [6; 32];
        store
            .issue_enrollment_token(
                worker_id,
                replacement_token,
                Utc::now() + ChronoDuration::minutes(5),
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .resolve_enrollment_token(replacement_token)
                .await
                .unwrap(),
            Some(worker_id)
        );
        let replacement_fingerprint = [7; 32];
        store
            .enroll_certificate(
                replacement_token,
                WorkerCertificate {
                    fingerprint_sha256: replacement_fingerprint,
                    serial: "02".to_owned(),
                    expires_at: Utc::now() + ChronoDuration::days(1),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert!(store
            .authenticate_certificate(certificate_fingerprint)
            .await
            .unwrap()
            .is_none());
        assert!(store
            .authenticate_certificate(replacement_fingerprint)
            .await
            .unwrap()
            .is_none());
        assert!(store
            .set_administrative_state(worker_id, WorkerAdministrativeState::Enabled)
            .await
            .unwrap());
        assert!(store
            .authenticate_certificate(certificate_fingerprint)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            store
                .authenticate_certificate(replacement_fingerprint)
                .await
                .unwrap()
                .unwrap()
                .id,
            worker_id
        );

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
