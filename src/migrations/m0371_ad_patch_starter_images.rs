use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "AdPatchStarters"
    ADD COLUMN IF NOT EXISTS runtime_image_ref TEXT,
    ADD COLUMN IF NOT EXISTS awaiting_image_build BOOLEAN NOT NULL DEFAULT FALSE;

-- The editor already locks the challenge row before changing a starter.
-- Keep the binding at the storage boundary so older editors during an
-- upgrade also replace the source and its runtime identity together.
CREATE OR REPLACE FUNCTION rsctf_capture_patch_starter_image() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    before_start BOOLEAN;
    reviewed_image TEXT;
BEGIN
    IF TG_OP='UPDATE' AND NEW.tree_id=OLD.tree_id THEN
        RETURN NEW;
    END IF;
    SELECT g.start_time_utc>clock_timestamp(),
           CASE WHEN c.build_status=1 THEN btrim(c.build_image_digest) END
      INTO before_start,reviewed_image
      FROM "GameChallenges" c JOIN "Games" g ON g.id=c.game_id
     WHERE c.id=NEW.challenge_id AND c."Type"=4 AND NOT c.ad_self_hosted;
    NEW.runtime_image_ref := CASE WHEN before_start THEN reviewed_image END;
    NEW.awaiting_image_build := COALESCE(before_start,FALSE) AND reviewed_image IS NULL;
    RETURN NEW;
END $$;
CREATE OR REPLACE TRIGGER capture_image_on_patch_starter
BEFORE INSERT OR UPDATE OF tree_id ON "AdPatchStarters"
FOR EACH ROW EXECUTE FUNCTION rsctf_capture_patch_starter_image();

-- Capture the first successful organizer build if the ZIP was registered
-- first. Existing ZIPs have no historical receipt and stay unbound.
CREATE OR REPLACE FUNCTION rsctf_bind_patch_starter_image() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    UPDATE "AdPatchStarters" s
       SET runtime_image_ref = CASE WHEN g.start_time_utc > clock_timestamp()
                 AND NOT EXISTS (SELECT 1 FROM "AdTeamServices" a
                     JOIN "AdPatchSourceUsage" u ON u.service_id=a.id
                     WHERE a.challenge_id=NEW.id AND a.game_id=g.id)
               THEN btrim(NEW.build_image_digest) END,
           awaiting_image_build = FALSE
      FROM "Games" g
     WHERE s.challenge_id=NEW.id AND g.id=NEW.game_id AND s.awaiting_image_build;
    RETURN NULL;
END $$;
CREATE OR REPLACE TRIGGER bind_patch_starter_on_build
AFTER UPDATE OF build_status,build_image_digest ON "GameChallenges"
FOR EACH ROW WHEN (NEW."Type"=4 AND NOT NEW.ad_self_hosted AND NEW.build_status=1
    AND NEW.build_image_digest IS NOT NULL
    AND (NEW.build_status IS DISTINCT FROM OLD.build_status
        OR NEW.build_image_digest IS DISTINCT FROM OLD.build_image_digest))
EXECUTE FUNCTION rsctf_bind_patch_starter_image();

CREATE INDEX IF NOT EXISTS ix_ad_patch_starter_local_image
    ON "AdPatchStarters" (lower(runtime_image_ref)) WHERE runtime_image_ref IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_ad_hosted_runtime_image
    ON "GameChallenges" (lower(btrim(build_image_digest)))
    WHERE "Type"=4 AND NOT ad_self_hosted AND build_status=1;
CREATE OR REPLACE VIEW "AdProtectedLocalImages" AS
SELECT lower(s.runtime_image_ref) AS image_id,c.id AS challenge_id
  FROM "AdPatchStarters" s JOIN "GameChallenges" c ON c.id=s.challenge_id
  JOIN "Games" g ON g.id=c.game_id
 WHERE lower(s.runtime_image_ref) LIKE 'sha256:%' AND g.end_time_utc>clock_timestamp()
UNION ALL
SELECT lower(btrim(c.build_image_digest)) AS image_id,c.id AS challenge_id
  FROM "GameChallenges" c JOIN "Games" g ON g.id=c.game_id
 WHERE c."Type"=4 AND NOT c.ad_self_hosted AND c.build_status=1
   AND lower(btrim(c.build_image_digest)) LIKE 'sha256:%'
   AND g.end_time_utc>clock_timestamp();
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
