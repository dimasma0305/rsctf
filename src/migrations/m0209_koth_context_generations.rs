//! Durable invalidation generations for externally cached KotH observer context.

use sea_orm_migration::prelude::*;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "KothObserverContextGenerations" (
    game_id INTEGER NOT NULL REFERENCES "Games"(id) ON DELETE CASCADE,
    challenge_id INTEGER NOT NULL REFERENCES "GameChallenges"(id) ON DELETE CASCADE,
    generation BIGINT NOT NULL DEFAULT 1 CHECK (generation >= 1),
    PRIMARY KEY (game_id, challenge_id)
);

INSERT INTO "KothObserverContextGenerations" (game_id, challenge_id, generation)
SELECT game_id, challenge_id, 1 FROM "KothApiObservers"
ON CONFLICT (game_id, challenge_id) DO NOTHING;

CREATE OR REPLACE FUNCTION bump_koth_context_pair(target_game INTEGER, target_challenge INTEGER)
RETURNS VOID LANGUAGE SQL AS $$
  INSERT INTO "KothObserverContextGenerations" (game_id, challenge_id, generation)
  VALUES (target_game, target_challenge, 2)
  ON CONFLICT (game_id, challenge_id) DO UPDATE
     SET generation = "KothObserverContextGenerations".generation + 1;
$$;

CREATE OR REPLACE FUNCTION bump_koth_context_from_pair()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  PERFORM bump_koth_context_pair(
    COALESCE(NEW.game_id, OLD.game_id), COALESCE(NEW.challenge_id, OLD.challenge_id)
  );
  RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE OR REPLACE FUNCTION bump_koth_context_from_game()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE event_id INTEGER := COALESCE(NEW.game_id, OLD.game_id);
BEGIN
  UPDATE "KothObserverContextGenerations"
     SET generation = generation + 1
   WHERE game_id = event_id;
  RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE OR REPLACE FUNCTION bump_koth_context_from_team_member()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE changed_team INTEGER := COALESCE(NEW.team_id, OLD.team_id);
BEGIN
  UPDATE "KothObserverContextGenerations" generation
     SET generation = generation.generation + 1
   WHERE generation.game_id IN (
     SELECT participation.game_id FROM "Participations" participation
     WHERE participation.team_id = changed_team
   );
  IF TG_OP = 'UPDATE' AND NEW.team_id IS DISTINCT FROM OLD.team_id THEN
    UPDATE "KothObserverContextGenerations" generation
       SET generation = generation.generation + 1
     WHERE generation.game_id IN (
       SELECT participation.game_id FROM "Participations" participation
        WHERE participation.team_id = OLD.team_id
     );
  END IF;
  RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE OR REPLACE FUNCTION bump_koth_context_from_team()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE changed_team INTEGER := COALESCE(NEW.id, OLD.id);
BEGIN
  UPDATE "KothObserverContextGenerations" generation
     SET generation = generation.generation + 1
   WHERE generation.game_id IN (
     SELECT participation.game_id FROM "Participations" participation
      WHERE participation.team_id = changed_team
   );
  RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE OR REPLACE FUNCTION bump_koth_context_from_account()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  UPDATE "KothObserverContextGenerations" generation
     SET generation = generation.generation + 1
   WHERE generation.game_id IN (
     SELECT participation.game_id
       FROM "Participations" participation
       JOIN "Teams" team ON team.id = participation.team_id
      WHERE team.captain_id = COALESCE(NEW.id, OLD.id)
         OR EXISTS (
              SELECT 1 FROM "TeamMembers" member
               WHERE member.team_id = team.id
                 AND member.user_id = COALESCE(NEW.id, OLD.id)
            )
   );
  RETURN COALESCE(NEW, OLD);
END;
$$;

DO $$
DECLARE table_name TEXT;
BEGIN
  FOREACH table_name IN ARRAY ARRAY[
    'KothApiObservers', 'KothApiTeamTokens', 'KothTargets',
    'KothCrownCycles', 'KothApiArenaSchemes'
  ] LOOP
    EXECUTE format('DROP TRIGGER IF EXISTS tr_koth_context_generation ON %I', table_name);
    EXECUTE format(
      'CREATE TRIGGER tr_koth_context_generation AFTER INSERT OR UPDATE OR DELETE ON %I FOR EACH ROW EXECUTE FUNCTION bump_koth_context_from_pair()',
      table_name
    );
  END LOOP;
END $$;

DO $$
DECLARE table_name TEXT;
BEGIN
  FOREACH table_name IN ARRAY ARRAY['AdRounds', 'KothOfficialConfigs', 'Participations'] LOOP
    EXECUTE format('DROP TRIGGER IF EXISTS tr_koth_context_generation ON %I', table_name);
    EXECUTE format(
      'CREATE TRIGGER tr_koth_context_generation AFTER INSERT OR UPDATE OR DELETE ON %I FOR EACH ROW EXECUTE FUNCTION bump_koth_context_from_game()',
      table_name
    );
  END LOOP;
END $$;

DROP TRIGGER IF EXISTS tr_koth_context_generation ON "TeamMembers";
CREATE TRIGGER tr_koth_context_generation AFTER INSERT OR UPDATE OR DELETE ON "TeamMembers"
FOR EACH ROW EXECUTE FUNCTION bump_koth_context_from_team_member();

DROP TRIGGER IF EXISTS tr_koth_context_generation ON "Teams";
CREATE TRIGGER tr_koth_context_generation AFTER UPDATE OR DELETE ON "Teams"
FOR EACH ROW EXECUTE FUNCTION bump_koth_context_from_team();

DROP TRIGGER IF EXISTS tr_koth_context_generation ON "AspNetUsers";
DROP TRIGGER IF EXISTS tr_koth_context_generation_account_role ON "AspNetUsers";
CREATE TRIGGER tr_koth_context_generation_account_role AFTER UPDATE OF role ON "AspNetUsers"
FOR EACH ROW WHEN (NEW.role IS DISTINCT FROM OLD.role)
EXECUTE FUNCTION bump_koth_context_from_account();
DROP TRIGGER IF EXISTS tr_koth_context_generation_account_delete ON "AspNetUsers";
CREATE TRIGGER tr_koth_context_generation_account_delete AFTER DELETE ON "AspNetUsers"
FOR EACH ROW EXECUTE FUNCTION bump_koth_context_from_account();
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sea_orm::SqlxPostgresConnector;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::Migration;
    use super::UP_SQL;
    use sea_orm_migration::{MigrationTrait, SchemaManager};

    #[test]
    fn context_generation_covers_context_roster_and_round_inputs() {
        for table in [
            "KothApiTeamTokens",
            "KothTargets",
            "KothCrownCycles",
            "AdRounds",
            "Participations",
            "TeamMembers",
            "AspNetUsers",
        ] {
            assert!(
                UP_SQL.contains(table),
                "missing invalidation source {table}"
            );
        }
        assert!(UP_SQL.contains("generation = generation.generation + 1"));
        assert!(UP_SQL.contains(
            "tr_koth_context_generation_account_role AFTER UPDATE OF role ON \"AspNetUsers\""
        ));
        assert!(UP_SQL.contains("WHEN (NEW.role IS DISTINCT FROM OLD.role)"));
        assert!(UP_SQL
            .contains("tr_koth_context_generation_account_delete AFTER DELETE ON \"AspNetUsers\""));
        assert!(!UP_SQL.contains("AFTER UPDATE OR DELETE ON \"AspNetUsers\""));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn account_activity_does_not_invalidate_observer_context() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_m0209_{}", uuid::Uuid::new_v4().simple());
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
            r#"CREATE TABLE "Games" (id INTEGER PRIMARY KEY);
               CREATE TABLE "GameChallenges" (id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL);
               CREATE TABLE "KothApiObservers" (game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL);
               CREATE TABLE "KothApiTeamTokens" (game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL);
               CREATE TABLE "KothTargets" (game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL);
               CREATE TABLE "KothCrownCycles" (game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL);
               CREATE TABLE "KothApiArenaSchemes" (game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL);
               CREATE TABLE "AdRounds" (game_id INTEGER NOT NULL);
               CREATE TABLE "KothOfficialConfigs" (game_id INTEGER NOT NULL);
               CREATE TABLE "Participations" (game_id INTEGER NOT NULL, team_id INTEGER NOT NULL);
               CREATE TABLE "Teams" (id INTEGER PRIMARY KEY, captain_id UUID);
               CREATE TABLE "TeamMembers" (team_id INTEGER NOT NULL, user_id UUID NOT NULL);
               CREATE TABLE "AspNetUsers" (
                 id UUID PRIMARY KEY, role SMALLINT NOT NULL,
                 last_visited_utc TIMESTAMPTZ NOT NULL
               );
               INSERT INTO "Games" VALUES (7);
               INSERT INTO "GameChallenges" VALUES (9, 7);
               INSERT INTO "KothApiObservers" VALUES (7, 9);
               INSERT INTO "AspNetUsers" VALUES
                 ('00000000-0000-4000-8000-000000000209', 1, clock_timestamp());
               INSERT INTO "Teams" VALUES
                 (21, '00000000-0000-4000-8000-000000000209');
               INSERT INTO "Participations" VALUES (7, 21);"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let db = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await.unwrap();
        Migration.up(&manager).await.unwrap();

        let generation = || async {
            sqlx::query_scalar::<_, i64>(
                r#"SELECT generation FROM "KothObserverContextGenerations"
                    WHERE game_id = 7 AND challenge_id = 9"#,
            )
            .fetch_one(&pool)
            .await
            .unwrap()
        };
        assert_eq!(generation().await, 1);
        sqlx::query(
            r#"UPDATE "AspNetUsers"
                  SET last_visited_utc = clock_timestamp(), role = role
                WHERE id = '00000000-0000-4000-8000-000000000209'"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(generation().await, 1);
        sqlx::query(
            r#"UPDATE "AspNetUsers" SET role = 2
                WHERE id = '00000000-0000-4000-8000-000000000209'"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(generation().await, 2);
        sqlx::query(
            r#"DELETE FROM "AspNetUsers"
                WHERE id = '00000000-0000-4000-8000-000000000209'"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(generation().await, 3);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
