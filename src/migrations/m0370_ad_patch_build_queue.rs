use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
-- Workers recheck the current game, accepted participation and division's
-- VIEW_CHALLENGE bit when reading source and when publishing a candidate.
CREATE OR REPLACE VIEW "AdPatchBuildEligibleServices" AS
SELECT s.id AS service_id, c.id AS challenge_id FROM "AdTeamServices" s
JOIN "GameChallenges" c ON c.id=s.challenge_id AND c.game_id=s.game_id
JOIN "Games" g ON g.id=c.game_id
JOIN "Participations" p ON p.id=s.participation_id AND p.game_id=g.id
JOIN "Teams" t ON t.id=p.team_id AND NOT t.deletion_pending
WHERE c."Type"=4 AND NOT c.ad_self_hosted AND c.is_enabled AND c.review_status=0
  AND NOT c.deletion_pending AND NOT g.deletion_pending AND p.status=1
  AND g.start_time_utc<=clock_timestamp() AND clock_timestamp()<g.end_time_utc
  AND (p.division_id IS NULL OR EXISTS (
      SELECT 1 FROM "Divisions" d LEFT JOIN "DivisionChallengeConfigs" dc
        ON dc.division_id=d.id AND dc.challenge_id=c.id
      WHERE d.id=p.division_id AND d.game_id=g.id
        AND (COALESCE(dc.permissions,d.default_permissions) & 256) = 256
  ));
CREATE TABLE IF NOT EXISTS "AdPatchBuilders" (
    challenge_id INTEGER PRIMARY KEY REFERENCES "GameChallenges"(id) ON DELETE CASCADE,
    backend_pid INTEGER NOT NULL,
    profile TEXT NOT NULL CHECK (profile ~ '^[0-9a-f]{64}$')
);
CREATE OR REPLACE VIEW "AdPatchOnlineBuilders" AS
SELECT w.challenge_id,w.profile,w.backend_pid FROM "AdPatchBuilders" w
WHERE EXISTS (SELECT 1 FROM pg_locks l WHERE l.locktype='advisory'
    AND l.classid=1734271011::oid AND l.objid=w.challenge_id::oid AND l.objsubid=2
    AND l.pid=w.backend_pid AND l.granted AND l.mode='ExclusiveLock'
    AND l.database=(SELECT oid FROM pg_database WHERE datname=current_database()));
CREATE TABLE IF NOT EXISTS "AdPatchBuilds" (
    service_id INTEGER NOT NULL, revision INTEGER NOT NULL, attempt UUID NOT NULL UNIQUE,
    profile TEXT NOT NULL, status TEXT NOT NULL CHECK (status IN ('Queued','Building','Ready','Failed','Replaced')),
    claim UUID, claimed_by INTEGER, queued_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(service_id,revision),
    FOREIGN KEY(service_id,revision) REFERENCES "AdPatchRevisions"(service_id,revision) ON DELETE CASCADE
);
CREATE UNIQUE INDEX IF NOT EXISTS ux_ad_patch_build_busy ON "AdPatchBuilds"(service_id)
    WHERE status IN ('Queued','Building');
CREATE UNIQUE INDEX IF NOT EXISTS ux_ad_patch_build_ready ON "AdPatchBuilds"(service_id)
    WHERE status='Ready';
CREATE INDEX IF NOT EXISTS ix_ad_patch_build_pending ON "AdPatchBuilds"(queued_at,service_id)
    WHERE status IN ('Queued','Building');
-- Only the last successful candidate is retained per service. Chunk rows keep
-- reading, writing and replacing large private images bounded in application RAM.
CREATE TABLE IF NOT EXISTS "AdPatchCandidates" (
    service_id INTEGER PRIMARY KEY REFERENCES "AdTeamServices"(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL, digest TEXT NOT NULL CHECK (digest ~ '^[0-9a-f]{64}$'),
    archive_bytes INTEGER NOT NULL CHECK (archive_bytes BETWEEN 1 AND 268435456),
    FOREIGN KEY(service_id,revision) REFERENCES "AdPatchBuilds"(service_id,revision) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS "AdPatchCandidateChunks" (
    service_id INTEGER NOT NULL REFERENCES "AdPatchCandidates"(service_id) ON DELETE CASCADE,
    part INTEGER NOT NULL CHECK (part BETWEEN 0 AND 255),
    payload BYTEA NOT NULL CHECK (octet_length(payload) BETWEEN 1 AND 1048576),
    PRIMARY KEY(service_id,part)
);
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
