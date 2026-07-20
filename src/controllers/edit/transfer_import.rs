//! Atomic persistence and deployment-local sanitization for game archives.

use std::collections::{BTreeMap, BTreeSet};

use sea_orm::DatabaseTransaction;

use super::*;

pub(super) async fn persist_game_import(
    st: &SharedState,
    entries: &BTreeMap<String, Vec<u8>>,
    export_game: &ExportGameModel,
    export_challenges: &[ExportChallengeModel],
) -> AppResult<i32> {
    let transaction = crate::utils::database::begin_seaorm_transaction(&st.db).await?;
    let mut stored_hashes = BTreeSet::new();
    let result = persist_game_import_locked(
        st,
        &transaction,
        entries,
        export_game,
        export_challenges,
        &mut stored_hashes,
    )
    .await;

    match result {
        Ok(game_id) => {
            transaction.commit().await?;
            Ok(game_id)
        }
        Err(error) => {
            match transaction.rollback().await {
                Ok(()) => purge_rolled_back_blobs(st, stored_hashes).await,
                Err(rollback_error) => tracing::error!(
                    %rollback_error,
                    "game import transaction rollback failed; skipping ambiguous blob cleanup"
                ),
            }
            Err(error)
        }
    }
}

async fn persist_game_import_locked(
    st: &SharedState,
    transaction: &DatabaseTransaction,
    entries: &BTreeMap<String, Vec<u8>>,
    export_game: &ExportGameModel,
    export_challenges: &[ExportChallengeModel],
    stored_hashes: &mut BTreeSet<String>,
) -> AppResult<i32> {
    let (public_key, private_key) = crate::utils::crypto_utils::generate_game_keypair();
    let new_game = imported_game_model(export_game, public_key, private_key)
        .insert(transaction)
        .await?;
    let mut challenge_id_map = BTreeMap::new();

    for src in export_challenges {
        let challenge_attachment_id = import_attachment(
            st,
            transaction,
            entries,
            src.attachment_type,
            src.attachment_file_hash.as_deref(),
            src.attachment_remote_url.as_deref(),
            src.attachment_file_name.as_deref(),
            stored_hashes,
        )
        .await?;
        let workload_spec = match src.workload_spec.clone() {
            Some(value) => Some(crate::services::challenge_workloads::to_json(
                crate::services::challenge_workloads::validate_json_for_challenge(
                    src.challenge_type,
                    value,
                )?,
            )?),
            None => None,
        };
        let challenge =
            imported_challenge_model(src, new_game.id, challenge_attachment_id, workload_spec)
                .insert(transaction)
                .await?;
        challenge_id_map.insert(src.id, challenge.id);

        for flag in &src.flags {
            let attachment_id = import_attachment(
                st,
                transaction,
                entries,
                flag.attachment_type,
                flag.file_hash.as_deref(),
                flag.remote_url.as_deref(),
                flag.file_name.as_deref(),
                stored_hashes,
            )
            .await?;
            flag_context::ActiveModel {
                flag: Set(flag.flag.clone()),
                is_occupied: Set(false),
                challenge_id: Set(Some(challenge.id)),
                attachment_id: Set(attachment_id),
                ..Default::default()
            }
            .insert(transaction)
            .await?;
        }
    }

    for source in &export_game.divisions {
        let imported = division::ActiveModel {
            game_id: Set(new_game.id),
            name: Set(source.name.clone()),
            invite_code: Set(source.invite_code.clone()),
            default_permissions: Set(source.default_permissions),
            ..Default::default()
        }
        .insert(transaction)
        .await?;

        for config in &source.challenge_configs {
            let Some(&challenge_id) = challenge_id_map.get(&config.challenge_id) else {
                continue;
            };
            division_challenge_config::ActiveModel {
                division_id: Set(imported.id),
                challenge_id: Set(challenge_id),
                permissions: Set(config.permissions),
            }
            .insert(transaction)
            .await?;
        }
    }

    Ok(new_game.id)
}

fn imported_game_model(
    source: &ExportGameModel,
    public_key: String,
    private_key: String,
) -> game::ActiveModel {
    game::ActiveModel {
        title: Set(source.title.clone()),
        public_key: Set(public_key),
        private_key: Set(private_key),
        hidden: Set(true),
        practice_mode: Set(false),
        summary: Set(source.summary.clone()),
        content: Set(source.content.clone()),
        accept_without_review: Set(source.accept_without_review),
        allow_user_submissions: Set(source.allow_user_submissions),
        writeup_required: Set(source.writeup_required),
        invite_code: Set(None),
        team_member_count_limit: Set(source.team_member_count_limit),
        discord_webhook: Set(source.discord_webhook.clone()),
        container_count_limit: Set(source.container_count_limit),
        start_time_utc: Set(source.start_time_utc),
        end_time_utc: Set(source.end_time_utc),
        writeup_deadline: Set(source.writeup_deadline),
        freeze_time_utc: Set(source.freeze_time_utc),
        writeup_note: Set(source.writeup_note.clone()),
        blood_bonus_value: Set(blood_bonus_from_value(source.blood_bonus_value)),
        // Posters are direct hash owners rather than attachment rows. Archives
        // do not bundle/refcount them, so retaining this hash would create a
        // deployment-local dangling reference.
        poster_hash: Set(None),
        ad_warmup_seconds: Set(source.ad_warmup_seconds),
        ad_tick_seconds: Set(source.ad_tick_seconds),
        ad_flag_lifetime_ticks: Set(source.ad_flag_lifetime_ticks),
        ad_reset_cooldown_minutes: Set(source.ad_reset_cooldown_minutes),
        ad_getflag_window_fraction: Set(source.ad_getflag_window_fraction),
        ad_min_grace_period_seconds: Set(source.ad_min_grace_period_seconds),
        ad_allow_snapshot_download: Set(source.ad_allow_snapshot_download.unwrap_or(true)),
        ad_snapshot_retention_days: Set(source.ad_snapshot_retention_days),
        ad_epoch_ticks: Set(source.ad_epoch_ticks),
        koth_epoch_ticks: Set(source.koth_epoch_ticks),
        koth_cycle_ticks: Set(source.koth_cycle_ticks),
        koth_champion_cooldown_ticks: Set(source.koth_champion_cooldown_ticks),
        koth_claim_confirmation_ticks: Set(source.koth_claim_confirmation_ticks),
        ad_scoring_start_round: Set(None),
        ad_scoring_paused: Set(false),
        ..Default::default()
    }
}

fn imported_challenge_model(
    source: &ExportChallengeModel,
    game_id: i32,
    attachment_id: Option<i32>,
    workload_spec: Option<JsonValue>,
) -> game_challenge::ActiveModel {
    game_challenge::ActiveModel {
        game_id: Set(game_id),
        attachment_id: Set(attachment_id),
        title: Set(source.title.clone()),
        content: Set(source.content.clone()),
        category: Set(source.category),
        challenge_type: Set(source.challenge_type),
        hints: Set(source.hints.clone()),
        flag_template: Set(source.flag_template.clone()),
        file_name: Set(source.file_name.clone()),
        container_image: Set(source.container_image.clone()),
        memory_limit: Set(source.memory_limit),
        storage_limit: Set(source.storage_limit),
        cpu_count: Set(source.cpu_count),
        expose_port: Set(source.expose_port),
        workload_spec: Set(workload_spec),
        deadline_utc: Set(source.deadline_utc),
        enable_traffic_capture: Set(source.enable_traffic_capture),
        enable_shared_container: Set(source.challenge_type == ChallengeType::StaticContainer
            && source.enable_shared_container),
        disable_blood_bonus: Set(source.disable_blood_bonus),
        original_score: Set(source.original_score),
        min_score_rate: Set(source.min_score_rate),
        difficulty: Set(source.difficulty),
        score_curve: Set(source.score_curve),
        submission_limit: Set(source.submission_limit),
        is_enabled: Set(false),
        accepted_count: Set(0),
        submission_count: Set(0),
        review_status: Set(ChallengeReviewStatus::Active),
        build_status: Set(ChallengeBuildStatus::None),
        // Checker paths/images are deployment-local executable references. An
        // imported template must be explicitly configured and rebuilt here.
        ad_checker_image: Set(None),
        ad_allow_egress: Set(source.ad_allow_egress),
        ad_allow_self_reset: Set(source.ad_allow_self_reset),
        ad_ssh_requires_flag: Set(source.ad_ssh_requires_flag),
        ad_self_hosted: Set(source.ad_self_hosted),
        ad_scoring_weight: Set(source.ad_scoring_weight),
        ..Default::default()
    }
}

#[allow(clippy::too_many_arguments)]
async fn import_attachment(
    st: &SharedState,
    transaction: &DatabaseTransaction,
    entries: &BTreeMap<String, Vec<u8>>,
    file_type: Option<FileType>,
    file_hash: Option<&str>,
    remote_url: Option<&str>,
    file_name: Option<&str>,
    stored_hashes: &mut BTreeSet<String>,
) -> AppResult<Option<i32>> {
    let (file_type, remote_url, local_file_id) = match file_type.unwrap_or(FileType::None) {
        FileType::None => return Ok(None),
        FileType::Remote => (
            FileType::Remote,
            Some(validate_remote_attachment_url(
                remote_url.unwrap_or_default(),
            )?),
            None,
        ),
        FileType::Local => {
            let Some(hash) = file_hash.filter(|hash| !hash.is_empty()) else {
                return Ok(None);
            };
            let Some(bytes) = valid_bundled_blob(entries, hash) else {
                // Never link an attachment to an unavailable deployment-local
                // blob merely because another database happens to know its hash.
                return Ok(None);
            };
            let name = file_name.unwrap_or(hash);
            // Record cleanup intent before storage runs: a metadata-upsert
            // failure happens after the physical content-addressed write.
            stored_hashes.insert(hash.to_owned());
            let (_, file_id) = crate::services::blob_refs::store_and_acquire_in_seaorm_transaction(
                st.storage.as_ref(),
                transaction,
                name,
                bytes,
            )
            .await?;
            (FileType::Local, None, Some(file_id))
        }
    };

    let attachment = attachment::ActiveModel {
        file_type: Set(file_type),
        remote_url: Set(remote_url),
        local_file_id: Set(local_file_id),
        ..Default::default()
    }
    .insert(transaction)
    .await?;
    Ok(Some(attachment.id))
}

fn valid_bundled_blob<'a>(entries: &'a BTreeMap<String, Vec<u8>>, hash: &str) -> Option<&'a [u8]> {
    entries
        .get(&format!("files/{hash}"))
        .filter(|bytes| crate::utils::codec::sha256_hex(bytes) == hash)
        .map(Vec::as_slice)
}

async fn purge_rolled_back_blobs(st: &SharedState, hashes: BTreeSet<String>) {
    for hash in hashes {
        if let Err(error) =
            crate::services::blob_refs::purge_if_unreferenced(st.pg(), st.storage.as_ref(), &hash)
                .await
        {
            tracing::warn!(%error, %hash, "could not clean a rolled-back game-import blob");
        }
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::ActiveValue::Set;

    use super::*;

    #[test]
    fn import_clears_unbundled_direct_hash_and_executable_references() {
        let game: ExportGameModel = serde_json::from_value(serde_json::json!({
            "posterHash": "deployment-a-poster"
        }))
        .unwrap();
        let imported_game = imported_game_model(&game, "public".into(), "private".into());
        assert_eq!(imported_game.poster_hash, Set(None));

        let challenge: ExportChallengeModel = serde_json::from_value(serde_json::json!({
            "adCheckerImage": "/data/files/checkers/other-game/checker"
        }))
        .unwrap();
        let imported_challenge = imported_challenge_model(&challenge, 7, None, None);
        assert_eq!(imported_challenge.ad_checker_image, Set(None));
    }

    #[test]
    fn local_attachments_require_hash_verified_archive_bytes() {
        let bytes = b"portable attachment".to_vec();
        let hash = crate::utils::codec::sha256_hex(&bytes);
        let mut entries = BTreeMap::new();
        assert!(valid_bundled_blob(&entries, &hash).is_none());

        entries.insert(format!("files/{hash}"), b"tampered".to_vec());
        assert!(valid_bundled_blob(&entries, &hash).is_none());

        entries.insert(format!("files/{hash}"), bytes.clone());
        assert_eq!(valid_bundled_blob(&entries, &hash), Some(bytes.as_slice()));
    }
}
