//! Exactly-once temporary Event-VPN override transitions and bounded history.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "EventVpnGateOverrides"
    ADD COLUMN IF NOT EXISTS policy_revision BIGINT NULL,
    ADD COLUMN IF NOT EXISTS revoked_by_user_id UUID NULL,
    ADD COLUMN IF NOT EXISTS revoke_policy_revision BIGINT NULL;

CREATE TABLE IF NOT EXISTS "EventVpnOverrideOperations" (
    game_id            INTEGER NOT NULL,
    operation_id       UUID NOT NULL,
    actor_user_id      UUID NOT NULL,
    action             VARCHAR(8) NOT NULL,
    override_id        UUID NOT NULL,
    request_digest     BYTEA NOT NULL CHECK (OCTET_LENGTH(request_digest) = 32),
    result_revision    BIGINT NOT NULL CHECK (result_revision >= 1),
    created_at_utc     TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (game_id, operation_id),
    CONSTRAINT fk_event_vpn_override_operation_game
        FOREIGN KEY (game_id) REFERENCES "Games"(id) ON DELETE RESTRICT,
    CONSTRAINT fk_event_vpn_override_operation_actor
        FOREIGN KEY (actor_user_id) REFERENCES "AspNetUsers"(id) ON DELETE RESTRICT,
    CONSTRAINT fk_event_vpn_override_operation_override
        FOREIGN KEY (override_id) REFERENCES "EventVpnGateOverrides"(id) ON DELETE RESTRICT,
    CONSTRAINT ck_event_vpn_override_operation_action
        CHECK (action IN ('create', 'revoke'))
);

CREATE INDEX IF NOT EXISTS ix_event_vpn_override_operations_retention
    ON "EventVpnOverrideOperations" (created_at_utc, game_id, operation_id);
CREATE INDEX IF NOT EXISTS ix_event_vpn_gate_overrides_active_complete
    ON "EventVpnGateOverrides" (game_id, created_at_utc DESC, id DESC)
    WHERE revoked_at_utc IS NULL;

-- Preserve an immutable 30-day audit window while allowing the bounded
-- maintenance path to remove terminal history after its operation record has
-- expired. Active grants can never be deleted.
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
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "EventVpnOverrideOperations";
DROP INDEX IF EXISTS ix_event_vpn_gate_overrides_active_complete;
CREATE OR REPLACE FUNCTION guard_event_vpn_override_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'EventVpnGateOverrides cannot be deleted' USING ERRCODE = '55000';
    END IF;
    IF OLD.id IS DISTINCT FROM NEW.id
       OR OLD.game_id IS DISTINCT FROM NEW.game_id
       OR OLD.created_by_user_id IS DISTINCT FROM NEW.created_by_user_id
       OR OLD.reason IS DISTINCT FROM NEW.reason
       OR OLD.created_at_utc IS DISTINCT FROM NEW.created_at_utc
       OR OLD.expires_at_utc IS DISTINCT FROM NEW.expires_at_utc
       OR OLD.revoked_at_utc IS NOT NULL
       OR NEW.revoked_at_utc IS NULL
    THEN
        RAISE EXCEPTION 'EventVpnGateOverrides provenance is immutable' USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END;
$$;
ALTER TABLE "EventVpnGateOverrides"
    DROP COLUMN IF EXISTS revoke_policy_revision,
    DROP COLUMN IF EXISTS revoked_by_user_id,
    DROP COLUMN IF EXISTS policy_revision;
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
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use uuid::Uuid;

    use super::{DOWN_SQL, UP_SQL};

    #[test]
    fn operation_identity_and_active_listing_are_indexed() {
        assert!(UP_SQL.contains("PRIMARY KEY (game_id, operation_id)"));
        assert!(UP_SQL.contains("OCTET_LENGTH(request_digest) = 32"));
        assert!(UP_SQL.contains("WHERE revoked_at_utc IS NULL"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn operation_uniqueness_retention_and_trigger_upgrade_are_enforced() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("vpn_override_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Games" (id INTEGER PRIMARY KEY);
            CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY);
            CREATE TABLE "EventVpnGateOverrides" (
              id UUID PRIMARY KEY,
              game_id INTEGER NOT NULL,
              created_by_user_id UUID NOT NULL,
              reason TEXT NOT NULL,
              created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
              expires_at_utc TIMESTAMPTZ NOT NULL,
              revoked_at_utc TIMESTAMPTZ NULL
            );
            CREATE OR REPLACE FUNCTION guard_event_vpn_override_mutation()
            RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN
              IF TG_OP = 'DELETE' THEN RAISE EXCEPTION 'immutable' USING ERRCODE = '55000'; END IF;
              IF OLD.revoked_at_utc IS NOT NULL OR NEW.revoked_at_utc IS NULL
              THEN RAISE EXCEPTION 'immutable' USING ERRCODE = '55000'; END IF;
              RETURN NEW;
            END;
            $$;
            CREATE TRIGGER tr_event_vpn_gate_overrides_immutable
              BEFORE UPDATE OR DELETE ON "EventVpnGateOverrides"
              FOR EACH ROW EXECUTE FUNCTION guard_event_vpn_override_mutation();
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();

        let game_id = 7;
        let actor = Uuid::new_v4();
        let override_id = Uuid::new_v4();
        let operation_id = Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "Games" (id) VALUES ($1)"#)
            .bind(game_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "AspNetUsers" (id) VALUES ($1)"#)
            .bind(actor)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "EventVpnGateOverrides"
                 (id,game_id,created_by_user_id,reason,expires_at_utc,policy_revision)
               VALUES ($1,$2,$3,'incident response',clock_timestamp()+interval '15 minutes',2)"#,
        )
        .bind(override_id)
        .bind(game_id)
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
        let insert_operation = || {
            sqlx::query(
                r#"INSERT INTO "EventVpnOverrideOperations"
                     (game_id,operation_id,actor_user_id,action,override_id,request_digest,result_revision)
                   VALUES ($1,$2,$3,'create',$4,$5,2)"#,
            )
            .bind(game_id)
            .bind(operation_id)
            .bind(actor)
            .bind(override_id)
            .bind(vec![7_u8; 32])
        };
        insert_operation().execute(&pool).await.unwrap();
        let duplicate = insert_operation().execute(&pool).await.unwrap_err();
        assert_eq!(
            duplicate.as_database_error().unwrap().code().as_deref(),
            Some("23505")
        );

        let immutable =
            sqlx::query(r#"UPDATE "EventVpnGateOverrides" SET reason='changed' WHERE id=$1"#)
                .bind(override_id)
                .execute(&pool)
                .await
                .unwrap_err();
        assert_eq!(
            immutable.as_database_error().unwrap().code().as_deref(),
            Some("55000")
        );
        sqlx::query(
            r#"UPDATE "EventVpnGateOverrides"
                  SET revoked_at_utc=clock_timestamp(),revoked_by_user_id=$2,revoke_policy_revision=3
                WHERE id=$1"#,
        )
        .bind(override_id)
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(r#"DELETE FROM "EventVpnOverrideOperations""#)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(DOWN_SQL).execute(&pool).await.unwrap();
        let second_revoke = sqlx::query(
            r#"UPDATE "EventVpnGateOverrides" SET revoked_at_utc=clock_timestamp() WHERE id=$1"#,
        )
        .bind(override_id)
        .execute(&pool)
        .await
        .unwrap_err();
        assert_eq!(
            second_revoke.as_database_error().unwrap().code().as_deref(),
            Some("55000")
        );

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
