//! Durable cross-replica generations for player catalog and live-detail projections.

use sea_orm_migration::prelude::*;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "ParticipantDetailGenerations" (
    game_id INTEGER PRIMARY KEY REFERENCES "Games"(id) ON DELETE CASCADE,
    generation BIGINT NOT NULL DEFAULT 1 CHECK (generation >= 1)
);

INSERT INTO "ParticipantDetailGenerations" (game_id, generation)
SELECT id, 1 FROM "Games"
ON CONFLICT (game_id) DO NOTHING;

CREATE INDEX IF NOT EXISTS ix_participant_detail_submission_cursor
    ON "Submissions" (game_id, id DESC);

CREATE OR REPLACE FUNCTION bump_participant_detail_generation(target_game INTEGER)
RETURNS VOID LANGUAGE SQL AS $$
  INSERT INTO "ParticipantDetailGenerations" (game_id, generation)
  VALUES (target_game, 2)
  ON CONFLICT (game_id) DO UPDATE
     SET generation = "ParticipantDetailGenerations".generation + 1;
$$;

CREATE OR REPLACE FUNCTION bump_participant_detail_from_game_row()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  PERFORM bump_participant_detail_generation(COALESCE(NEW.game_id, OLD.game_id));
  IF TG_OP = 'UPDATE' AND NEW.game_id IS DISTINCT FROM OLD.game_id THEN
    PERFORM bump_participant_detail_generation(OLD.game_id);
  END IF;
  RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE OR REPLACE FUNCTION bump_participant_detail_from_team_row()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE changed_team INTEGER := COALESCE(NEW.team_id, OLD.team_id);
DECLARE event_id INTEGER;
BEGIN
  FOR event_id IN SELECT DISTINCT game_id FROM "Participations" WHERE team_id = changed_team LOOP
    PERFORM bump_participant_detail_generation(event_id);
  END LOOP;
  IF TG_OP = 'UPDATE' AND NEW.team_id IS DISTINCT FROM OLD.team_id THEN
    FOR event_id IN SELECT DISTINCT game_id FROM "Participations" WHERE team_id = OLD.team_id LOOP
      PERFORM bump_participant_detail_generation(event_id);
    END LOOP;
  END IF;
  RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE OR REPLACE FUNCTION bump_participant_detail_from_team()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE changed_team INTEGER := COALESCE(NEW.id, OLD.id);
DECLARE event_id INTEGER;
BEGIN
  FOR event_id IN SELECT DISTINCT game_id FROM "Participations" WHERE team_id = changed_team LOOP
    PERFORM bump_participant_detail_generation(event_id);
  END LOOP;
  RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE OR REPLACE FUNCTION bump_participant_detail_from_account()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE event_id INTEGER;
BEGIN
  FOR event_id IN
    SELECT DISTINCT participation.game_id
      FROM "Participations" participation
      JOIN "Teams" team ON team.id = participation.team_id
     WHERE team.captain_id = COALESCE(NEW.id, OLD.id)
        OR EXISTS (
             SELECT 1 FROM "TeamMembers" member
              WHERE member.team_id = team.id
                AND member.user_id = COALESCE(NEW.id, OLD.id)
           )
  LOOP
    PERFORM bump_participant_detail_generation(event_id);
  END LOOP;
  RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE OR REPLACE FUNCTION bump_participant_detail_from_division_config()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE event_id INTEGER;
BEGIN
  SELECT game_id INTO event_id FROM "GameChallenges"
   WHERE id = COALESCE(NEW.challenge_id, OLD.challenge_id);
  IF event_id IS NOT NULL THEN
    PERFORM bump_participant_detail_generation(event_id);
  END IF;
  RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE OR REPLACE FUNCTION bump_participant_detail_from_game()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  PERFORM bump_participant_detail_generation(COALESCE(NEW.id, OLD.id));
  RETURN COALESCE(NEW, OLD);
END;
$$;

DROP TRIGGER IF EXISTS tr_participant_detail_submission_mutation ON "Submissions";
CREATE TRIGGER tr_participant_detail_submission_mutation
AFTER UPDATE OR DELETE ON "Submissions"
FOR EACH ROW EXECUTE FUNCTION bump_participant_detail_from_game_row();

DROP TRIGGER IF EXISTS tr_participant_detail_participation ON "Participations";
CREATE TRIGGER tr_participant_detail_participation
AFTER INSERT OR UPDATE OR DELETE ON "Participations"
FOR EACH ROW EXECUTE FUNCTION bump_participant_detail_from_game_row();

DROP TRIGGER IF EXISTS tr_participant_detail_challenge ON "GameChallenges";
CREATE TRIGGER tr_participant_detail_challenge
AFTER INSERT OR UPDATE OR DELETE ON "GameChallenges"
FOR EACH ROW EXECUTE FUNCTION bump_participant_detail_from_game_row();

DROP TRIGGER IF EXISTS tr_participant_detail_team_member ON "TeamMembers";
CREATE TRIGGER tr_participant_detail_team_member
AFTER INSERT OR UPDATE OR DELETE ON "TeamMembers"
FOR EACH ROW EXECUTE FUNCTION bump_participant_detail_from_team_row();

DROP TRIGGER IF EXISTS tr_participant_detail_team ON "Teams";
CREATE TRIGGER tr_participant_detail_team
AFTER UPDATE OR DELETE ON "Teams"
FOR EACH ROW EXECUTE FUNCTION bump_participant_detail_from_team();

DROP TRIGGER IF EXISTS tr_participant_detail_account ON "AspNetUsers";
DROP TRIGGER IF EXISTS tr_participant_detail_account_user_name ON "AspNetUsers";
CREATE TRIGGER tr_participant_detail_account_user_name
AFTER UPDATE OF user_name ON "AspNetUsers"
FOR EACH ROW
WHEN (NEW.user_name IS DISTINCT FROM OLD.user_name)
EXECUTE FUNCTION bump_participant_detail_from_account();

DROP TRIGGER IF EXISTS tr_participant_detail_account_delete ON "AspNetUsers";
CREATE TRIGGER tr_participant_detail_account_delete
AFTER DELETE ON "AspNetUsers"
FOR EACH ROW EXECUTE FUNCTION bump_participant_detail_from_account();

DROP TRIGGER IF EXISTS tr_participant_detail_division_config ON "DivisionChallengeConfigs";
CREATE TRIGGER tr_participant_detail_division_config
AFTER INSERT OR UPDATE OR DELETE ON "DivisionChallengeConfigs"
FOR EACH ROW EXECUTE FUNCTION bump_participant_detail_from_division_config();

DROP TRIGGER IF EXISTS tr_participant_detail_division ON "Divisions";
CREATE TRIGGER tr_participant_detail_division
AFTER INSERT OR UPDATE OR DELETE ON "Divisions"
FOR EACH ROW EXECUTE FUNCTION bump_participant_detail_from_game_row();

DROP TRIGGER IF EXISTS tr_participant_detail_game ON "Games";
CREATE TRIGGER tr_participant_detail_game
AFTER UPDATE ON "Games"
FOR EACH ROW EXECUTE FUNCTION bump_participant_detail_from_game();
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

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::UP_SQL;

    #[test]
    fn generation_covers_score_catalog_and_roster_inputs() {
        for table in [
            "Participations",
            "GameChallenges",
            "TeamMembers",
            "Teams",
            "AspNetUsers",
            "Divisions",
            "DivisionChallengeConfigs",
        ] {
            assert!(
                UP_SQL.contains(&format!("ON \"{table}\"")),
                "missing {table} trigger"
            );
        }
        assert!(UP_SQL.contains("ON CONFLICT (game_id) DO UPDATE"));
        assert!(UP_SQL.contains("ON \"Submissions\" (game_id, id DESC)"));
        assert!(UP_SQL.contains("AFTER UPDATE OR DELETE ON \"Submissions\""));
        assert!(!UP_SQL.contains("AFTER INSERT OR UPDATE OR DELETE ON \"Submissions\""));
        assert!(UP_SQL.contains("AFTER UPDATE OF user_name ON \"AspNetUsers\""));
        assert!(UP_SQL.contains("WHEN (NEW.user_name IS DISTINCT FROM OLD.user_name)"));
        assert!(UP_SQL.contains("AFTER DELETE ON \"AspNetUsers\""));
        assert!(!UP_SQL.contains("AFTER UPDATE OR DELETE ON \"AspNetUsers\""));
    }

    #[test]
    fn submission_invalidation_trigger_is_not_removed_after_creation() {
        let create = UP_SQL
            .find("CREATE TRIGGER tr_participant_detail_submission_mutation")
            .expect("submission invalidation trigger must be created");
        let statements_after_creation = &UP_SQL[create..];

        assert!(!statements_after_creation
            .contains("DROP TRIGGER IF EXISTS tr_participant_detail_submission_mutation"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn account_activity_does_not_invalidate_participant_rows() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_m0208_{}", uuid::Uuid::new_v4().simple());
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
               CREATE TABLE "Submissions" (id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL);
               CREATE TABLE "Participations" (game_id INTEGER NOT NULL, team_id INTEGER NOT NULL);
               CREATE TABLE "GameChallenges" (id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL);
               CREATE TABLE "TeamMembers" (team_id INTEGER NOT NULL, user_id UUID NOT NULL);
               CREATE TABLE "Teams" (id INTEGER PRIMARY KEY, captain_id UUID);
               CREATE TABLE "AspNetUsers" (
                 id UUID PRIMARY KEY, user_name TEXT,
                 last_visited_utc TIMESTAMPTZ NOT NULL
               );
               CREATE TABLE "DivisionChallengeConfigs" (challenge_id INTEGER NOT NULL);
               CREATE TABLE "Divisions" (game_id INTEGER NOT NULL);
               INSERT INTO "Games" VALUES (7);
               INSERT INTO "AspNetUsers" VALUES
                 ('00000000-0000-4000-8000-000000000208', 'player', clock_timestamp());
               INSERT INTO "Teams" VALUES
                 (21, '00000000-0000-4000-8000-000000000208');
               INSERT INTO "Participations" VALUES (7, 21);"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();

        let generation = || async {
            sqlx::query_scalar::<_, i64>(
                r#"SELECT generation FROM "ParticipantDetailGenerations" WHERE game_id = 7"#,
            )
            .fetch_one(&pool)
            .await
            .unwrap()
        };
        assert_eq!(generation().await, 1);
        sqlx::query(
            r#"UPDATE "AspNetUsers"
                  SET last_visited_utc = clock_timestamp(), user_name = user_name
                WHERE id = '00000000-0000-4000-8000-000000000208'"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(generation().await, 1);
        sqlx::query(
            r#"UPDATE "AspNetUsers" SET user_name = 'renamed-player'
                WHERE id = '00000000-0000-4000-8000-000000000208'"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(generation().await, 2);
        sqlx::query(
            r#"DELETE FROM "AspNetUsers"
                WHERE id = '00000000-0000-4000-8000-000000000208'"#,
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
