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
CREATE TRIGGER tr_koth_context_generation AFTER UPDATE OR DELETE ON "AspNetUsers"
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
    use super::UP_SQL;

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
    }
}
