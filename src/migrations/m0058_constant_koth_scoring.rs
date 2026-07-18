//! Make KotH scoring constant-only while preserving fixed-value schema shims for
//! live pre-upgrade writers during the expand phase.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                -- The runtime has only ever materialized crown-cycle data with
                -- formula 2. Refuse to relabel unknown official evidence.
                DO $$
                DECLARE
                  unsupported BOOLEAN := FALSE;
                BEGIN
                  IF EXISTS (
                    SELECT 1 FROM information_schema.columns
                     WHERE table_schema = 'public'
                       AND table_name = 'KothOfficialConfigs'
                       AND column_name = 'formula_version'
                  ) THEN
                    EXECUTE 'SELECT EXISTS (
                      SELECT 1 FROM "KothOfficialConfigs" WHERE formula_version <> 2
                    )' INTO unsupported;
                  END IF;
                  IF NOT unsupported AND EXISTS (
                    SELECT 1 FROM information_schema.columns
                     WHERE table_schema = 'public'
                       AND table_name = 'KothCrownCycles'
                       AND column_name = 'formula_version'
                  ) THEN
                    EXECUTE 'SELECT EXISTS (
                      SELECT 1 FROM "KothCrownCycles" WHERE formula_version <> 2
                    )' INTO unsupported;
                  END IF;
                  IF unsupported THEN
                    RAISE EXCEPTION
                      'cannot remove KotH formula selector: unsupported official evidence exists'
                      USING HINT = 'Archive or remove non-current KotH official data before retrying.';
                  END IF;
                END
                $$;

                -- Legacy pre-crown rollups used a different formula. They were
                -- already excluded from every current score; retain only the
                -- current formula before making the identity versionless.
                DO $$
                BEGIN
                  IF EXISTS (
                    SELECT 1 FROM information_schema.columns
                     WHERE table_schema = 'public'
                       AND table_name = 'KothEpochRollups'
                       AND column_name = 'formula_version'
                  ) THEN
                    EXECUTE 'DELETE FROM "KothEpochRollups" WHERE formula_version <> 2';
                  END IF;
                END
                $$;

                -- Expand/contract rollout shim: runtime scoring has no selector,
                -- but an already-running pre-upgrade process still names these
                -- columns and conflict targets. Keep them pinned to the sole
                -- constant during the rolling upgrade while making the
                -- versionless keys authoritative for the new binary.
                ALTER TABLE "Games"
                  ADD COLUMN IF NOT EXISTS koth_scoring_formula_version
                    SMALLINT NOT NULL DEFAULT 2;
                UPDATE "Games" SET koth_scoring_formula_version = 2;
                ALTER TABLE "Games"
                  ALTER COLUMN koth_scoring_formula_version SET DEFAULT 2,
                  ALTER COLUMN koth_scoring_formula_version SET NOT NULL,
                  DROP CONSTRAINT IF EXISTS ck_games_koth_scoring_formula_version,
                  ADD CONSTRAINT ck_games_koth_scoring_formula_version
                    CHECK (koth_scoring_formula_version = 2);

                -- Remove dependants before rebuilding the configuration's
                -- compatibility unique key.
                ALTER TABLE "KothCrownCycles"
                  DROP CONSTRAINT IF EXISTS fk_koth_crown_cycles_config,
                  DROP CONSTRAINT IF EXISTS fk_koth_crown_cycles_config_compat,
                  DROP CONSTRAINT IF EXISTS ux_koth_crown_cycles_identity,
                  DROP CONSTRAINT IF EXISTS ux_koth_crown_cycles_identity_compat,
                  DROP CONSTRAINT IF EXISTS ck_koth_crown_cycles_identity;

                ALTER TABLE "KothOfficialConfigs"
                  ADD COLUMN IF NOT EXISTS formula_version SMALLINT NOT NULL DEFAULT 2,
                  ALTER COLUMN formula_version SET DEFAULT 2,
                  ALTER COLUMN formula_version SET NOT NULL,
                  DROP CONSTRAINT IF EXISTS ux_koth_official_configs_game_formula,
                  DROP CONSTRAINT IF EXISTS ck_koth_official_configs_version,
                  ADD CONSTRAINT ux_koth_official_configs_game_formula
                    UNIQUE (game_id, formula_version),
                  ADD CONSTRAINT ck_koth_official_configs_version
                    CHECK (formula_version = 2);

                ALTER TABLE "KothCrownCycles"
                  ADD COLUMN IF NOT EXISTS formula_version SMALLINT NOT NULL DEFAULT 2,
                  ALTER COLUMN formula_version SET DEFAULT 2,
                  ALTER COLUMN formula_version SET NOT NULL,
                  ADD CONSTRAINT ux_koth_crown_cycles_identity
                    UNIQUE (game_id, challenge_id, cycle_number),
                  ADD CONSTRAINT ux_koth_crown_cycles_identity_compat
                    UNIQUE (game_id, challenge_id, formula_version, cycle_number),
                  ADD CONSTRAINT fk_koth_crown_cycles_config
                    FOREIGN KEY (game_id) REFERENCES "KothOfficialConfigs"(game_id)
                    ON DELETE CASCADE,
                  ADD CONSTRAINT fk_koth_crown_cycles_config_compat
                    FOREIGN KEY (game_id, formula_version)
                    REFERENCES "KothOfficialConfigs"(game_id, formula_version)
                    ON DELETE CASCADE,
                  ADD CONSTRAINT ck_koth_crown_cycles_identity
                    CHECK (formula_version = 2 AND cycle_number >= 1 AND epoch >= 1);

                ALTER TABLE "KothEpochTeamRollups"
                  DROP CONSTRAINT IF EXISTS
                    "KothEpochTeamRollups_game_id_formula_version_epoch_fkey",
                  DROP CONSTRAINT IF EXISTS
                    "KothEpochTeamRollups_game_id_epoch_fkey",
                  DROP CONSTRAINT IF EXISTS "KothEpochTeamRollups_pkey",
                  DROP CONSTRAINT IF EXISTS ux_koth_epoch_team_rollups_compat,
                  DROP CONSTRAINT IF EXISTS ck_koth_epoch_team_rollups_constant_formula;
                ALTER TABLE "KothEpochHillRollups"
                  DROP CONSTRAINT IF EXISTS
                    "KothEpochHillRollups_game_id_formula_version_epoch_fkey",
                  DROP CONSTRAINT IF EXISTS
                    "KothEpochHillRollups_game_id_epoch_fkey",
                  DROP CONSTRAINT IF EXISTS "KothEpochHillRollups_pkey",
                  DROP CONSTRAINT IF EXISTS ux_koth_epoch_hill_rollups_compat,
                  DROP CONSTRAINT IF EXISTS ck_koth_epoch_hill_rollups_constant_formula;
                DROP INDEX IF EXISTS ix_koth_epoch_team_rollups_latest;
                DROP INDEX IF EXISTS ix_koth_epoch_hill_rollups_latest;

                ALTER TABLE "KothEpochRollups"
                  ADD COLUMN IF NOT EXISTS formula_version SMALLINT NOT NULL DEFAULT 2,
                  ALTER COLUMN formula_version SET DEFAULT 2,
                  ALTER COLUMN formula_version SET NOT NULL,
                  DROP CONSTRAINT IF EXISTS "KothEpochRollups_pkey",
                  DROP CONSTRAINT IF EXISTS ux_koth_epoch_rollups_compat,
                  DROP CONSTRAINT IF EXISTS ck_koth_epoch_rollups_identity,
                  ADD CONSTRAINT "KothEpochRollups_pkey" PRIMARY KEY (game_id, epoch),
                  ADD CONSTRAINT ux_koth_epoch_rollups_compat
                    UNIQUE (game_id, formula_version, epoch),
                  ADD CONSTRAINT ck_koth_epoch_rollups_identity
                    CHECK (formula_version = 2 AND epoch >= 1);

                ALTER TABLE "KothEpochTeamRollups"
                  ADD COLUMN IF NOT EXISTS formula_version SMALLINT NOT NULL DEFAULT 2,
                  ALTER COLUMN formula_version SET DEFAULT 2,
                  ALTER COLUMN formula_version SET NOT NULL,
                  ADD CONSTRAINT "KothEpochTeamRollups_pkey"
                    PRIMARY KEY (game_id, epoch, participation_id),
                  ADD CONSTRAINT ux_koth_epoch_team_rollups_compat
                    UNIQUE (game_id, formula_version, epoch, participation_id),
                  ADD CONSTRAINT "KothEpochTeamRollups_game_id_epoch_fkey"
                    FOREIGN KEY (game_id, epoch)
                    REFERENCES "KothEpochRollups"(game_id, epoch) ON DELETE CASCADE,
                  ADD CONSTRAINT
                    "KothEpochTeamRollups_game_id_formula_version_epoch_fkey"
                    FOREIGN KEY (game_id, formula_version, epoch)
                    REFERENCES "KothEpochRollups"(game_id, formula_version, epoch)
                    ON DELETE CASCADE,
                  ADD CONSTRAINT ck_koth_epoch_team_rollups_constant_formula
                    CHECK (formula_version = 2);
                CREATE INDEX IF NOT EXISTS ix_koth_epoch_team_rollups_latest
                  ON "KothEpochTeamRollups"(game_id, participation_id, epoch DESC);

                ALTER TABLE "KothEpochHillRollups"
                  ADD COLUMN IF NOT EXISTS formula_version SMALLINT NOT NULL DEFAULT 2,
                  ALTER COLUMN formula_version SET DEFAULT 2,
                  ALTER COLUMN formula_version SET NOT NULL,
                  ADD CONSTRAINT "KothEpochHillRollups_pkey"
                    PRIMARY KEY (game_id, epoch, participation_id, challenge_id),
                  ADD CONSTRAINT ux_koth_epoch_hill_rollups_compat
                    UNIQUE (game_id, formula_version, epoch, participation_id, challenge_id),
                  ADD CONSTRAINT "KothEpochHillRollups_game_id_epoch_fkey"
                    FOREIGN KEY (game_id, epoch)
                    REFERENCES "KothEpochRollups"(game_id, epoch) ON DELETE CASCADE,
                  ADD CONSTRAINT
                    "KothEpochHillRollups_game_id_formula_version_epoch_fkey"
                    FOREIGN KEY (game_id, formula_version, epoch)
                    REFERENCES "KothEpochRollups"(game_id, formula_version, epoch)
                    ON DELETE CASCADE,
                  ADD CONSTRAINT ck_koth_epoch_hill_rollups_constant_formula
                    CHECK (formula_version = 2);
                CREATE INDEX IF NOT EXISTS ix_koth_epoch_hill_rollups_latest
                  ON "KothEpochHillRollups"
                    (game_id, participation_id, challenge_id, epoch DESC);
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE "Games"
                  DROP CONSTRAINT IF EXISTS ck_games_koth_scoring_formula_version,
                  ADD CONSTRAINT ck_games_koth_scoring_formula_version
                    CHECK (koth_scoring_formula_version >= 1);

                ALTER TABLE "KothCrownCycles"
                  DROP CONSTRAINT IF EXISTS fk_koth_crown_cycles_config,
                  DROP CONSTRAINT IF EXISTS fk_koth_crown_cycles_config_compat,
                  DROP CONSTRAINT IF EXISTS ux_koth_crown_cycles_identity,
                  DROP CONSTRAINT IF EXISTS ux_koth_crown_cycles_identity_compat,
                  DROP CONSTRAINT IF EXISTS ck_koth_crown_cycles_identity;
                ALTER TABLE "KothOfficialConfigs"
                  DROP CONSTRAINT IF EXISTS ux_koth_official_configs_game_formula,
                  DROP CONSTRAINT IF EXISTS ck_koth_official_configs_version,
                  ADD CONSTRAINT ux_koth_official_configs_game_formula
                    UNIQUE (game_id, formula_version),
                  ADD CONSTRAINT ck_koth_official_configs_version
                    CHECK (formula_version >= 2);
                ALTER TABLE "KothCrownCycles"
                  ADD CONSTRAINT ux_koth_crown_cycles_identity
                    UNIQUE (game_id, challenge_id, formula_version, cycle_number),
                  ADD CONSTRAINT fk_koth_crown_cycles_config
                    FOREIGN KEY (game_id, formula_version)
                    REFERENCES "KothOfficialConfigs"(game_id, formula_version)
                    ON DELETE CASCADE,
                  ADD CONSTRAINT ck_koth_crown_cycles_identity
                    CHECK (formula_version >= 2 AND cycle_number >= 1 AND epoch >= 1);

                ALTER TABLE "KothEpochTeamRollups"
                  DROP CONSTRAINT IF EXISTS "KothEpochTeamRollups_game_id_epoch_fkey",
                  DROP CONSTRAINT IF EXISTS
                    "KothEpochTeamRollups_game_id_formula_version_epoch_fkey",
                  DROP CONSTRAINT IF EXISTS "KothEpochTeamRollups_pkey",
                  DROP CONSTRAINT IF EXISTS ux_koth_epoch_team_rollups_compat,
                  DROP CONSTRAINT IF EXISTS ck_koth_epoch_team_rollups_constant_formula;
                ALTER TABLE "KothEpochHillRollups"
                  DROP CONSTRAINT IF EXISTS "KothEpochHillRollups_game_id_epoch_fkey",
                  DROP CONSTRAINT IF EXISTS
                    "KothEpochHillRollups_game_id_formula_version_epoch_fkey",
                  DROP CONSTRAINT IF EXISTS "KothEpochHillRollups_pkey",
                  DROP CONSTRAINT IF EXISTS ux_koth_epoch_hill_rollups_compat,
                  DROP CONSTRAINT IF EXISTS ck_koth_epoch_hill_rollups_constant_formula;
                DROP INDEX IF EXISTS ix_koth_epoch_team_rollups_latest;
                DROP INDEX IF EXISTS ix_koth_epoch_hill_rollups_latest;
                ALTER TABLE "KothEpochRollups"
                  DROP CONSTRAINT IF EXISTS "KothEpochRollups_pkey",
                  DROP CONSTRAINT IF EXISTS ux_koth_epoch_rollups_compat,
                  DROP CONSTRAINT IF EXISTS ck_koth_epoch_rollups_identity,
                  ADD CONSTRAINT "KothEpochRollups_pkey"
                    PRIMARY KEY (game_id, formula_version, epoch),
                  ADD CONSTRAINT ck_koth_epoch_rollups_identity
                    CHECK (formula_version >= 1 AND epoch >= 1);
                ALTER TABLE "KothEpochTeamRollups"
                  ADD CONSTRAINT "KothEpochTeamRollups_pkey"
                    PRIMARY KEY (game_id, formula_version, epoch, participation_id),
                  ADD CONSTRAINT
                    "KothEpochTeamRollups_game_id_formula_version_epoch_fkey"
                    FOREIGN KEY (game_id, formula_version, epoch)
                    REFERENCES "KothEpochRollups"(game_id, formula_version, epoch)
                    ON DELETE CASCADE;
                CREATE INDEX IF NOT EXISTS ix_koth_epoch_team_rollups_latest
                  ON "KothEpochTeamRollups"
                    (game_id, formula_version, participation_id, epoch DESC);
                ALTER TABLE "KothEpochHillRollups"
                  ADD CONSTRAINT "KothEpochHillRollups_pkey"
                    PRIMARY KEY (
                      game_id, formula_version, epoch, participation_id, challenge_id
                    ),
                  ADD CONSTRAINT
                    "KothEpochHillRollups_game_id_formula_version_epoch_fkey"
                    FOREIGN KEY (game_id, formula_version, epoch)
                    REFERENCES "KothEpochRollups"(game_id, formula_version, epoch)
                    ON DELETE CASCADE;
                CREATE INDEX IF NOT EXISTS ix_koth_epoch_hill_rollups_latest
                  ON "KothEpochHillRollups"
                    (game_id, formula_version, participation_id, challenge_id, epoch DESC);
                "#,
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sea_orm_migration::sea_orm::Database;

    use super::Migration;
    use crate::migrations::{MigrationTrait, Migrator, MigratorTrait, SchemaManager};

    #[tokio::test]
    #[ignore = "requires a disposable PostgreSQL database via RSCTF_TEST_DATABASE_URL"]
    async fn fresh_schema_has_one_constant_koth_formula_and_rolling_conflict_targets() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
        let db = Database::connect(database_url).await.unwrap();
        Migrator::fresh(&db).await.unwrap();
        let pool = db.get_postgres_connection_pool();
        let manager = SchemaManager::new(&db);

        let constant_columns = sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*)
                 FROM information_schema.columns
                WHERE table_schema = 'public'
                  AND is_nullable = 'NO'
                  AND column_default LIKE '2%'
                  AND (
                    (table_name = 'Games' AND column_name = 'koth_scoring_formula_version')
                    OR (table_name IN (
                      'KothOfficialConfigs', 'KothCrownCycles', 'KothEpochRollups',
                      'KothEpochTeamRollups', 'KothEpochHillRollups'
                    ) AND column_name = 'formula_version')
                  )"#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(constant_columns, 6);

        let constant_constraints = sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*)
                 FROM pg_constraint
                WHERE conname IN (
                  'ck_games_koth_scoring_formula_version',
                  'ck_koth_official_configs_version',
                  'ck_koth_crown_cycles_identity',
                  'ck_koth_epoch_rollups_identity',
                  'ck_koth_epoch_team_rollups_constant_formula',
                  'ck_koth_epoch_hill_rollups_constant_formula'
                )
                  AND pg_get_constraintdef(oid) ~ '= 2'"#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(constant_constraints, 6);
        Migration.up(&manager).await.unwrap();

        // Exercise the real upgrade boundary with both historical and current
        // rollups. The historical row was already invisible to the runtime;
        // the current row and official snapshot must survive the constant-only
        // expand phase.
        Migration.down(&manager).await.unwrap();
        sqlx::query(
            r#"INSERT INTO "Games" (
                 id, title, public_key, private_key, hidden, practice_mode,
                 summary, content, accept_without_review, allow_user_submissions,
                 writeup_required, team_member_count_limit, container_count_limit,
                 start_time_utc, end_time_utc, writeup_deadline, writeup_note,
                 blood_bonus_value, ad_allow_snapshot_download, ad_scoring_paused,
                 koth_epoch_ticks, koth_cycle_ticks,
                 koth_champion_cooldown_ticks, koth_claim_confirmation_ticks
               ) VALUES (
                 900001, 'constant KotH migration', 'public', 'private', FALSE, FALSE,
                 '', '', FALSE, FALSE, FALSE, 1, 1,
                 now() - interval '1 hour', now() + interval '1 hour',
                 now() + interval '2 hours', '', 0, FALSE, FALSE, 12, 3, 1, 2
               )"#,
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "KothOfficialConfigs" (
                 game_id, formula_version, scoring_start_round, epoch_ticks,
                 cycle_ticks, champion_cooldown_ticks, claim_confirmation_ticks,
                 roster_snapshot, hills_snapshot
               ) VALUES (900001, 2, 1, 12, 3, 1, 2, '[]', '[]')"#,
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "KothEpochRollups" (
                 game_id, formula_version, epoch, start_round, end_round,
                 round_count, epoch_weight, finalized_round, evidence_finalized_at,
                 scorable_ticks, eligible_windows, cumulative_scorable_ticks,
                 cumulative_eligible_windows
               ) VALUES
                 (900001, 1, 1, 1, 8, 8, 1, 8, now(), 8, 2, 8, 2),
                 (900001, 2, 1, 10, 21, 12, 1, 21, now(), 12, 4, 12, 4)"#,
        )
        .execute(pool)
        .await
        .unwrap();

        Migration.up(&manager).await.unwrap();
        let surviving_rollup = sqlx::query_as::<_, (i64, i32)>(
            r#"SELECT COUNT(*), MIN(start_round)
                 FROM "KothEpochRollups" WHERE game_id = 900001"#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(surviving_rollup, (1, 10));
        let official_configs = sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*) FROM "KothOfficialConfigs" WHERE game_id = 900001"#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(official_configs, 1);

        // Both the pre-upgrade and versionless runtimes must be able to infer
        // every ON CONFLICT target while old processes drain. WHERE FALSE keeps
        // this a pure schema-compatibility check without fabricating evidence.
        sqlx::raw_sql(
            r#"
            INSERT INTO "KothOfficialConfigs" (game_id)
              SELECT 0 WHERE FALSE ON CONFLICT (game_id) DO NOTHING;
            INSERT INTO "KothOfficialConfigs" (game_id, formula_version)
              SELECT 0,2 WHERE FALSE
              ON CONFLICT (game_id, formula_version) DO NOTHING;
            INSERT INTO "KothCrownCycles"
              (game_id, challenge_id, cycle_number)
              SELECT 0,0,0 WHERE FALSE
              ON CONFLICT (game_id, challenge_id, cycle_number) DO NOTHING;
            INSERT INTO "KothCrownCycles"
              (game_id, challenge_id, formula_version, cycle_number)
              SELECT 0,0,2,0 WHERE FALSE
              ON CONFLICT (game_id, challenge_id, formula_version, cycle_number)
              DO NOTHING;
            INSERT INTO "KothEpochRollups" (game_id, epoch)
              SELECT 0,0 WHERE FALSE
              ON CONFLICT (game_id, epoch) DO NOTHING;
            INSERT INTO "KothEpochRollups" (game_id, formula_version, epoch)
              SELECT 0,2,0 WHERE FALSE
              ON CONFLICT (game_id, formula_version, epoch) DO NOTHING;
            INSERT INTO "KothEpochTeamRollups"
              (game_id, epoch, participation_id)
              SELECT 0,0,0 WHERE FALSE
              ON CONFLICT (game_id, epoch, participation_id) DO NOTHING;
            INSERT INTO "KothEpochTeamRollups"
              (game_id, formula_version, epoch, participation_id)
              SELECT 0,2,0,0 WHERE FALSE
              ON CONFLICT (game_id, formula_version, epoch, participation_id)
              DO NOTHING;
            INSERT INTO "KothEpochHillRollups"
              (game_id, epoch, participation_id, challenge_id)
              SELECT 0,0,0,0 WHERE FALSE
              ON CONFLICT (game_id, epoch, participation_id, challenge_id)
              DO NOTHING;
            INSERT INTO "KothEpochHillRollups"
              (game_id, formula_version, epoch, participation_id, challenge_id)
              SELECT 0,2,0,0,0 WHERE FALSE
              ON CONFLICT
                (game_id, formula_version, epoch, participation_id, challenge_id)
              DO NOTHING;
            "#,
        )
        .execute(pool)
        .await
        .unwrap();
    }
}
