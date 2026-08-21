DROP TRIGGER IF EXISTS trg_games_seed_suspicion_reconciliation ON "Games";
DROP FUNCTION IF EXISTS rsctf_seed_suspicion_reconciliation_state();
DROP TABLE IF EXISTS "SuspicionReconciliationState";
DROP TRIGGER IF EXISTS trg_submissions_evaluation_outbox ON "Submissions";
DROP FUNCTION IF EXISTS rsctf_enqueue_submission_evaluation();
DROP TABLE IF EXISTS "SuspicionEvaluationOutbox";
DROP FUNCTION IF EXISTS rsctf_guard_outbox_operational_update();

DROP INDEX IF EXISTS ix_suspicionevents_challenge;
DROP INDEX IF EXISTS ix_suspicionevents_participation;
DROP INDEX IF EXISTS ix_suspicionevents_created_id;
DROP INDEX IF EXISTS ix_containeraccess_game_time;
DROP INDEX IF EXISTS ix_containeraccess_challenge_owner_container_time;
DROP INDEX IF EXISTS ix_honeypothits_game_time;
DROP INDEX IF EXISTS ix_honeypothits_game_participation_time;

DROP TRIGGER IF EXISTS trg_cheatinfo_immutable ON "CheatInfo";
DROP TRIGGER IF EXISTS trg_cheatinfo_validate_insert ON "CheatInfo";
DROP FUNCTION IF EXISTS rsctf_reject_cheat_info_mutation();
DROP FUNCTION IF EXISTS rsctf_validate_cheat_info_insert();
DROP TRIGGER IF EXISTS trg_submission_core_immutable_update ON "Submissions";
DROP TRIGGER IF EXISTS trg_submission_core_immutable_delete ON "Submissions";
DROP FUNCTION IF EXISTS rsctf_reject_submission_mutation();
DROP TRIGGER IF EXISTS trg_firstsolve_validate_insert ON "FirstSolves";
DROP TRIGGER IF EXISTS trg_firstsolve_immutable ON "FirstSolves";
DROP FUNCTION IF EXISTS rsctf_validate_firstsolve_insert();
DROP TRIGGER IF EXISTS trg_containeraccess_immutable ON "ContainerAccessEvents";
DROP FUNCTION IF EXISTS rsctf_reject_evidence_mutation();
DROP TRIGGER IF EXISTS trg_submissions_immutable_observations ON "Submissions";
DROP FUNCTION IF EXISTS rsctf_reject_submission_observation_mutation();
DROP TRIGGER IF EXISTS trg_submissions_snapshot_observations ON "Submissions";
DROP FUNCTION IF EXISTS rsctf_snapshot_legacy_submission_observations();
DROP TRIGGER IF EXISTS trg_participations_competitive_admission ON "Participations";
DROP FUNCTION IF EXISTS rsctf_capture_competitive_admission();

ALTER TABLE "CheatInfo"
  DROP CONSTRAINT IF EXISTS ck_cheatinfo_evidence_contract,
  DROP CONSTRAINT IF EXISTS ck_cheatinfo_distinct_participations,
  DROP CONSTRAINT IF EXISTS fk_cheatinfo_source_participation,
  DROP CONSTRAINT IF EXISTS fk_cheatinfo_submission_provenance;
DROP INDEX IF EXISTS ix_cheatinfo_observed;
DROP INDEX IF EXISTS ix_cheatinfo_game_observed;
DROP INDEX IF EXISTS ux_cheatinfo_submission_id;
DROP INDEX IF EXISTS ux_submissions_cheat_provenance;

DROP INDEX IF EXISTS ix_submissions_container_id;
DROP INDEX IF EXISTS ix_submissions_submit_remote_ip_hash;
DROP INDEX IF EXISTS ix_submissions_wrong_game_part_challenge_time;
DROP INDEX IF EXISTS ix_submissions_game_time;
DROP INDEX IF EXISTS ix_participations_competitive_cohort;
DROP INDEX IF EXISTS ix_gameevents_submit_interactions;
DROP INDEX IF EXISTS ix_containeraccess_remote_ip_hash;
ALTER TABLE "Submissions"
  DROP CONSTRAINT IF EXISTS ck_submissions_immutable_observations,
  DROP COLUMN IF EXISTS first_container_start_at_submit,
  DROP COLUMN IF EXISTS first_download_at_submit,
  DROP COLUMN IF EXISTS first_open_at_submit,
  DROP COLUMN IF EXISTS container_was_loaded_at_submit,
  DROP COLUMN IF EXISTS container_last_operation_at_submit,
  DROP COLUMN IF EXISTS container_id,
  DROP COLUMN IF EXISTS submit_remote_ip_hash;

ALTER TABLE "Participations"
  DROP COLUMN IF EXISTS competitive_admitted_at_utc;

ALTER TABLE "ContainerAccessEvents"
  DROP CONSTRAINT IF EXISTS ck_containeraccess_remote_ip_hash,
  DROP COLUMN IF EXISTS is_monitor,
  DROP COLUMN IF EXISTS remote_ip_hash;

ALTER TABLE "CheatInfo"
  DROP COLUMN IF EXISTS evidence_version,
  DROP COLUMN IF EXISTS evidence_payload,
  DROP COLUMN IF EXISTS observed_at_utc,
  DROP COLUMN IF EXISTS evidence_key,
  DROP COLUMN IF EXISTS challenge_id,
  DROP COLUMN IF EXISTS source_participation_id,
  DROP COLUMN IF EXISTS submit_participation_id;
