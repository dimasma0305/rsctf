//! Transaction-scoped authorization for deleting otherwise immutable event-VPN history.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE OR REPLACE FUNCTION rsctf_event_history_purge_authorized(p_game_id INTEGER)
RETURNS BOOLEAN LANGUAGE sql STABLE AS $$
    SELECT COALESCE(current_setting('rsctf.event_history_purge_operation', TRUE), '') <> ''
       AND EXISTS (
            SELECT 1
              FROM "GamePurgeOperations"
             WHERE game_id = p_game_id
               AND status = 0
               AND operation_id::text =
                   current_setting('rsctf.event_history_purge_operation', TRUE)
       );
$$;

CREATE OR REPLACE FUNCTION guard_event_vpn_peer_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        IF rsctf_event_history_purge_authorized(OLD.game_id) THEN
            RETURN OLD;
        END IF;
        RAISE EXCEPTION 'EventVpnUserPeers cannot be deleted' USING ERRCODE = '55000';
    END IF;
    IF OLD.id IS DISTINCT FROM NEW.id
       OR OLD.game_id IS DISTINCT FROM NEW.game_id
       OR OLD.user_id IS DISTINCT FROM NEW.user_id
       OR OLD.participation_id IS DISTINCT FROM NEW.participation_id
       OR OLD.public_key IS DISTINCT FROM NEW.public_key
       OR OLD.private_key_ciphertext IS DISTINCT FROM NEW.private_key_ciphertext
       OR OLD.private_key_nonce IS DISTINCT FROM NEW.private_key_nonce
       OR OLD.address IS DISTINCT FROM NEW.address
       OR OLD.generation IS DISTINCT FROM NEW.generation
       OR OLD.issued_at_utc IS DISTINCT FROM NEW.issued_at_utc
       OR (OLD.revoked_at_utc IS NOT NULL AND NEW IS DISTINCT FROM OLD)
       OR (OLD.revoked_at_utc IS NULL AND NEW.revoked_at_utc IS NOT NULL
           AND NEW.revoked_at_utc < OLD.issued_at_utc)
       OR (OLD.last_config_download_at_utc IS NOT NULL
           AND NEW.last_config_download_at_utc < OLD.last_config_download_at_utc)
       OR (NEW.last_config_download_at_utc IS NOT NULL
           AND NEW.last_config_download_at_utc < OLD.issued_at_utc)
    THEN
        RAISE EXCEPTION 'EventVpnUserPeers provenance is immutable' USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION guard_event_vpn_override_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        IF rsctf_event_history_purge_authorized(OLD.game_id) THEN
            RETURN OLD;
        END IF;
        IF (OLD.revoked_at_utc IS NOT NULL OR OLD.expires_at_utc <= clock_timestamp())
           AND OLD.created_at_utc < clock_timestamp() - INTERVAL '30 days'
        THEN
            RETURN OLD;
        END IF;
        RAISE EXCEPTION 'EventVpnGateOverrides cannot be deleted inside retention'
            USING ERRCODE = '55000';
    END IF;
    IF OLD.id IS DISTINCT FROM NEW.id
       OR OLD.game_id IS DISTINCT FROM NEW.game_id
       OR OLD.created_by_user_id IS DISTINCT FROM NEW.created_by_user_id
       OR OLD.reason IS DISTINCT FROM NEW.reason
       OR OLD.created_at_utc IS DISTINCT FROM NEW.created_at_utc
       OR OLD.expires_at_utc IS DISTINCT FROM NEW.expires_at_utc
       OR OLD.policy_revision IS DISTINCT FROM NEW.policy_revision
       OR OLD.revoked_at_utc IS NOT NULL
       OR NEW.revoked_at_utc IS NULL
    THEN
        RAISE EXCEPTION 'EventVpnGateOverrides provenance is immutable' USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION reject_event_vpn_policy_audit_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' AND rsctf_event_history_purge_authorized(OLD.game_id) THEN
        RETURN OLD;
    END IF;
    RAISE EXCEPTION 'EventVpnPolicyAudit is append-only' USING ERRCODE = '55000';
END;
$$;

CREATE OR REPLACE FUNCTION reject_identity_observation_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE'
       AND OLD.game_id IS NOT NULL
       AND rsctf_event_history_purge_authorized(OLD.game_id)
    THEN
        RETURN OLD;
    END IF;
    RAISE EXCEPTION 'IdentityObservations is append-only' USING ERRCODE = '55000';
END;
$$;

CREATE OR REPLACE FUNCTION rsctf_reject_suspicion_event_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' AND rsctf_event_history_purge_authorized(OLD.game_id) THEN
        RETURN OLD;
    END IF;
    RAISE EXCEPTION 'SuspicionEvents is an immutable evidence ledger'
        USING ERRCODE = '55000';
END;
$$;
"#;

const DOWN_SQL: &str = r#"
CREATE OR REPLACE FUNCTION guard_event_vpn_peer_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'EventVpnUserPeers cannot be deleted' USING ERRCODE = '55000';
    END IF;
    IF OLD.id IS DISTINCT FROM NEW.id
       OR OLD.game_id IS DISTINCT FROM NEW.game_id
       OR OLD.user_id IS DISTINCT FROM NEW.user_id
       OR OLD.participation_id IS DISTINCT FROM NEW.participation_id
       OR OLD.public_key IS DISTINCT FROM NEW.public_key
       OR OLD.private_key_ciphertext IS DISTINCT FROM NEW.private_key_ciphertext
       OR OLD.private_key_nonce IS DISTINCT FROM NEW.private_key_nonce
       OR OLD.address IS DISTINCT FROM NEW.address
       OR OLD.generation IS DISTINCT FROM NEW.generation
       OR OLD.issued_at_utc IS DISTINCT FROM NEW.issued_at_utc
       OR (OLD.revoked_at_utc IS NOT NULL AND NEW IS DISTINCT FROM OLD)
       OR (OLD.revoked_at_utc IS NULL AND NEW.revoked_at_utc IS NOT NULL
           AND NEW.revoked_at_utc < OLD.issued_at_utc)
       OR (OLD.last_config_download_at_utc IS NOT NULL
           AND NEW.last_config_download_at_utc < OLD.last_config_download_at_utc)
       OR (NEW.last_config_download_at_utc IS NOT NULL
           AND NEW.last_config_download_at_utc < OLD.issued_at_utc)
    THEN
        RAISE EXCEPTION 'EventVpnUserPeers provenance is immutable' USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION guard_event_vpn_override_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        IF (OLD.revoked_at_utc IS NOT NULL OR OLD.expires_at_utc <= clock_timestamp())
           AND OLD.created_at_utc < clock_timestamp() - INTERVAL '30 days'
        THEN
            RETURN OLD;
        END IF;
        RAISE EXCEPTION 'EventVpnGateOverrides cannot be deleted inside retention'
            USING ERRCODE = '55000';
    END IF;
    IF OLD.id IS DISTINCT FROM NEW.id
       OR OLD.game_id IS DISTINCT FROM NEW.game_id
       OR OLD.created_by_user_id IS DISTINCT FROM NEW.created_by_user_id
       OR OLD.reason IS DISTINCT FROM NEW.reason
       OR OLD.created_at_utc IS DISTINCT FROM NEW.created_at_utc
       OR OLD.expires_at_utc IS DISTINCT FROM NEW.expires_at_utc
       OR OLD.policy_revision IS DISTINCT FROM NEW.policy_revision
       OR OLD.revoked_at_utc IS NOT NULL
       OR NEW.revoked_at_utc IS NULL
    THEN
        RAISE EXCEPTION 'EventVpnGateOverrides provenance is immutable' USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION reject_event_vpn_policy_audit_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'EventVpnPolicyAudit is append-only' USING ERRCODE = '55000';
END;
$$;

CREATE OR REPLACE FUNCTION reject_identity_observation_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'IdentityObservations is append-only' USING ERRCODE = '55000';
END;
$$;

CREATE OR REPLACE FUNCTION rsctf_reject_suspicion_event_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'SuspicionEvents is an immutable evidence ledger';
END;
$$;

DROP FUNCTION IF EXISTS rsctf_event_history_purge_authorized(INTEGER);
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(DOWN_SQL)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn immutable_event_history_requires_a_matching_pending_purge_operation() {
        assert!(UP_SQL.contains("current_setting('rsctf.event_history_purge_operation', TRUE)"));
        assert!(UP_SQL.contains("operation_id::text"));
        assert!(UP_SQL.contains("game_id = p_game_id"));
        assert!(UP_SQL.contains("status = 0"));
        assert_eq!(
            UP_SQL
                .matches("rsctf_event_history_purge_authorized(OLD.game_id)")
                .count(),
            5
        );
        assert!(UP_SQL.contains("TG_OP = 'DELETE' AND"));
        assert!(UP_SQL.contains("OLD.game_id IS NOT NULL"));
        assert!(UP_SQL.contains("IdentityObservations is append-only"));
        assert!(UP_SQL.contains("SuspicionEvents is an immutable evidence ledger"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn purge_authorization_is_transaction_local_game_bound_and_pending_only() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let mut tx = pool.begin().await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&mut *tx).await.unwrap();

        let operation_id = uuid::Uuid::new_v4();
        let game_id = 2_000_000_000;
        sqlx::query(
            r#"INSERT INTO "GamePurgeOperations"
                 (operation_id, game_id, actor_user_id, request_digest,
                  expected_configuration_revision, confirmation_title)
               VALUES ($1, $2, $3, $4, 0, 'migration-test')"#,
        )
        .bind(operation_id)
        .bind(game_id)
        .bind(uuid::Uuid::new_v4())
        .bind("0".repeat(64))
        .execute(&mut *tx)
        .await
        .unwrap();

        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE purge_suspicion_fixture (game_id INTEGER NOT NULL);
            CREATE TRIGGER purge_suspicion_fixture_guard
            BEFORE DELETE ON purge_suspicion_fixture
            FOR EACH ROW EXECUTE FUNCTION rsctf_reject_suspicion_event_mutation();
            CREATE TEMP TABLE purge_identity_fixture (game_id INTEGER);
            CREATE TRIGGER purge_identity_fixture_guard
            BEFORE DELETE ON purge_identity_fixture
            FOR EACH ROW EXECUTE FUNCTION reject_identity_observation_mutation();
            INSERT INTO purge_suspicion_fixture VALUES (2000000000), (1999999999);
            INSERT INTO purge_identity_fixture VALUES (2000000000), (1999999999), (NULL);
            "#,
        )
        .execute(&mut *tx)
        .await
        .unwrap();

        for (savepoint, table) in [
            ("before_suspicion_delete", "purge_suspicion_fixture"),
            ("before_identity_delete", "purge_identity_fixture"),
        ] {
            sqlx::query(&format!("SAVEPOINT {savepoint}"))
                .execute(&mut *tx)
                .await
                .unwrap();
            let error = sqlx::query(&format!("DELETE FROM {table} WHERE game_id = 2000000000"))
                .execute(&mut *tx)
                .await
                .unwrap_err();
            assert_eq!(
                error
                    .as_database_error()
                    .and_then(|db| db.code())
                    .as_deref(),
                Some("55000")
            );
            sqlx::query(&format!("ROLLBACK TO SAVEPOINT {savepoint}"))
                .execute(&mut *tx)
                .await
                .unwrap();
        }

        assert!(
            !sqlx::query_scalar::<_, bool>("SELECT rsctf_event_history_purge_authorized($1)",)
                .bind(game_id)
                .fetch_one(&mut *tx)
                .await
                .unwrap()
        );
        sqlx::query_scalar::<_, String>(
            "SELECT set_config('rsctf.event_history_purge_operation', $1::text, TRUE)",
        )
        .bind(operation_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert!(
            sqlx::query_scalar::<_, bool>("SELECT rsctf_event_history_purge_authorized($1)",)
                .bind(game_id)
                .fetch_one(&mut *tx)
                .await
                .unwrap()
        );
        assert_eq!(
            sqlx::query("DELETE FROM purge_suspicion_fixture WHERE game_id = $1")
                .bind(game_id)
                .execute(&mut *tx)
                .await
                .unwrap()
                .rows_affected(),
            1
        );
        assert_eq!(
            sqlx::query("DELETE FROM purge_identity_fixture WHERE game_id = $1")
                .bind(game_id)
                .execute(&mut *tx)
                .await
                .unwrap()
                .rows_affected(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM purge_suspicion_fixture WHERE game_id = $1",
            )
            .bind(game_id - 1)
            .fetch_one(&mut *tx)
            .await
            .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM purge_identity_fixture WHERE game_id IS NULL",
            )
            .fetch_one(&mut *tx)
            .await
            .unwrap(),
            1
        );
        assert!(
            !sqlx::query_scalar::<_, bool>("SELECT rsctf_event_history_purge_authorized($1)",)
                .bind(game_id - 1)
                .fetch_one(&mut *tx)
                .await
                .unwrap()
        );
        sqlx::query(
            r#"UPDATE "GamePurgeOperations"
                  SET status = 1, result = '{}'::jsonb,
                      completed_at_utc = clock_timestamp()
                WHERE operation_id = $1"#,
        )
        .bind(operation_id)
        .execute(&mut *tx)
        .await
        .unwrap();
        assert!(
            !sqlx::query_scalar::<_, bool>("SELECT rsctf_event_history_purge_authorized($1)",)
                .bind(game_id)
                .fetch_one(&mut *tx)
                .await
                .unwrap()
        );
        tx.rollback().await.unwrap();
    }
}
