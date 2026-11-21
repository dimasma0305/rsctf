use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "AdTeamServices"
    ADD COLUMN IF NOT EXISTS published_image_backend TEXT,
    ADD COLUMN IF NOT EXISTS published_image_ref TEXT,
    ADD COLUMN IF NOT EXISTS previous_image_ref TEXT;

CREATE OR REPLACE FUNCTION rsctf_record_service_image() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    -- An unpublished replacement temporarily owns the row during teardown.
    -- Retain the last publication until the replacement actually serves.
    IF NEW.host='' OR NEW.port=0 THEN RETURN NEW; END IF;
    IF NEW.container_id IS NULL OR NEW.published_image_backend IS DISTINCT FROM NEW.container_id THEN
        -- BYOC and older servers can publish endpoints without an image receipt.
        NEW.published_image_backend := NULL;
        NEW.published_image_ref := NULL;
        NEW.previous_image_ref := NULL;
    ELSIF TG_OP='UPDATE' AND NEW.published_image_ref IS DISTINCT FROM OLD.published_image_ref THEN
        NEW.previous_image_ref := OLD.published_image_ref;
    END IF;
    RETURN NEW;
END $$;
CREATE OR REPLACE TRIGGER record_service_image_on_publication
BEFORE INSERT OR UPDATE OF host,port,container_id,published_image_ref ON "AdTeamServices"
FOR EACH ROW EXECUTE FUNCTION rsctf_record_service_image();

CREATE INDEX IF NOT EXISTS ix_ad_service_published_local_image
    ON "AdTeamServices" (lower(published_image_ref)) WHERE published_image_ref IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_ad_service_previous_local_image
    ON "AdTeamServices" (lower(previous_image_ref)) WHERE previous_image_ref IS NOT NULL;

CREATE OR REPLACE VIEW "AdProtectedLocalImages" AS
SELECT lower(s.runtime_image_ref) AS image_id,c.id AS challenge_id
  FROM "AdPatchStarters" s JOIN "GameChallenges" c ON c.id=s.challenge_id
  JOIN "Games" g ON g.id=c.game_id
 WHERE lower(s.runtime_image_ref) LIKE 'sha256:%' AND g.end_time_utc>clock_timestamp()
UNION
SELECT lower(btrim(c.build_image_digest)),c.id
  FROM "GameChallenges" c JOIN "Games" g ON g.id=c.game_id
 WHERE c."Type"=4 AND NOT c.ad_self_hosted AND c.build_status=1
   AND lower(btrim(c.build_image_digest)) LIKE 'sha256:%' AND g.end_time_utc>clock_timestamp()
UNION
SELECT lower(s.published_image_ref),s.challenge_id FROM "AdTeamServices" s JOIN "Games" g ON g.id=s.game_id
 WHERE lower(s.published_image_ref) LIKE 'sha256:%' AND g.end_time_utc>clock_timestamp()
UNION
SELECT lower(s.previous_image_ref),s.challenge_id FROM "AdTeamServices" s JOIN "Games" g ON g.id=s.game_id
 WHERE lower(s.previous_image_ref) LIKE 'sha256:%' AND g.end_time_utc>clock_timestamp();
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
