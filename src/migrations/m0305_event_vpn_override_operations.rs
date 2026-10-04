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
    result_revision    BIGINT NOT NULL,
    created_at_utc     TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (game_id, operation_id),
    CONSTRAINT fk_event_vpn_override_operation_game
        FOREIGN KEY (game_id) REFERENCES "Games"(id) ON DELETE RESTRICT,
    CONSTRAINT fk_event_vpn_override_operation_actor
        FOREIGN KEY (actor_user_id) REFERENCES "AspNetUsers"(id) ON DELETE RESTRICT,
    CONSTRAINT fk_event_vpn_override_operation_override
        FOREIGN KEY (override_id) REFERENCES "EventVpnGateOverrides"(id) ON DELETE RESTRICT,
    CONSTRAINT ck_event_vpn_override_operation_action
        CHECK (action IN ('create', 'revoke')),
    CONSTRAINT ck_event_vpn_override_operation_result_revision
        CHECK (result_revision BETWEEN 1 AND 9007199254740991)
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_event_vpn_gate_overrides_game_id
    ON "EventVpnGateOverrides" (game_id, id);
CREATE INDEX IF NOT EXISTS ix_event_vpn_override_operations_retention
    ON "EventVpnOverrideOperations" (created_at_utc, game_id, operation_id);
CREATE INDEX IF NOT EXISTS ix_event_vpn_gate_overrides_active_complete
    ON "EventVpnGateOverrides" (game_id, created_at_utc DESC, id DESC)
    WHERE revoked_at_utc IS NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_games_vpn_policy_revision_js_safe'
           AND conrelid = '"Games"'::regclass
    ) THEN
        ALTER TABLE "Games"
            ADD CONSTRAINT ck_games_vpn_policy_revision_js_safe
            CHECK (vpn_policy_revision BETWEEN 1 AND 9007199254740991) NOT VALID;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_event_vpn_gate_override_policy_revision_js_safe'
           AND conrelid = '"EventVpnGateOverrides"'::regclass
    ) THEN
        ALTER TABLE "EventVpnGateOverrides"
            ADD CONSTRAINT ck_event_vpn_gate_override_policy_revision_js_safe
            CHECK (
                policy_revision IS NULL
                OR policy_revision BETWEEN 1 AND 9007199254740991
            ) NOT VALID;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_event_vpn_gate_override_revoke_audit_pair'
           AND conrelid = '"EventVpnGateOverrides"'::regclass
    ) THEN
        -- Historical overrides may have been revoked before actor and revision
        -- metadata existed. NOT VALID preserves them while enforcing complete,
        -- ordered audit metadata on every new or updated row.
        ALTER TABLE "EventVpnGateOverrides"
            ADD CONSTRAINT ck_event_vpn_gate_override_revoke_audit_pair CHECK (
                (
                    revoked_at_utc IS NULL
                    AND revoked_by_user_id IS NULL
                    AND revoke_policy_revision IS NULL
                ) OR (
                    revoked_at_utc IS NOT NULL
                    AND revoked_by_user_id IS NOT NULL
                    AND revoke_policy_revision BETWEEN 1 AND 9007199254740991
                    AND (
                        policy_revision IS NULL
                        OR revoke_policy_revision >= policy_revision
                    )
                )
            ) NOT VALID;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'fk_event_vpn_gate_override_revoker'
           AND conrelid = '"EventVpnGateOverrides"'::regclass
    ) THEN
        ALTER TABLE "EventVpnGateOverrides"
            ADD CONSTRAINT fk_event_vpn_gate_override_revoker
            FOREIGN KEY (revoked_by_user_id)
            REFERENCES "AspNetUsers"(id) ON DELETE RESTRICT NOT VALID;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_event_vpn_override_operation_result_revision'
           AND conrelid = '"EventVpnOverrideOperations"'::regclass
    ) THEN
        ALTER TABLE "EventVpnOverrideOperations"
            ADD CONSTRAINT ck_event_vpn_override_operation_result_revision
            CHECK (result_revision BETWEEN 1 AND 9007199254740991) NOT VALID;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'fk_event_vpn_override_operation_override_pair'
           AND conrelid = '"EventVpnOverrideOperations"'::regclass
    ) THEN
        ALTER TABLE "EventVpnOverrideOperations"
            ADD CONSTRAINT fk_event_vpn_override_operation_override_pair
            FOREIGN KEY (game_id, override_id)
            REFERENCES "EventVpnGateOverrides"(game_id, id)
            ON DELETE RESTRICT NOT VALID;
    END IF;
END $$;

ALTER TABLE "Games"
    VALIDATE CONSTRAINT ck_games_vpn_policy_revision_js_safe;
ALTER TABLE "EventVpnGateOverrides"
    VALIDATE CONSTRAINT ck_event_vpn_gate_override_policy_revision_js_safe;
ALTER TABLE "EventVpnGateOverrides"
    VALIDATE CONSTRAINT fk_event_vpn_gate_override_revoker;
ALTER TABLE "EventVpnOverrideOperations"
    VALIDATE CONSTRAINT ck_event_vpn_override_operation_result_revision;
ALTER TABLE "EventVpnOverrideOperations"
    VALIDATE CONSTRAINT fk_event_vpn_override_operation_override_pair;

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
DROP INDEX IF EXISTS ux_event_vpn_gate_overrides_game_id;
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
ALTER TABLE "Games"
    DROP CONSTRAINT IF EXISTS ck_games_vpn_policy_revision_js_safe;
ALTER TABLE "EventVpnGateOverrides"
    DROP CONSTRAINT IF EXISTS ck_event_vpn_gate_override_revoke_audit_pair,
    DROP CONSTRAINT IF EXISTS ck_event_vpn_gate_override_policy_revision_js_safe,
    DROP CONSTRAINT IF EXISTS fk_event_vpn_gate_override_revoker,
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
        assert!(UP_SQL.contains("fk_event_vpn_override_operation_override_pair"));
        assert!(UP_SQL.contains("FOREIGN KEY (game_id, override_id)"));
        assert!(UP_SQL.contains("fk_event_vpn_gate_override_revoker"));
        assert!(UP_SQL.contains("ck_event_vpn_gate_override_revoke_audit_pair"));
        assert!(UP_SQL.contains("revoke_policy_revision >= policy_revision"));
        assert!(UP_SQL.contains("BETWEEN 1 AND 9007199254740991"));
        assert!(UP_SQL.contains("NOT VALID preserves them"));
        assert!(DOWN_SQL.contains("DROP CONSTRAINT IF EXISTS fk_event_vpn_gate_override_revoker"));
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
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY,
              vpn_policy_revision BIGINT NOT NULL DEFAULT 1
            );
            CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY);
            CREATE TABLE "EventVpnGateOverrides" (
              id UUID PRIMARY KEY,
              game_id INTEGER NOT NULL,
              created_by_user_id UUID NOT NULL,
              reason TEXT NOT NULL,
              created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
              expires_at_utc TIMESTAMPTZ NOT NULL,
              revoked_at_utc TIMESTAMPTZ NULL,
              CONSTRAINT fk_event_vpn_gate_override_game
                FOREIGN KEY (game_id) REFERENCES "Games"(id) ON DELETE RESTRICT,
              CONSTRAINT fk_event_vpn_gate_override_actor
                FOREIGN KEY (created_by_user_id) REFERENCES "AspNetUsers"(id) ON DELETE RESTRICT,
              CONSTRAINT ck_event_vpn_gate_override_window CHECK (
                expires_at_utc > created_at_utc
                AND expires_at_utc <= created_at_utc + INTERVAL '60 minutes'
                AND (revoked_at_utc IS NULL OR revoked_at_utc >= created_at_utc)
              )
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
        sqlx::query(r#"INSERT INTO "Games" (id) VALUES ($1), ($2)"#)
            .bind(game_id)
            .bind(game_id + 1)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "AspNetUsers" (id) VALUES ($1)"#)
            .bind(actor)
            .execute(&pool)
            .await
            .unwrap();
        let exact_boundary = sqlx::query_scalar::<_, bool>(
            r#"WITH observed_clock AS MATERIALIZED (
                   SELECT clock_timestamp() AS now
               )
               INSERT INTO "EventVpnGateOverrides"
                 (id,game_id,created_by_user_id,reason,created_at_utc,expires_at_utc,policy_revision)
               SELECT $1,$2,$3,'incident response',observed_clock.now,
                      observed_clock.now + interval '60 minutes',2
                 FROM observed_clock
               RETURNING expires_at_utc - created_at_utc = interval '60 minutes'"#,
        )
        .bind(override_id)
        .bind(game_id)
        .bind(actor)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(exact_boundary);
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

        let replay = || {
            sqlx::query_as::<_, (Uuid, String, Uuid, Vec<u8>, i64)>(
                r#"SELECT actor_user_id, action, override_id, request_digest, result_revision
                     FROM "EventVpnOverrideOperations"
                    WHERE game_id=$1 AND operation_id=$2"#,
            )
            .bind(game_id)
            .bind(operation_id)
        };
        let first_replay = replay().fetch_one(&pool).await.unwrap();
        let second_replay = replay().fetch_one(&pool).await.unwrap();
        assert_eq!(first_replay, second_replay);
        assert_eq!(first_replay.0, actor);
        assert_eq!(first_replay.1, "create");
        assert_eq!(first_replay.2, override_id);
        assert_eq!(first_replay.3, vec![7_u8; 32]);
        assert_eq!(first_replay.4, 2);

        let duplicate = insert_operation().execute(&pool).await.unwrap_err();
        assert_eq!(
            duplicate.as_database_error().unwrap().code().as_deref(),
            Some("23505")
        );

        let cross_game = sqlx::query(
            r#"INSERT INTO "EventVpnOverrideOperations"
                 (game_id,operation_id,actor_user_id,action,override_id,request_digest,result_revision)
               VALUES ($1,$2,$3,'create',$4,$5,2)"#,
        )
        .bind(game_id + 1)
        .bind(Uuid::new_v4())
        .bind(actor)
        .bind(override_id)
        .bind(vec![8_u8; 32])
        .execute(&pool)
        .await
        .unwrap_err();
        assert_eq!(
            cross_game.as_database_error().unwrap().code().as_deref(),
            Some("23503")
        );

        let unsafe_result_revision = sqlx::query(
            r#"INSERT INTO "EventVpnOverrideOperations"
                 (game_id,operation_id,actor_user_id,action,override_id,request_digest,result_revision)
               VALUES ($1,$2,$3,'create',$4,$5,9007199254740992)"#,
        )
        .bind(game_id)
        .bind(Uuid::new_v4())
        .bind(actor)
        .bind(override_id)
        .bind(vec![9_u8; 32])
        .execute(&pool)
        .await
        .unwrap_err();
        assert_eq!(
            unsafe_result_revision
                .as_database_error()
                .unwrap()
                .code()
                .as_deref(),
            Some("23514")
        );

        let unsafe_game_revision =
            sqlx::query(r#"UPDATE "Games" SET vpn_policy_revision=9007199254740992 WHERE id=$1"#)
                .bind(game_id + 1)
                .execute(&pool)
                .await
                .unwrap_err();
        assert_eq!(
            unsafe_game_revision
                .as_database_error()
                .unwrap()
                .code()
                .as_deref(),
            Some("23514")
        );

        let unsafe_override_revision = sqlx::query(
            r#"WITH observed_clock AS MATERIALIZED (
                   SELECT clock_timestamp() AS now
               )
               INSERT INTO "EventVpnGateOverrides"
                 (id,game_id,created_by_user_id,reason,created_at_utc,expires_at_utc,policy_revision)
               SELECT $1,$2,$3,'unsafe revision',observed_clock.now,
                      observed_clock.now + interval '15 minutes',9007199254740992
                 FROM observed_clock"#,
        )
        .bind(Uuid::new_v4())
        .bind(game_id)
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap_err();
        assert_eq!(
            unsafe_override_revision
                .as_database_error()
                .unwrap()
                .code()
                .as_deref(),
            Some("23514")
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

        let partial_revoke = sqlx::query(
            r#"UPDATE "EventVpnGateOverrides"
                  SET revoked_at_utc=clock_timestamp()
                WHERE id=$1"#,
        )
        .bind(override_id)
        .execute(&pool)
        .await
        .unwrap_err();
        assert_eq!(
            partial_revoke
                .as_database_error()
                .unwrap()
                .code()
                .as_deref(),
            Some("23514")
        );

        let unknown_revoker = sqlx::query(
            r#"UPDATE "EventVpnGateOverrides"
                  SET revoked_at_utc=clock_timestamp(),revoked_by_user_id=$2,revoke_policy_revision=3
                WHERE id=$1"#,
        )
        .bind(override_id)
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap_err();
        assert_eq!(
            unknown_revoker
                .as_database_error()
                .unwrap()
                .code()
                .as_deref(),
            Some("23503")
        );

        let regressed_revision = sqlx::query(
            r#"UPDATE "EventVpnGateOverrides"
                  SET revoked_at_utc=clock_timestamp(),revoked_by_user_id=$2,revoke_policy_revision=1
                WHERE id=$1"#,
        )
        .bind(override_id)
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap_err();
        assert_eq!(
            regressed_revision
                .as_database_error()
                .unwrap()
                .code()
                .as_deref(),
            Some("23514")
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
