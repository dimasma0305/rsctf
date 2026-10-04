//! Durable recovery for resources created after a participation is accepted.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "ParticipationProvisionJobs" (
    participation_id INTEGER PRIMARY KEY
        REFERENCES "Participations" (id) ON DELETE CASCADE,
    game_id INTEGER NOT NULL REFERENCES "Games" (id) ON DELETE CASCADE,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    lease_owner UUID NULL,
    lease_until TIMESTAMPTZ NULL,
    last_error TEXT NULL,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

CREATE INDEX IF NOT EXISTS ix_participationprovisionjobs_due
    ON "ParticipationProvisionJobs" (next_attempt_at, participation_id);

-- Recover already-accepted teams that predate the durable queue and are still
-- entitled to a managed A&D service. Enum values are the stable database
-- values: Accepted=1, Active=0, AttackDefense=4.
INSERT INTO "ParticipationProvisionJobs"
    (participation_id, game_id, attempts, next_attempt_at,
     lease_owner, lease_until, last_error, updated_at_utc)
SELECT DISTINCT participation.id, participation.game_id, 0, clock_timestamp(),
       NULL::UUID, NULL::TIMESTAMPTZ, NULL::TEXT, clock_timestamp()
  FROM "Participations" participation
  JOIN "Games" game ON game.id = participation.game_id
 WHERE participation.status = 1
   AND game.deletion_pending = FALSE
   AND (game.practice_mode = TRUE OR game.end_time_utc >= clock_timestamp())
   AND EXISTS (
       SELECT 1
         FROM "GameChallenges" challenge
        WHERE challenge.game_id = participation.game_id
          AND challenge.is_enabled = TRUE
          AND challenge.review_status = 0
          AND challenge."Type" = 4
          AND challenge.deletion_pending = FALSE
          AND (
              (
                  challenge.ad_self_hosted = TRUE
                  AND NOT EXISTS (
                      SELECT 1
                        FROM "AdTeamServices" service
                       WHERE service.participation_id = participation.id
                         AND service.challenge_id = challenge.id
                  )
              )
              OR (
                  challenge.ad_self_hosted = FALSE
                  AND NULLIF(BTRIM(challenge.container_image), '') IS NOT NULL
                  AND NOT EXISTS (
                      SELECT 1
                        FROM "AdTeamServices" service
                       WHERE service.participation_id = participation.id
                         AND service.challenge_id = challenge.id
                         AND NULLIF(BTRIM(service.container_id), '') IS NOT NULL
                         AND NULLIF(BTRIM(service.host), '') IS NOT NULL
                         AND service.port > 0
                  )
              )
          )
   )
ON CONFLICT (participation_id) DO NOTHING;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Production migrations are forward-only. Retaining queued work is safe
        // for older binaries, which simply leave it untouched.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn recovery_queue_is_durable_idempotent_due_indexed_and_backfilled() {
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS"));
        assert!(UP_SQL.contains("participation_id INTEGER PRIMARY KEY"));
        assert!(UP_SQL.contains("lease_owner UUID NULL"));
        assert!(UP_SQL.contains("CREATE INDEX IF NOT EXISTS ix_participationprovisionjobs_due"));
        assert!(UP_SQL.contains("SELECT DISTINCT participation.id"));
        assert!(UP_SQL.contains("participation.status = 1"));
        assert!(UP_SQL.contains("game.practice_mode = TRUE OR game.end_time_utc"));
        assert!(UP_SQL.contains("challenge.\"Type\" = 4"));
        assert!(UP_SQL.contains("challenge.ad_self_hosted = TRUE"));
        assert!(UP_SQL.contains("challenge.ad_self_hosted = FALSE"));
        assert!(UP_SQL.contains("FROM \"AdTeamServices\" service"));
        assert!(UP_SQL.contains("service.port > 0"));
        assert!(UP_SQL.contains("ON CONFLICT (participation_id) DO NOTHING"));
    }
}
