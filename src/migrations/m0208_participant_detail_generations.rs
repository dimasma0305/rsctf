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
DROP TRIGGER IF EXISTS tr_participant_detail_submission_mutation ON "Submissions";
CREATE TRIGGER tr_participant_detail_team
AFTER UPDATE OR DELETE ON "Teams"
FOR EACH ROW EXECUTE FUNCTION bump_participant_detail_from_team();

DROP TRIGGER IF EXISTS tr_participant_detail_account ON "AspNetUsers";
CREATE TRIGGER tr_participant_detail_account
AFTER UPDATE OR DELETE ON "AspNetUsers"
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

pub(crate) const DOWN_SQL: &str = r#"
DROP TRIGGER IF EXISTS tr_participant_detail_team ON "Teams";
DROP TRIGGER IF EXISTS tr_participant_detail_game ON "Games";
DROP TRIGGER IF EXISTS tr_participant_detail_division ON "Divisions";
DROP TRIGGER IF EXISTS tr_participant_detail_division_config ON "DivisionChallengeConfigs";
DROP TRIGGER IF EXISTS tr_participant_detail_account ON "AspNetUsers";
DROP TRIGGER IF EXISTS tr_participant_detail_team_member ON "TeamMembers";
DROP TRIGGER IF EXISTS tr_participant_detail_challenge ON "GameChallenges";
DROP TRIGGER IF EXISTS tr_participant_detail_participation ON "Participations";
DROP FUNCTION IF EXISTS bump_participant_detail_from_team();
DROP FUNCTION IF EXISTS bump_participant_detail_from_game();
DROP FUNCTION IF EXISTS bump_participant_detail_from_division_config();
DROP FUNCTION IF EXISTS bump_participant_detail_from_account();
DROP FUNCTION IF EXISTS bump_participant_detail_from_team_row();
DROP FUNCTION IF EXISTS bump_participant_detail_from_game_row();
DROP FUNCTION IF EXISTS bump_participant_detail_generation(INTEGER);
DROP TABLE IF EXISTS "ParticipantDetailGenerations";
DROP INDEX IF EXISTS ix_participant_detail_submission_cursor;
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
            .execute_unprepared(DOWN_SQL)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
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
    }
}
