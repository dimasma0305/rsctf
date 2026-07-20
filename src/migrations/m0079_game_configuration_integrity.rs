//! Normalize legacy event configuration and enforce the editor's bounds in PostgreSQL.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
LOCK TABLE "Games" IN SHARE ROW EXCLUSIVE MODE;

-- Convert every non-finite schedule into an explicitly closed or dormant
-- finite window. Never let migration turn an ambiguous legacy sentinel into
-- a currently active event: negative infinity closes, positive infinity
-- defers, and a -infinity end takes precedence over a +infinity start.
-- Normalize a -infinity end first, replacing both fields so this closed-end
-- rule wins even when the original start was +infinity.
UPDATE "Games"
   SET start_time_utc = TIMESTAMPTZ '2000-01-01 00:00:00+00',
       end_time_utc = TIMESTAMPTZ '2000-01-01 00:00:01+00'
 WHERE end_time_utc = '-infinity'::TIMESTAMPTZ;

UPDATE "Games"
   SET start_time_utc = TIMESTAMPTZ '9999-12-31 23:59:58+00',
       end_time_utc = TIMESTAMPTZ '9999-12-31 23:59:59+00'
 WHERE start_time_utc = 'infinity'::TIMESTAMPTZ;

UPDATE "Games"
   SET start_time_utc = TIMESTAMPTZ '2000-01-01 00:00:00+00',
       end_time_utc = TIMESTAMPTZ '2000-01-01 00:00:01+00'
 WHERE start_time_utc = '-infinity'::TIMESTAMPTZ;

UPDATE "Games"
   SET start_time_utc = TIMESTAMPTZ '9999-12-31 23:59:58+00',
       end_time_utc = TIMESTAMPTZ '9999-12-31 23:59:59+00'
 WHERE end_time_utc = 'infinity'::TIMESTAMPTZ;

UPDATE "Games"
   SET end_time_utc = start_time_utc + INTERVAL '1 second'
 WHERE end_time_utc <= start_time_utc;

UPDATE "Games"
   SET freeze_time_utc = NULL
 WHERE freeze_time_utc IS NOT NULL
   AND (NOT isfinite(freeze_time_utc)
        OR freeze_time_utc <= start_time_utc
        OR freeze_time_utc >= end_time_utc);

UPDATE "Games" SET team_member_count_limit = GREATEST(0, team_member_count_limit)
 WHERE team_member_count_limit < 0;
UPDATE "Games" SET container_count_limit = GREATEST(0, container_count_limit)
 WHERE container_count_limit < 0;
UPDATE "Games" SET ad_warmup_seconds = LEAST(86400, GREATEST(0, ad_warmup_seconds))
 WHERE ad_warmup_seconds IS NOT NULL
   AND ad_warmup_seconds NOT BETWEEN 0 AND 86400;
UPDATE "Games" SET ad_snapshot_retention_days = LEAST(3650, GREATEST(1, ad_snapshot_retention_days))
 WHERE ad_snapshot_retention_days IS NOT NULL
   AND ad_snapshot_retention_days NOT BETWEEN 1 AND 3650;
UPDATE "Games" SET ad_tick_seconds = LEAST(600, GREATEST(30, ad_tick_seconds))
 WHERE ad_tick_seconds IS NOT NULL
   AND ad_tick_seconds NOT BETWEEN 30 AND 600;
UPDATE "Games" SET ad_reset_cooldown_minutes = LEAST(60, GREATEST(0, ad_reset_cooldown_minutes))
 WHERE ad_reset_cooldown_minutes IS NOT NULL
   AND ad_reset_cooldown_minutes NOT BETWEEN 0 AND 60;
UPDATE "Games" SET ad_getflag_window_fraction = 0.5
 WHERE ad_getflag_window_fraction IS NOT NULL
   AND NOT (ad_getflag_window_fraction >= 0.05
            AND ad_getflag_window_fraction <= 0.9);
UPDATE "Games" SET ad_min_grace_period_seconds = LEAST(60, GREATEST(1, ad_min_grace_period_seconds))
 WHERE ad_min_grace_period_seconds IS NOT NULL
   AND ad_min_grace_period_seconds NOT BETWEEN 1 AND 60;
UPDATE "Games"
   SET ad_min_grace_period_seconds = GREATEST(1, COALESCE(ad_tick_seconds, 60) - 12)
 WHERE ad_min_grace_period_seconds IS NOT NULL
   AND ad_min_grace_period_seconds > GREATEST(1, COALESCE(ad_tick_seconds, 60) - 12);

DO $migration$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_games_event_times_finite' AND conrelid = '"Games"'::regclass) THEN
    ALTER TABLE "Games" ADD CONSTRAINT ck_games_event_times_finite
      CHECK (isfinite(start_time_utc) AND isfinite(end_time_utc));
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_games_event_window' AND conrelid = '"Games"'::regclass) THEN
    ALTER TABLE "Games" ADD CONSTRAINT ck_games_event_window
      CHECK (end_time_utc > start_time_utc);
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_games_freeze_window' AND conrelid = '"Games"'::regclass) THEN
    ALTER TABLE "Games" ADD CONSTRAINT ck_games_freeze_window
      CHECK (freeze_time_utc IS NULL OR
             (isfinite(freeze_time_utc) AND freeze_time_utc > start_time_utc AND freeze_time_utc < end_time_utc));
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_games_nonnegative_limits' AND conrelid = '"Games"'::regclass) THEN
    ALTER TABLE "Games" ADD CONSTRAINT ck_games_nonnegative_limits
      CHECK (team_member_count_limit >= 0 AND container_count_limit >= 0);
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_games_ad_warmup_seconds' AND conrelid = '"Games"'::regclass) THEN
    ALTER TABLE "Games" ADD CONSTRAINT ck_games_ad_warmup_seconds
      CHECK (ad_warmup_seconds IS NULL OR ad_warmup_seconds BETWEEN 0 AND 86400);
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_games_ad_snapshot_retention_days' AND conrelid = '"Games"'::regclass) THEN
    ALTER TABLE "Games" ADD CONSTRAINT ck_games_ad_snapshot_retention_days
      CHECK (ad_snapshot_retention_days IS NULL OR ad_snapshot_retention_days BETWEEN 1 AND 3650);
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_games_ad_tick_seconds' AND conrelid = '"Games"'::regclass) THEN
    ALTER TABLE "Games" ADD CONSTRAINT ck_games_ad_tick_seconds
      CHECK (ad_tick_seconds IS NULL OR ad_tick_seconds BETWEEN 30 AND 600);
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_games_ad_reset_cooldown_minutes' AND conrelid = '"Games"'::regclass) THEN
    ALTER TABLE "Games" ADD CONSTRAINT ck_games_ad_reset_cooldown_minutes
      CHECK (ad_reset_cooldown_minutes IS NULL OR ad_reset_cooldown_minutes BETWEEN 0 AND 60);
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_games_ad_getflag_window_fraction' AND conrelid = '"Games"'::regclass) THEN
    ALTER TABLE "Games" ADD CONSTRAINT ck_games_ad_getflag_window_fraction
      CHECK (ad_getflag_window_fraction IS NULL OR
             (ad_getflag_window_fraction >= 0.05 AND ad_getflag_window_fraction <= 0.9));
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_games_ad_min_grace_period_seconds' AND conrelid = '"Games"'::regclass) THEN
    ALTER TABLE "Games" ADD CONSTRAINT ck_games_ad_min_grace_period_seconds
      CHECK (ad_min_grace_period_seconds IS NULL OR
             (ad_min_grace_period_seconds BETWEEN 1 AND 60
              AND ad_min_grace_period_seconds <=
                  GREATEST(1, COALESCE(ad_tick_seconds, 60) - 12)));
  END IF;
END
$migration$;
"#;

const CONSTRAINTS: &[&str] = &[
    "ck_games_event_times_finite",
    "ck_games_event_window",
    "ck_games_freeze_window",
    "ck_games_nonnegative_limits",
    "ck_games_ad_warmup_seconds",
    "ck_games_ad_snapshot_retention_days",
    "ck_games_ad_tick_seconds",
    "ck_games_ad_reset_cooldown_minutes",
    "ck_games_ad_getflag_window_fraction",
    "ck_games_ad_min_grace_period_seconds",
];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let drops = CONSTRAINTS
            .iter()
            .map(|name| format!(r#"DROP CONSTRAINT IF EXISTS {name}"#))
            .collect::<Vec<_>>()
            .join(", ");
        manager
            .get_connection()
            .execute_unprepared(&format!(r#"ALTER TABLE "Games" {drops};"#))
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{CONSTRAINTS, UP_SQL};
    use chrono::{DateTime, Utc};
    use sqlx::postgres::PgPoolOptions;

    #[test]
    fn normalizes_legacy_values_before_adding_every_constraint() {
        let final_update = UP_SQL.rfind("UPDATE \"Games\"").unwrap();
        let constraints = UP_SQL.find("DO $migration$").unwrap();
        assert!(final_update < constraints);
        let dormant_start = UP_SQL
            .find("WHERE start_time_utc = 'infinity'::TIMESTAMPTZ")
            .unwrap();
        let closed_end = UP_SQL
            .find("WHERE end_time_utc = '-infinity'::TIMESTAMPTZ")
            .unwrap();
        let past_start = UP_SQL
            .find("WHERE start_time_utc = '-infinity'::TIMESTAMPTZ")
            .unwrap();
        let future_end = UP_SQL
            .find("WHERE end_time_utc = 'infinity'::TIMESTAMPTZ")
            .unwrap();
        let invalid_window = UP_SQL.find("WHERE end_time_utc <= start_time_utc").unwrap();
        assert!(closed_end < dormant_start, "closed-end precedence changed");
        assert!(dormant_start < past_start);
        assert!(past_start < future_end);
        assert!(future_end < invalid_window);
        assert!(UP_SQL.contains("TIMESTAMPTZ '2000-01-01 00:00:00+00'"));
        assert!(UP_SQL.contains("TIMESTAMPTZ '2000-01-01 00:00:01+00'"));
        assert!(UP_SQL.contains("TIMESTAMPTZ '9999-12-31 23:59:58+00'"));
        assert!(UP_SQL.contains("TIMESTAMPTZ '9999-12-31 23:59:59+00'"));
        assert!(UP_SQL.contains("end_time_utc = start_time_utc + INTERVAL '1 second'"));
        assert!(UP_SQL.contains("NOT (ad_getflag_window_fraction >= 0.05"));
        assert!(UP_SQL.contains("COALESCE(ad_tick_seconds, 60) - 12"));
        for constraint in CONSTRAINTS {
            assert!(UP_SQL.contains(constraint), "missing {constraint}");
        }
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn signed_event_infinities_fail_closed_and_are_idempotent() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("game_config_{}", uuid::Uuid::new_v4().simple());
        let setup = format!(
            r#"
            CREATE SCHEMA "{schema}";
            CREATE TABLE "{schema}"."Games" (
                id INTEGER PRIMARY KEY,
                start_time_utc TIMESTAMPTZ NOT NULL,
                end_time_utc TIMESTAMPTZ NOT NULL,
                freeze_time_utc TIMESTAMPTZ,
                team_member_count_limit INTEGER NOT NULL DEFAULT 0,
                container_count_limit INTEGER NOT NULL DEFAULT 0,
                ad_warmup_seconds INTEGER,
                ad_snapshot_retention_days INTEGER,
                ad_tick_seconds INTEGER,
                ad_reset_cooldown_minutes INTEGER,
                ad_getflag_window_fraction DOUBLE PRECISION,
                ad_min_grace_period_seconds INTEGER
            );
            INSERT INTO "{schema}"."Games" (id, start_time_utc, end_time_utc)
            VALUES
                (1, '-infinity', '2040-01-01T00:00:00Z'),
                (2,  'infinity', '2040-01-01T00:00:00Z'),
                (3, '2020-01-01T00:00:00Z', '-infinity'),
                (4, '2020-01-01T00:00:00Z',  'infinity'),
                (5,  'infinity', '-infinity'),
                (6, '-infinity',  'infinity'),
                (7,  'infinity',  'infinity'),
                (8, '-infinity', '-infinity'),
                (9, '2030-01-01T00:00:00Z', '2020-01-01T00:00:00Z'),
                (10, '2020-01-01T00:00:00Z', '2030-01-01T00:00:00Z');
            "#
        );
        sqlx::raw_sql(&setup).execute(&admin).await.unwrap();

        let search_path_schema = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(2)
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
        let first = sqlx::query_as::<_, (i32, DateTime<Utc>, DateTime<Utc>)>(
            r#"SELECT id, start_time_utc, end_time_utc FROM "Games" ORDER BY id"#,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        let second = sqlx::query_as::<_, (i32, DateTime<Utc>, DateTime<Utc>)>(
            r#"SELECT id, start_time_utc, end_time_utc FROM "Games" ORDER BY id"#,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(first, second, "normalization must be idempotent");

        let at = |value: &str| {
            DateTime::parse_from_rfc3339(value)
                .unwrap()
                .with_timezone(&Utc)
        };
        let past_start = at("2000-01-01T00:00:00Z");
        let past_end = at("2000-01-01T00:00:01Z");
        let future_start = at("9999-12-31T23:59:58Z");
        let future_end = at("9999-12-31T23:59:59Z");
        let expected = vec![
            (1, past_start, past_end),
            (2, future_start, future_end),
            (3, past_start, past_end),
            (4, future_start, future_end),
            (5, past_start, past_end),
            (6, past_start, past_end),
            (7, future_start, future_end),
            (8, past_start, past_end),
            (9, at("2030-01-01T00:00:00Z"), at("2030-01-01T00:00:01Z")),
            (10, at("2020-01-01T00:00:00Z"), at("2030-01-01T00:00:00Z")),
        ];
        assert_eq!(first, expected);

        let unsafe_windows = sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*)
                 FROM "Games"
                WHERE id BETWEEN 1 AND 8
                  AND start_time_utc <= statement_timestamp()
                  AND end_time_utc > statement_timestamp()"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(unsafe_windows, 0, "closed or dormant events were reopened");

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
