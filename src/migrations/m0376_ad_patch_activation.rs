use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE UNIQUE INDEX IF NOT EXISTS ix_ad_active_service_change
    ON "ControlPlaneJobs"(scope_key)
    WHERE kind IN ('AdReset','AdRollback','AdActivate') AND status IN (0,1);
DROP INDEX IF EXISTS ix_ad_one_active_replacement;

CREATE OR REPLACE VIEW "AdPatchActivationTargets" AS
SELECT c.service_id, c.revision, b.attempt, c.registry_image_ref, s.container_id, ch.runtime_profile
FROM "AdPatchCandidates" c
JOIN "AdPatchBuilds" b USING(service_id,revision)
JOIN "AdPatchBuildEligibleServices" e USING(service_id)
JOIN "AdTeamServices" s ON s.id=c.service_id
JOIN "GameChallenges" ch ON ch.id=s.challenge_id
JOIN "Games" g ON g.id=s.game_id
JOIN "AdPatchStarters" starter ON starter.challenge_id=ch.id
WHERE b.status='Ready' AND c.registry_image_ref IS NOT NULL
  AND ch.ad_allow_self_reset AND starter.runtime_image_ref IS NOT NULL
  AND (s.last_reset_at IS NULL OR s.last_reset_at <= clock_timestamp()
       - make_interval(secs => greatest(30,coalesce(g.ad_reset_cooldown_minutes::bigint*60,300))))
  AND s.host<>'' AND s.port BETWEEN 1 AND 65535
  AND s.published_image_backend=s.container_id
  AND s.published_image_ref NOT IN (c.registry_image_ref,c.config_digest);
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
