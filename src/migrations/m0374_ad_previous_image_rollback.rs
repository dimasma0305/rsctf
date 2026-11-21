use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
-- Old replicas can still enqueue ordinary resets. Serialize both intents at the
-- database boundary even when callers use different operation IDs.
CREATE UNIQUE INDEX IF NOT EXISTS ix_ad_one_active_replacement
    ON "ControlPlaneJobs" (scope_key) WHERE kind IN ('AdReset','AdRollback') AND status IN (0,1);

CREATE OR REPLACE VIEW "AdPreviousImageTargets" AS
SELECT s.id AS service_id, s.game_id, s.participation_id, s.challenge_id,
       s.container_id, s.previous_image_ref
FROM "AdPatchBuildEligibleServices" eligible
JOIN "AdTeamServices" s ON s.id=eligible.service_id
JOIN "GameChallenges" c ON c.id=s.challenge_id
WHERE c.ad_allow_self_reset AND s.host<>'' AND s.port BETWEEN 1 AND 65535
  AND s.published_image_backend=s.container_id AND s.published_image_ref IS NOT NULL
  AND s.previous_image_ref IS NOT NULL AND s.previous_image_ref<>s.published_image_ref;

CREATE INDEX IF NOT EXISTS ix_ad_rollback_prepared_image
    ON "ControlPlaneJobs" (lower(input->>'preparedImageRef'))
    WHERE kind='AdRollback' AND status IN (0,1) AND input ? 'preparedImageRef';

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
 WHERE lower(s.previous_image_ref) LIKE 'sha256:%' AND g.end_time_utc>clock_timestamp()
UNION
SELECT lower(j.input->>'preparedImageRef'),j.challenge_id FROM "ControlPlaneJobs" j JOIN "Games" g ON g.id=j.game_id
 WHERE j.kind IN ('AdReset','AdRollback') AND j.status IN (0,1) AND j.challenge_id IS NOT NULL
   AND j.input ? 'preparedImageRef' AND lower(j.input->>'preparedImageRef') LIKE 'sha256:%'
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
