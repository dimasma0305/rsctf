use sea_orm_migration::prelude::*;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "CompetitionMaintenanceWindows" (
    id UUID PRIMARY KEY,
    game_id INTEGER NOT NULL REFERENCES "Games"(id) ON DELETE CASCADE,
    actor_id UUID NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('Requested','Editing','Applying','Completed','Cancelled')),
    effective_from_round INTEGER CHECK (effective_from_round > 0),
    before_snapshot JSONB,
    after_snapshot JSONB,
    touched_challenge_ids INTEGER[] NOT NULL DEFAULT '{}' CHECK (cardinality(touched_challenge_ids) <= 256),
    error TEXT CHECK (octet_length(error) <= 4000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    entered_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    CHECK (state IN ('Requested','Cancelled') OR
           (effective_from_round IS NOT NULL AND before_snapshot IS NOT NULL)),
    CHECK (state <> 'Applying' OR after_snapshot IS NOT NULL)
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_competition_maintenance_active
    ON "CompetitionMaintenanceWindows" (game_id)
    WHERE state IN ('Requested','Editing','Applying');
CREATE INDEX IF NOT EXISTS ix_competition_maintenance_history
    ON "CompetitionMaintenanceWindows" (game_id, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS ix_competition_maintenance_before
    ON "CompetitionMaintenanceWindows" USING gin (before_snapshot jsonb_path_ops);
CREATE INDEX IF NOT EXISTS ix_competition_maintenance_after
    ON "CompetitionMaintenanceWindows" USING gin (after_snapshot jsonb_path_ops);

-- Do not copy unbounded presentation content, hints or build logs into every
-- operational revision. Keep only runtime settings and source identities.
CREATE OR REPLACE FUNCTION rsctf_competition_definition(definition "GameChallenges")
RETURNS jsonb LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$
    SELECT jsonb_build_object(
        'id', definition.id, 'title', definition.title, 'revision', definition.revision,
        'container_image', definition.container_image,
        'build_image_digest', definition.build_image_digest,
        'ad_checker_image', definition.ad_checker_image,
        'original_archive_blob_path', definition.original_archive_blob_path,
        'build_context_subdir', definition.build_context_subdir,
        'workload_spec', definition.workload_spec,
        'runtime_profile', definition.runtime_profile,
        'memory_limit', definition.memory_limit, 'cpu_count', definition.cpu_count,
        'storage_limit', definition.storage_limit, 'network_mode', definition.network_mode,
        'expose_port', definition.expose_port, 'ad_allow_egress', definition.ad_allow_egress,
        'ad_self_hosted', definition.ad_self_hosted, 'is_enabled', definition.is_enabled,
        'enable_traffic_capture', definition.enable_traffic_capture,
        'ad_allow_self_reset', definition.ad_allow_self_reset,
        'ad_ssh_requires_flag', definition.ad_ssh_requires_flag)
$$;

CREATE OR REPLACE FUNCTION rsctf_maintenance_definition_fence() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF current_setting('rsctf.maintenance_writer', TRUE) IS DISTINCT FROM 'on'
       AND OLD."Type" IN (4,5)
       AND ROW(NEW.build_image_digest, NEW.build_status) IS DISTINCT FROM ROW(OLD.build_image_digest, OLD.build_status)
       AND EXISTS (SELECT 1 FROM "Games" WHERE id = OLD.game_id
                    AND (ad_scoring_start_round IS NOT NULL OR koth_scoring_start_round IS NOT NULL))
       AND NOT EXISTS (SELECT 1 FROM "CompetitionMaintenanceWindows"
                        WHERE game_id = OLD.game_id AND state = 'Editing')
    THEN
        RAISE EXCEPTION 'Open a maintenance window before rebuilding a scored challenge'
            USING ERRCODE = '40001';
    END IF;
    IF current_setting('rsctf.maintenance_writer', TRUE) IS DISTINCT FROM 'on'
       AND EXISTS (SELECT 1 FROM "CompetitionMaintenanceWindows"
                    WHERE game_id = OLD.game_id AND state = 'Applying')
       AND ROW(NEW.container_image, NEW.build_image_digest, NEW.build_status,
           NEW.ad_checker_image, NEW.workload_spec, NEW.runtime_profile,
           NEW.memory_limit, NEW.cpu_count, NEW.storage_limit, NEW.network_mode,
           NEW.expose_port, NEW.ad_allow_egress, NEW.ad_self_hosted, NEW.is_enabled,
           NEW.enable_traffic_capture, NEW.ad_allow_self_reset, NEW.ad_ssh_requires_flag)
           IS DISTINCT FROM ROW(OLD.container_image, OLD.build_image_digest, OLD.build_status,
           OLD.ad_checker_image, OLD.workload_spec, OLD.runtime_profile,
           OLD.memory_limit, OLD.cpu_count, OLD.storage_limit, OLD.network_mode,
           OLD.expose_port, OLD.ad_allow_egress, OLD.ad_self_hosted, OLD.is_enabled,
           OLD.enable_traffic_capture, OLD.ad_allow_self_reset, OLD.ad_ssh_requires_flag)
    THEN
        RAISE EXCEPTION 'Maintenance is applying this competition; retry after completion'
            USING ERRCODE = '40001';
    END IF;
    RETURN NEW;
END $$;
DROP TRIGGER IF EXISTS tr_competition_maintenance_definition ON "GameChallenges";
CREATE TRIGGER tr_competition_maintenance_definition BEFORE UPDATE ON "GameChallenges"
    FOR EACH ROW EXECUTE FUNCTION rsctf_maintenance_definition_fence();

CREATE OR REPLACE FUNCTION rsctf_maintenance_resume_fence() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.ad_scoring_paused AND NOT NEW.ad_scoring_paused
       AND EXISTS (SELECT 1 FROM "CompetitionMaintenanceWindows"
                    WHERE game_id = OLD.id AND state IN ('Editing','Applying'))
    THEN
        RAISE EXCEPTION 'Apply the maintenance window before resuming scoring'
            USING ERRCODE = '40001';
    END IF;
    RETURN NEW;
END $$;
DROP TRIGGER IF EXISTS tr_competition_maintenance_resume ON "Games";
CREATE TRIGGER tr_competition_maintenance_resume BEFORE UPDATE OF ad_scoring_paused ON "Games"
    FOR EACH ROW EXECUTE FUNCTION rsctf_maintenance_resume_fence();

CREATE OR REPLACE FUNCTION rsctf_maintenance_observer_fence() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM "CompetitionMaintenanceWindows"
                WHERE game_id = COALESCE(NEW.game_id, OLD.game_id) AND state = 'Applying')
    THEN
        RAISE EXCEPTION 'Observer changes must wait until maintenance finishes'
            USING ERRCODE = '40001';
    END IF;
    RETURN COALESCE(NEW, OLD);
END $$;
DROP TRIGGER IF EXISTS tr_competition_maintenance_observer ON "KothApiObservers";
CREATE TRIGGER tr_competition_maintenance_observer BEFORE INSERT OR UPDATE OR DELETE ON "KothApiObservers"
    FOR EACH ROW EXECUTE FUNCTION rsctf_maintenance_observer_fence();
DROP TRIGGER IF EXISTS tr_competition_maintenance_observer_revision ON "KothApiObserverRevisions";
CREATE TRIGGER tr_competition_maintenance_observer_revision BEFORE INSERT OR UPDATE OR DELETE ON "KothApiObserverRevisions"
    FOR EACH ROW EXECUTE FUNCTION rsctf_maintenance_observer_fence();
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
        Err(DbErr::Custom(
            "Maintenance audit history cannot be removed by downgrade".into(),
        ))
    }
}
