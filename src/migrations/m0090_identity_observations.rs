//! Durable, timestamped identity observations for login-policy decisions.
//!
//! The legacy gate compared the one mutable IP/fingerprint stored on
//! `AspNetUsers` and used `last_visited_utc` as a proxy for when that value was
//! observed.  That both forgot older identities too early and made stale values
//! look recent.  These append-only rows carry their own observation time and an
//! immutable account id.  Fingerprints are stored only as domain-separated
//! hashes; `value_hint` is a short display prefix.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "IdentityObservations" (
    id BIGSERIAL PRIMARY KEY,
    user_id UUID NOT NULL,
    team_id INTEGER NULL,
    game_id INTEGER NULL,
    participation_id INTEGER NULL,
    kind TEXT NOT NULL CHECK (kind IN ('Ip', 'Fingerprint')),
    value_hash BYTEA NOT NULL CHECK (OCTET_LENGTH(value_hash) = 32),
    subnet_group_hash BYTEA NULL CHECK (
        subnet_group_hash IS NULL OR OCTET_LENGTH(subnet_group_hash) = 32
    ),
    broad_network_hash BYTEA NULL CHECK (
        broad_network_hash IS NULL OR OCTET_LENGTH(broad_network_hash) = 32
    ),
    value_hint TEXT NOT NULL,
    source TEXT NOT NULL,
    observed_at_utc TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT ck_identity_observation_context CHECK (
        (team_id IS NULL AND game_id IS NULL AND participation_id IS NULL)
        OR
        (team_id IS NOT NULL AND game_id IS NOT NULL AND participation_id IS NOT NULL)
    ),
    CONSTRAINT ck_identity_observation_network_hashes CHECK (
        (kind = 'Ip' AND subnet_group_hash IS NOT NULL AND broad_network_hash IS NOT NULL)
        OR
        (kind = 'Fingerprint' AND subnet_group_hash IS NULL AND broad_network_hash IS NULL)
    ),
    CONSTRAINT ck_identity_observation_source CHECK (
        source IN ('Registration', 'Password', 'OAuth', 'TeamJoin', 'GameJoin', 'Legacy')
    )
);

-- Reconcile databases that briefly ran an earlier pre-release definition of
-- this migration. CREATE TABLE IF NOT EXISTS cannot widen its inline CHECK.
DO $$
DECLARE source_constraint RECORD;
BEGIN
    FOR source_constraint IN
        SELECT conname
          FROM pg_constraint
         WHERE conrelid = '"IdentityObservations"'::regclass
           AND contype = 'c'
           AND pg_get_constraintdef(oid) ILIKE '%source%'
    LOOP
        EXECUTE format(
            'ALTER TABLE "IdentityObservations" DROP CONSTRAINT %I',
            source_constraint.conname
        );
    END LOOP;
    ALTER TABLE "IdentityObservations"
        ADD CONSTRAINT ck_identity_observation_source CHECK (
            source IN ('Registration', 'Password', 'OAuth', 'TeamJoin', 'GameJoin', 'Legacy')
        );
END $$;

CREATE OR REPLACE FUNCTION reject_identity_observation_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'IdentityObservations is append-only' USING ERRCODE = '55000';
END;
$$;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_trigger
         WHERE tgname = 'tr_identity_observations_append_only'
           AND tgrelid = '"IdentityObservations"'::regclass
    ) THEN
        CREATE TRIGGER tr_identity_observations_append_only
        BEFORE UPDATE OR DELETE ON "IdentityObservations"
        FOR EACH ROW EXECUTE FUNCTION reject_identity_observation_mutation();
    END IF;
END $$;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'fk_identity_observations_participation'
           AND conrelid = '"IdentityObservations"'::regclass
    ) THEN
        ALTER TABLE "IdentityObservations"
            ADD CONSTRAINT fk_identity_observations_participation
            FOREIGN KEY (game_id, team_id, participation_id)
            REFERENCES "Participations" (game_id, team_id, id)
            ON DELETE RESTRICT;
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS ix_identity_observations_match
    ON "IdentityObservations" (kind, value_hash, observed_at_utc DESC, user_id);
CREATE INDEX IF NOT EXISTS ix_identity_observations_user_time
    ON "IdentityObservations" (user_id, observed_at_utc DESC);
CREATE INDEX IF NOT EXISTS ix_identity_observations_global_user_identity_time
    ON "IdentityObservations"
       (user_id, kind, value_hash, observed_at_utc DESC, id DESC)
    WHERE team_id IS NULL AND game_id IS NULL AND participation_id IS NULL;
CREATE INDEX IF NOT EXISTS ix_identity_observations_game_time
    ON "IdentityObservations" (game_id, observed_at_utc, kind, value_hash)
    WHERE game_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_identity_observations_subnet_time
    ON "IdentityObservations" (subnet_group_hash, observed_at_utc DESC, user_id)
    WHERE subnet_group_hash IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_identity_observations_broad_network_time
    ON "IdentityObservations" (broad_network_hash, observed_at_utc DESC, user_id)
    WHERE broad_network_hash IS NOT NULL;

-- Runtime owns the keyed legacy backfill because SQL migrations never receive
-- the deployment HMAC key. A single transactional marker makes that bootstrap
-- atomic and idempotent across replicas and restarts.
CREATE TABLE IF NOT EXISTS "IdentityObservationBootstrapState" (
    version SMALLINT PRIMARY KEY CHECK (version = 1),
    key_identifier BYTEA NOT NULL CHECK (OCTET_LENGTH(key_identifier) = 32),
    completed_at_utc TIMESTAMPTZ NOT NULL,
    observations_inserted BIGINT NOT NULL CHECK (observations_inserted >= 0)
);

CREATE TABLE IF NOT EXISTS "FingerprintChallenges" (
    nonce_hash BYTEA PRIMARY KEY CHECK (OCTET_LENGTH(nonce_hash) = 32),
    required_signals TEXT[] NOT NULL,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at_utc TIMESTAMPTZ NOT NULL,
    consumed_at_utc TIMESTAMPTZ NULL,
    CONSTRAINT ck_fingerprint_challenge_expiry CHECK (expires_at_utc > created_at_utc)
);

CREATE INDEX IF NOT EXISTS ix_fingerprint_challenges_expiry
    ON "FingerprintChallenges" (expires_at_utc);

CREATE TABLE IF NOT EXISTS "AntiCheatExemptions" (
    id BIGSERIAL PRIMARY KEY,
    user_a UUID NOT NULL,
    user_b UUID NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('Ip', 'Fingerprint')),
    value_hash BYTEA NOT NULL CHECK (OCTET_LENGTH(value_hash) = 32),
    created_from_block_id INTEGER NOT NULL,
    created_by_user_id UUID NOT NULL,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at_utc TIMESTAMPTZ NOT NULL,
    revoked_at_utc TIMESTAMPTZ NULL,
    CONSTRAINT ck_anticheat_exemption_pair CHECK (user_a < user_b)
);

-- A renewed exemption is a new adjudication interval.  The original unique
-- scope constraint made an UPSERT rewrite created_at_utc and thereby changed
-- the result of historical correlation.  Drop it explicitly as well as
-- omitting it above so rerunning this still-in-development migration repairs
-- databases that applied an earlier form of m0090.
ALTER TABLE "AntiCheatExemptions"
    DROP CONSTRAINT IF EXISTS ux_anticheat_exemption_scope;

CREATE INDEX IF NOT EXISTS ix_anticheat_exemptions_active
    ON "AntiCheatExemptions" (user_a, user_b, kind, value_hash, expires_at_utc)
    WHERE revoked_at_utc IS NULL;

CREATE INDEX IF NOT EXISTS ix_anticheat_exemptions_interval
    ON "AntiCheatExemptions"
       (user_a, user_b, kind, value_hash, created_at_utc, expires_at_utc,
        revoked_at_utc);

ALTER TABLE "AntiCheatBlocks"
    ADD COLUMN IF NOT EXISTS conflicting_value_hash BYTEA NULL,
    ADD COLUMN IF NOT EXISTS adjudicated_at_utc TIMESTAMPTZ NULL,
    ADD COLUMN IF NOT EXISTS adjudicated_by_user_id UUID NULL,
    ADD COLUMN IF NOT EXISTS exemption_expires_at_utc TIMESTAMPTZ NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_anticheatblocks_value_hash'
           AND conrelid = '"AntiCheatBlocks"'::regclass
    ) THEN
        ALTER TABLE "AntiCheatBlocks"
            ADD CONSTRAINT ck_anticheatblocks_value_hash
            CHECK (conflicting_value_hash IS NULL OR OCTET_LENGTH(conflicting_value_hash) = 32);
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS ix_anticheatblocks_occurred_id
    ON "AntiCheatBlocks" (occurred_at_utc DESC, id DESC);

-- Once the keyed bootstrap marker commits, every account identity mutation
-- must carry evidence from the same transaction. This makes older application
-- replicas fail closed during a rolling upgrade instead of silently writing
-- mutable state that the new ledger cannot see. Deliberately identity-neutral
-- provisioning must opt in for this transaction only with:
--   SELECT set_config('rsctf.identity_neutral_insert', '1', true)
CREATE OR REPLACE FUNCTION guard_legacy_account_identity_write()
RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    bootstrap_complete BOOLEAN;
    neutral_insert BOOLEAN;
    has_evidence BOOLEAN;
BEGIN
    SELECT EXISTS (
        SELECT 1 FROM "IdentityObservationBootstrapState" WHERE version = 1
    ) INTO bootstrap_complete;
    IF NOT bootstrap_complete THEN
        RETURN NEW;
    END IF;

    neutral_insert := COALESCE(
        current_setting('rsctf.identity_neutral_insert', true), ''
    ) = '1';

    IF TG_OP = 'INSERT' THEN
        IF NEW.browser_fingerprint IS NOT NULL THEN
            RAISE EXCEPTION 'raw browser fingerprints are not permitted'
                USING ERRCODE = '55000';
        END IF;
        IF neutral_insert OR NOT NEW.email_confirmed OR NEW.role = 0 THEN
            RETURN NEW;
        END IF;
        SELECT EXISTS (
                   SELECT 1
                     FROM "IdentityObservations" observation
                    WHERE observation.user_id = NEW.id
                      AND observation.team_id IS NULL
                      AND observation.game_id IS NULL
                      AND observation.participation_id IS NULL
                      AND observation.source IN ('Registration', 'OAuth')
                      AND observation.xmin = pg_current_xact_id()::xid
               ) OR EXISTS (
                   SELECT 1
                     FROM "AntiCheatBlocks" block
                    WHERE block.user_id = NEW.id
                      AND block.xmin = pg_current_xact_id()::xid
               )
          INTO has_evidence;
        IF NOT has_evidence THEN
            RAISE EXCEPTION 'account insert lacks same-transaction identity adjudication'
                USING ERRCODE = '55000';
        END IF;
        RETURN NEW;
    END IF;

    IF NEW.browser_fingerprint IS NOT NULL
       AND NEW.browser_fingerprint IS DISTINCT FROM OLD.browser_fingerprint THEN
        RAISE EXCEPTION 'raw browser fingerprints are not permitted'
            USING ERRCODE = '55000';
    END IF;
    IF NEW.ip IS DISTINCT FROM OLD.ip
       OR NEW.last_signed_in_utc IS DISTINCT FROM OLD.last_signed_in_utc THEN
        SELECT EXISTS (
                   SELECT 1
                     FROM "IdentityObservations" observation
                    WHERE observation.user_id = NEW.id
                      AND observation.team_id IS NULL
                      AND observation.game_id IS NULL
                      AND observation.participation_id IS NULL
                      AND observation.kind = 'Ip'
                      AND observation.source IN ('Password', 'OAuth')
                      AND observation.observed_at_utc = NEW.last_signed_in_utc
                      AND observation.xmin = pg_current_xact_id()::xid
               )
          INTO has_evidence;
        IF NOT has_evidence THEN
            RAISE EXCEPTION 'account identity update lacks same-transaction accepted observation'
                USING ERRCODE = '55000';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS tr_guard_legacy_account_identity_write ON "AspNetUsers";
CREATE TRIGGER tr_guard_legacy_account_identity_write
AFTER INSERT OR UPDATE OF ip, browser_fingerprint, last_signed_in_utc
ON "AspNetUsers"
FOR EACH ROW EXECUTE FUNCTION guard_legacy_account_identity_write();

CREATE OR REPLACE FUNCTION guard_legacy_anticheat_block_write()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM "IdentityObservationBootstrapState" WHERE version = 1) THEN
        IF TG_OP = 'DELETE' THEN
            RAISE EXCEPTION 'anti-cheat audit rows cannot be deleted'
                USING ERRCODE = '55000';
        END IF;
        IF NEW.conflicting_value_hash IS NULL
           OR OCTET_LENGTH(NEW.conflicting_value_hash) <> 32 THEN
            RAISE EXCEPTION 'anti-cheat block lacks keyed identity hash'
                USING ERRCODE = '55000';
        END IF;
        IF NOT (
            NEW.conflicting_value = 'masked'
            OR (NEW.kind = 'Ip' AND (
                NEW.conflicting_value ~ '^[0-9]{1,3}(\.[0-9]{1,3}){2}\.x$'
                OR NEW.conflicting_value ~ '^[0-9a-fA-F:]+/64$'
            ))
            OR (NEW.kind = 'Fingerprint'
                AND NEW.conflicting_value ~ '^[0-9a-f]{12}…$')
        ) THEN
            RAISE EXCEPTION 'anti-cheat block contains an unmasked identity value'
                USING ERRCODE = '55000';
        END IF;
    END IF;
    RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
END;
$$;

DROP TRIGGER IF EXISTS tr_guard_legacy_anticheat_block_write ON "AntiCheatBlocks";
CREATE TRIGGER tr_guard_legacy_anticheat_block_write
BEFORE INSERT OR UPDATE OR DELETE ON "AntiCheatBlocks"
FOR EACH ROW EXECUTE FUNCTION guard_legacy_anticheat_block_write();

CREATE OR REPLACE FUNCTION guard_legacy_team_member_insert()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM "IdentityObservationBootstrapState" WHERE version = 1)
       AND COALESCE(current_setting('rsctf.identity_neutral_insert', true), '') <> '1'
       AND NOT EXISTS (
            SELECT 1
              FROM "IdentityObservations" observation
             WHERE observation.user_id = NEW.user_id
               AND observation.team_id IS NULL
               AND observation.game_id IS NULL
               AND observation.participation_id IS NULL
               AND observation.source = 'TeamJoin'
               AND observation.xmin = pg_current_xact_id()::xid
       ) THEN
        RAISE EXCEPTION 'team membership insert lacks same-transaction identity admission'
            USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS tr_guard_legacy_team_member_insert ON "TeamMembers";
CREATE TRIGGER tr_guard_legacy_team_member_insert
BEFORE INSERT ON "TeamMembers"
FOR EACH ROW EXECUTE FUNCTION guard_legacy_team_member_insert();

CREATE OR REPLACE FUNCTION redact_legacy_log_fingerprint()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM "IdentityObservationBootstrapState" WHERE version = 1) THEN
        NEW.browser_fingerprint := NULL;
        IF LOWER(NEW.logger) = 'fingerprint' THEN
            NEW.message := 'Legacy browser identity removed during security migration';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS tr_redact_legacy_log_fingerprint ON "Logs";
CREATE TRIGGER tr_redact_legacy_log_fingerprint
BEFORE INSERT OR UPDATE OF browser_fingerprint, message ON "Logs"
FOR EACH ROW EXECUTE FUNCTION redact_legacy_log_fingerprint();
"#;

const DOWN_SQL: &str = r#"
DROP TRIGGER IF EXISTS tr_guard_legacy_team_member_insert ON "TeamMembers";
DROP FUNCTION IF EXISTS guard_legacy_team_member_insert();
DROP TRIGGER IF EXISTS tr_redact_legacy_log_fingerprint ON "Logs";
DROP FUNCTION IF EXISTS redact_legacy_log_fingerprint();
DROP TRIGGER IF EXISTS tr_guard_legacy_anticheat_block_write ON "AntiCheatBlocks";
DROP FUNCTION IF EXISTS guard_legacy_anticheat_block_write();
DROP TRIGGER IF EXISTS tr_guard_legacy_account_identity_write ON "AspNetUsers";
DROP FUNCTION IF EXISTS guard_legacy_account_identity_write();
DROP INDEX IF EXISTS ix_anticheatblocks_occurred_id;

ALTER TABLE "IdentityObservations"
    DROP CONSTRAINT IF EXISTS fk_identity_observations_participation;

ALTER TABLE "AntiCheatBlocks"
    DROP CONSTRAINT IF EXISTS ck_anticheatblocks_value_hash,
    DROP COLUMN IF EXISTS exemption_expires_at_utc,
    DROP COLUMN IF EXISTS adjudicated_by_user_id,
    DROP COLUMN IF EXISTS adjudicated_at_utc,
    DROP COLUMN IF EXISTS conflicting_value_hash;

DROP TABLE IF EXISTS "AntiCheatExemptions";
DROP TABLE IF EXISTS "FingerprintChallenges";
DROP TABLE IF EXISTS "IdentityObservationBootstrapState";
DROP TABLE IF EXISTS "IdentityObservations";
DROP FUNCTION IF EXISTS reject_identity_observation_mutation();
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
    use super::*;
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use uuid::Uuid;

    #[test]
    fn migration_is_idempotent_and_keeps_raw_fingerprints_out() {
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"IdentityObservations\""));
        assert!(UP_SQL.contains("value_hash BYTEA NOT NULL"));
        assert!(UP_SQL.contains("observed_at_utc TIMESTAMPTZ NOT NULL"));
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"AntiCheatExemptions\""));
        assert!(UP_SQL.contains("DROP CONSTRAINT IF EXISTS ux_anticheat_exemption_scope"));
        assert!(UP_SQL.contains("ix_anticheat_exemptions_interval"));
        assert!(!UP_SQL.contains("CONSTRAINT ux_anticheat_exemption_scope UNIQUE"));
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"IdentityObservationBootstrapState\""));
        assert!(UP_SQL.contains("key_identifier BYTEA NOT NULL"));
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"FingerprintChallenges\""));
        assert!(UP_SQL.contains("consumed_at_utc TIMESTAMPTZ NULL"));
        assert!(UP_SQL.contains("BEFORE UPDATE OR DELETE ON \"IdentityObservations\""));
        assert!(UP_SQL.contains("FOREIGN KEY (game_id, team_id, participation_id)"));
        assert!(UP_SQL.contains("REFERENCES \"Participations\" (game_id, team_id, id)"));
        assert!(UP_SQL.contains("ADD COLUMN IF NOT EXISTS conflicting_value_hash"));
        assert!(UP_SQL.contains("pg_current_xact_id()::xid"));
        assert!(!UP_SQL.contains("pg_current_xact_id()::text::xid"));
        assert!(!UP_SQL.contains("fingerprint TEXT"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn completed_bootstrap_fences_old_writers_and_allows_adjudicated_inserts() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("identity_fence_{}", Uuid::new_v4().simple());
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
            r#"
            CREATE TABLE "AspNetUsers" (
                id UUID PRIMARY KEY, ip TEXT NOT NULL DEFAULT '0.0.0.0',
                browser_fingerprint TEXT,
                last_signed_in_utc TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                email_confirmed BOOLEAN NOT NULL DEFAULT TRUE,
                role SMALLINT NOT NULL DEFAULT 1
            );
            CREATE TABLE "Participations" (
                id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
                team_id INTEGER NOT NULL, UNIQUE(game_id,team_id,id)
            );
            CREATE TABLE "AntiCheatBlocks" (
                id SERIAL PRIMARY KEY, user_id UUID NOT NULL, user_name TEXT,
                conflict_user_id UUID, conflict_user_name TEXT, kind TEXT NOT NULL,
                conflicting_value TEXT, occurred_at_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "Logs" (
                id BIGSERIAL PRIMARY KEY, logger TEXT NOT NULL, message TEXT NOT NULL,
                browser_fingerprint TEXT
            );
            CREATE TABLE "TeamMembers" (
                team_id INTEGER NOT NULL, user_id UUID NOT NULL,
                PRIMARY KEY(team_id,user_id)
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::query(
            r#"INSERT INTO "IdentityObservationBootstrapState"
                 (version,key_identifier,completed_at_utc,observations_inserted)
               VALUES (1,$1,NOW(),0)"#,
        )
        .bind(vec![7_u8; 32])
        .execute(&pool)
        .await
        .unwrap();

        let unmarked = Uuid::new_v4();
        let error = sqlx::query(r#"INSERT INTO "AspNetUsers" (id) VALUES ($1)"#)
            .bind(unmarked)
            .execute(&pool)
            .await
            .unwrap_err();
        assert!(
            matches!(&error, sqlx::Error::Database(db) if db.code().as_deref() == Some("55000"))
        );

        let neutral = Uuid::new_v4();
        let mut transaction = pool.begin().await.unwrap();
        sqlx::query("SELECT set_config('rsctf.identity_neutral_insert','1',true)")
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "AspNetUsers" (id) VALUES ($1)"#)
            .bind(neutral)
            .execute(&mut *transaction)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let legacy_join =
            sqlx::query(r#"INSERT INTO "TeamMembers" (team_id,user_id) VALUES (10,$1)"#)
                .bind(neutral)
                .execute(&pool)
                .await
                .unwrap_err();
        assert!(
            matches!(&legacy_join, sqlx::Error::Database(db) if db.code().as_deref() == Some("55000"))
        );
        let mut transaction = pool.begin().await.unwrap();
        sqlx::query(
            r#"INSERT INTO "IdentityObservations"
                 (user_id,kind,value_hash,subnet_group_hash,broad_network_hash,
                  value_hint,source,observed_at_utc)
               VALUES ($1,'Ip',$2,$3,$4,'198.51.100.x','TeamJoin',NOW())"#,
        )
        .bind(neutral)
        .bind(vec![8_u8; 32])
        .bind(vec![9_u8; 32])
        .bind(vec![10_u8; 32])
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(r#"INSERT INTO "TeamMembers" (team_id,user_id) VALUES (10,$1)"#)
            .bind(neutral)
            .execute(&mut *transaction)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let accepted = Uuid::new_v4();
        let accepted_at = chrono::Utc::now();
        let mut transaction = pool.begin().await.unwrap();
        sqlx::query(
            r#"INSERT INTO "IdentityObservations"
                 (user_id,kind,value_hash,subnet_group_hash,broad_network_hash,
                  value_hint,source,observed_at_utc)
               VALUES ($1,'Ip',$2,$3,$4,'192.0.2.x','Registration',$5)"#,
        )
        .bind(accepted)
        .bind(vec![1_u8; 32])
        .bind(vec![2_u8; 32])
        .bind(vec![3_u8; 32])
        .bind(accepted_at)
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "AspNetUsers" (id,ip,last_signed_in_utc)
               VALUES ($1,'192.0.2.10',$2)"#,
        )
        .bind(accepted)
        .bind(accepted_at)
        .execute(&mut *transaction)
        .await
        .unwrap();
        transaction.commit().await.unwrap();

        let block_user = Uuid::new_v4();
        let mut transaction = pool.begin().await.unwrap();
        let block_id: i32 = sqlx::query_scalar(
            r#"INSERT INTO "AntiCheatBlocks"
                 (user_id,kind,conflicting_value,conflicting_value_hash,occurred_at_utc)
               VALUES ($1,'Ip','203.0.113.x',$2,NOW()) RETURNING id"#,
        )
        .bind(block_user)
        .bind(vec![11_u8; 32])
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(r#"INSERT INTO "AspNetUsers" (id) VALUES ($1)"#)
            .bind(block_user)
            .execute(&mut *transaction)
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        let raw_block_update = sqlx::query(
            r#"UPDATE "AntiCheatBlocks" SET conflicting_value='203.0.113.7' WHERE id=$1"#,
        )
        .bind(block_id)
        .execute(&pool)
        .await
        .unwrap_err();
        assert!(
            matches!(&raw_block_update, sqlx::Error::Database(db) if db.code().as_deref() == Some("55000"))
        );
        let block_delete = sqlx::query(r#"DELETE FROM "AntiCheatBlocks" WHERE id=$1"#)
            .bind(block_id)
            .execute(&pool)
            .await
            .unwrap_err();
        assert!(
            matches!(&block_delete, sqlx::Error::Database(db) if db.code().as_deref() == Some("55000"))
        );

        let rejected_update = sqlx::query(
            r#"UPDATE "AspNetUsers" SET ip='198.51.100.7', last_signed_in_utc=NOW()
                WHERE id=$1"#,
        )
        .bind(accepted)
        .execute(&pool)
        .await
        .unwrap_err();
        assert!(
            matches!(&rejected_update, sqlx::Error::Database(db) if db.code().as_deref() == Some("55000"))
        );

        let login_at = chrono::Utc::now() + chrono::Duration::seconds(1);
        let mut transaction = pool.begin().await.unwrap();
        sqlx::query(
            r#"INSERT INTO "IdentityObservations"
                 (user_id,kind,value_hash,subnet_group_hash,broad_network_hash,
                  value_hint,source,observed_at_utc)
               VALUES ($1,'Ip',$2,$3,$4,'198.51.100.x','Password',$5)"#,
        )
        .bind(accepted)
        .bind(vec![4_u8; 32])
        .bind(vec![5_u8; 32])
        .bind(vec![6_u8; 32])
        .bind(login_at)
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            r#"UPDATE "AspNetUsers" SET ip='198.51.100.7', last_signed_in_utc=$2
                WHERE id=$1"#,
        )
        .bind(accepted)
        .bind(login_at)
        .execute(&mut *transaction)
        .await
        .unwrap();
        transaction.commit().await.unwrap();

        let raw_fingerprint = "a".repeat(64);
        let raw_insert =
            sqlx::query(r#"INSERT INTO "AspNetUsers" (id,browser_fingerprint) VALUES ($1,$2)"#)
                .bind(Uuid::new_v4())
                .bind(&raw_fingerprint)
                .execute(&pool)
                .await
                .unwrap_err();
        assert!(
            matches!(&raw_insert, sqlx::Error::Database(db) if db.code().as_deref() == Some("55000"))
        );
        let stored_log_fingerprint: Option<String> = sqlx::query_scalar(
            r#"INSERT INTO "Logs" (logger,message,browser_fingerprint)
               VALUES ('fingerprint','raw',$1) RETURNING browser_fingerprint"#,
        )
        .bind(raw_fingerprint)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(stored_log_fingerprint.is_none());
    }
}
