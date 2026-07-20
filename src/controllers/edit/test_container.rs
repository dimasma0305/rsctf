//! edit: test containers + imports (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;

const MAX_ARCHIVE_ENTRIES: usize = 2_048;
const MAX_ARCHIVE_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ARCHIVE_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ARCHIVE_COMPRESSION_RATIO: u64 = 200;
const MAX_ARCHIVE_PATH_COMPONENTS: usize = 32;

/// Spawn and persist the challenge's throwaway test container.
pub async fn create_test_container(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<ContainerInfoModel>> {
    manager_or_admin(&st, &user, id).await?;
    let lock_key = format!("test-containers-game:{id}");
    let _local = crate::utils::single_flight::coalesce(&lock_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &lock_key)
            .await?;
    let definition_lock =
        crate::services::challenge_workloads::acquire_definition_lock(st.pg(), id, c_id).await?;
    super::challenges::reject_pending_mutation(st.pg(), id, c_id).await?;
    let mut challenge = load_challenge(&st, id, c_id).await?;
    if !challenge.challenge_type.is_container() {
        return Err(AppError::bad_request(
            "Container creation is not allowed for this challenge",
        ));
    }
    let runtime = crate::services::challenge_workloads::resolve_runtime(&st, &challenge)?;
    let workload = runtime.workload;
    let identity = runtime.identity;
    let publication_fence = runtime.publication_fence;
    let legacy_image = runtime.legacy_image;
    definition_lock.release().await?;

    // Re-read under the cross-replica lock; clear stale pointers before replacement.
    if let Some(cuuid) = challenge.test_container_id {
        if let Some(c) = container::Entity::find_by_id(cuuid).one(&st.db).await? {
            if crate::services::challenge_workloads::existing_runtime_is_reusable(
                st.containers.as_ref(),
                &c.container_id,
                &c.image,
                &identity,
                legacy_image.is_some(),
            )
            .await?
            {
                distributed.release().await?;
                return Ok(RequestResponse::ok(ContainerInfoModel::from(&c)));
            }
            super::helpers::destroy_test_container_with(
                st.pg(),
                c_id,
                cuuid,
                &c.container_id,
                super::helpers::revoke_and_destroy_backend(&st, &c.container_id),
            )
            .await?;
        } else {
            sqlx::query(
                r#"UPDATE "GameChallenges" SET test_container_id = NULL
                    WHERE id = $1 AND test_container_id = $2"#,
            )
            .bind(c_id)
            .bind(cuuid)
            .execute(st.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        }
        challenge.test_container_id = None;
    }

    let selected_static_flag = crate::services::challenge_workloads::load_selected_static_flag(
        st.pg(),
        c_id,
        challenge.challenge_type,
    )
    .await?;
    // A DynamicContainer bakes a throwaway flag into the environment; other
    // runtime modes mirror their currently-selected static flag.
    let flag = if challenge.challenge_type == ChallengeType::DynamicContainer {
        let seed = sha256_str(&Uuid::new_v4().to_string());
        Some(flag_generator::generate_flag(
            challenge.flag_template.as_deref(),
            &seed,
        ))
    } else {
        selected_static_flag.clone()
    };

    let container_uuid = Uuid::new_v4();
    let operation_id = Some(format!("container:{container_uuid}"));
    let info = match workload {
        Some(spec) => {
            st.containers
                .create_workload(spec, operation_id, flag.clone())
                .await?
        }
        None => {
            st.containers
                .create(ContainerSpec {
                    game_kind: crate::services::container::game_kind_for_challenge(
                        challenge.challenge_type,
                    ),
                    image: legacy_image
                        .clone()
                        .expect("a legacy definition has an immutable launch image"),
                    memory_limit: challenge.memory_limit.unwrap_or(64),
                    cpu_count: challenge.cpu_count.unwrap_or(1),
                    expose_port: challenge.expose_port.unwrap_or(80),
                    env: Vec::new(),
                    flag: flag.clone(),
                    ad_network: None,
                    allow_egress: true,
                    operation_id,
                })
                .await?
        }
    };

    let backend_id = info.id.clone();
    let fenced = async {
        let mut lock =
            crate::services::challenge_workloads::acquire_definition_lock(st.pg(), id, c_id)
                .await?;
        // Persistence below uses its own transaction; take the canonical fence
        // snapshot through the pool so this guard does not self-block that write.
        super::challenges::reject_pending_mutation(st.pg(), id, c_id).await?;
        let current = load_challenge(&st, id, c_id).await?;
        let current_runtime = crate::services::challenge_workloads::resolve_runtime(&st, &current)?;
        crate::services::challenge_workloads::ensure_definition_unchanged(
            &publication_fence,
            &current_runtime.publication_fence,
        )?;
        crate::services::challenge_workloads::ensure_selected_static_flag_current(
            &mut lock,
            c_id,
            selected_static_flag.as_deref(),
        )
        .await?;
        Ok::<_, AppError>((lock, current))
    }
    .await;
    let (definition_lock, challenge) = match fenced {
        Ok(value) => value,
        Err(error) => {
            if let Err(destroy_error) = st.containers.destroy(&backend_id).await {
                tracing::warn!(%backend_id, error = %destroy_error, "unpublished stale-definition test container destroy failed");
            }
            distributed.release().await?;
            return Err(error);
        }
    };
    let now = Utc::now();
    let stop_at = now + chrono::Duration::hours(2);
    // PlatformProxy returns the wsrx proxy guid; Default returns host:port.
    let is_proxy = st.containers.requires_proxy()
        || crate::controllers::admin::container_port_mapping(&st).await == "PlatformProxy";
    let persisted: AppResult<container::Model> = async {
        let txn = crate::utils::database::begin_seaorm_transaction(&st.db).await?;
        let c = container::ActiveModel {
            id: Set(container_uuid),
            image: Set(identity),
            container_id: Set(info.id),
            status: Set(ContainerStatus::Running),
            started_at: Set(now),
            expect_stop_at: Set(stop_at),
            is_proxy: Set(is_proxy),
            ip: Set(info.ip),
            port: Set(info.port),
            public_ip: Set(None),
            public_port: Set(None),
            game_instance_id: Set(None),
            exercise_instance_id: Set(None),
        }
        .insert(&txn)
        .await?;

        let mut am: game_challenge::ActiveModel = challenge.into();
        am.test_container_id = Set(Some(container_uuid));
        am.update(&txn).await?;
        txn.commit().await?;
        Ok(c)
    }
    .await;
    definition_lock.release().await?;
    let c = match persisted {
        Ok(c) => c,
        Err(err) => {
            super::helpers::destroy_test_container_with(
                st.pg(),
                c_id,
                container_uuid,
                &backend_id,
                super::helpers::revoke_and_destroy_backend(&st, &backend_id),
            )
            .await?;
            return Err(err);
        }
    };
    distributed.release().await?;

    let log_id = format!("<{}> {}", &c.id.simple().to_string()[..12], c.container_id);
    crate::services::audit::info(
        &st.db,
        "EditController",
        Some(user.name.clone()),
        None,
        format!("Successfully created test container [{log_id}]"),
    )
    .await;

    Ok(RequestResponse::ok(ContainerInfoModel::from(&c)))
}

/// `DELETE /api/edit/games/{id}/challenges/{cId}/container` — void. Tear down
/// the challenge's test container. Mirrors `EditController.DestroyTestContainer`.
pub async fn destroy_test_container(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id)): Path<(i32, i32)>,
) -> AppResult<MessageResponse> {
    manager_or_admin(&st, &user, id).await?;
    let lock_key = format!("test-containers-game:{id}");
    let _local = crate::utils::single_flight::coalesce(&lock_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &lock_key)
            .await?;
    let challenge = load_challenge(&st, id, c_id).await?;
    let Some(cuuid) = challenge.test_container_id else {
        distributed.release().await?;
        return Ok(MessageResponse::ok(""));
    };

    let teardown: AppResult<()> = async {
        if let Some(c) = container::Entity::find_by_id(cuuid).one(&st.db).await? {
            super::helpers::destroy_test_container_with(
                st.pg(),
                c_id,
                cuuid,
                &c.container_id,
                super::helpers::revoke_and_destroy_backend(&st, &c.container_id),
            )
            .await?;
        } else {
            sqlx::query(
                r#"UPDATE "GameChallenges" SET test_container_id = NULL
                    WHERE id = $1 AND test_container_id = $2"#,
            )
            .bind(c_id)
            .bind(cuuid)
            .execute(st.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        }
        Ok(())
    }
    .await;
    let released = distributed.release().await;
    teardown?;
    released?;

    Ok(MessageResponse::ok(""))
}

/// Result of a challenge import (uploaded archive or GitHub clone), consumed by
/// the frontend challenge-management contract. Serialized raw (camelCase).
///
/// Because `git_sync::import_manifest` is create-only (a fresh INSERT per
/// manifest, never an update), every successful manifest is counted as
/// `imported`; `updated`/`skipped` stay 0. `messages` collects one line per failed
/// manifest, prefixed with the manifest's parent directory name.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeImportResult {
    pub imported: i32,
    pub updated: i32,
    pub skipped: i32,
    pub failed: i32,
    pub messages: Vec<String>,
}

/// RSCTF `Models/Request/Edit/ImportFromGitHubModel` — the JSON body of the
/// github bulk-import endpoint. `ref` is a branch/tag; `subpath` scopes discovery
/// to a subdirectory of the clone; `githubToken` authenticates a private repo.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportFromGitHubModel {
    #[serde(default)]
    pub repo_url: String,
    #[serde(default, rename = "ref")]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub subpath: Option<String>,
    #[serde(default)]
    pub github_token: Option<String>,
}

/// Discover every `challenge.yml`/`challenge.yaml` under `dir` and upsert each one
/// under `game_id`, tallying the outcome. Port of the discover→import half of
/// RSCTF `ChallengeImportService`. Never fails: a per-manifest error is recorded
/// as a `failed` count + a `messages` line (prefixed with the manifest's parent
/// directory) rather than aborting the whole import.
async fn import_from_dir(
    st: &SharedState,
    game_id: i32,
    dir: &std::path::Path,
    auto_approve: bool,
) -> ChallengeImportResult {
    let mut result = ChallengeImportResult::default();
    let manifests = match crate::services::git_sync::discover_challenges(dir).await {
        Ok(m) => m,
        Err(e) => {
            result.messages.push(e.to_string());
            return result;
        }
    };
    if manifests.is_empty() {
        return result;
    }
    let mut configuration_lock =
        match crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await {
            Ok(lock) => lock,
            Err(error) => {
                result.failed = manifests.len() as i32;
                result.messages.push(error.to_string());
                return result;
            }
        };
    match ad_epoch_scoring_started_locked(configuration_lock.transaction_mut(), game_id).await {
        Ok(false) => {}
        Ok(true) => {
            result.failed = manifests.len() as i32;
            result.messages.push(
                "Challenge import is locked after A&D epoch scoring has started.".to_string(),
            );
            return result;
        }
        Err(error) => {
            result.failed = manifests.len() as i32;
            result.messages.push(error.to_string());
            return result;
        }
    }
    let policy = if auto_approve {
        crate::services::git_sync::ImportPolicy::Trusted
    } else {
        crate::services::git_sync::ImportPolicy::PendingReview
    };
    let mut build_jobs = Vec::new();
    let mut archive_jobs = Vec::new();
    for manifest in manifests {
        match crate::services::git_sync::import_manifest(st, game_id, &manifest, policy).await {
            Ok(imported) => {
                if imported.created {
                    result.imported += 1;
                } else {
                    result.updated += 1;
                }
                // Pending submissions already persisted their complete source
                // archive before INSERT; approval depends on that immutable blob.
                // Trusted imports retain the historical best-effort audit copy.
                if matches!(policy, crate::services::git_sync::ImportPolicy::Trusted) {
                    archive_jobs.push((imported.challenge_id, manifest.clone()));
                }
                if imported.build_queued {
                    build_jobs.push(imported.challenge_id);
                }
                if imported.runtime_update_deferred {
                    result.failed += 1;
                    result.messages.push(format!(
                        "challenge #{}: the enabled live runtime was retained because imported runtime equivalence differs or could not be verified; disable, sync/build, then re-enable",
                        imported.challenge_id
                    ));
                }
                if imported.grading_update_deferred {
                    result.failed += 1;
                    result.messages.push(format!(
                        "challenge #{}: grading/scoring changes were retained because the Jeopardy game has started or accepted evidence exists",
                        imported.challenge_id
                    ));
                }
                if !imported.attachment_synced {
                    result.failed += 1;
                    result.messages.push(format!(
                        "challenge #{}: attachment synchronization failed",
                        imported.challenge_id
                    ));
                }
            }
            Err(e) => {
                result.failed += 1;
                let dir_name = manifest
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                    .unwrap_or("challenge");
                result.messages.push(format!("{dir_name}: {e}"));
            }
        }
    }
    if let Err(error) = configuration_lock.release().await {
        result
            .messages
            .push(format!("challenge import unlock failed: {error}"));
    }
    for (challenge_id, manifest) in archive_jobs {
        persist_challenge_archive(st, challenge_id, &manifest).await;
    }
    for challenge_id in build_jobs {
        let challenge = match game_challenge::Entity::find_by_id(challenge_id)
            .one(&st.db)
            .await
        {
            Ok(Some(challenge)) => challenge,
            Ok(None) => {
                result.failed += 1;
                result.messages.push(format!(
                    "challenge #{challenge_id}: disappeared before build"
                ));
                continue;
            }
            Err(error) => {
                result.failed += 1;
                result.messages.push(format!(
                    "challenge #{challenge_id}: build lookup failed: {error}"
                ));
                continue;
            }
        };
        let (outcome, _) = run_challenge_build(st, &challenge, "Import", 1).await;
        if outcome.status != ChallengeBuildStatus::Success {
            result.failed += 1;
            result.messages.push(format!(
                "challenge #{challenge_id}: import build failed: {}",
                outcome
                    .log
                    .unwrap_or_else(|| format!("{:?}", outcome.status))
            ));
        }
    }
    if result.imported > 0 || result.updated > 0 {
        flush_game_scoreboards(st, game_id).await;
    }
    result
}

/// Extract every ZIP entry from `bytes` into `dest`. Zip-slip safe: an entry with
/// a noncanonical path or a path that would escape `dest` rejects the archive.
/// A malformed archive is a client error (400).
fn extract_zip(bytes: &[u8], dest: &std::path::Path) -> AppResult<()> {
    extract_zip_with_limits(
        bytes,
        dest,
        ArchiveLimits {
            entries: MAX_ARCHIVE_ENTRIES,
            file_bytes: MAX_ARCHIVE_FILE_BYTES,
            total_bytes: MAX_ARCHIVE_TOTAL_BYTES,
            compression_ratio: MAX_ARCHIVE_COMPRESSION_RATIO,
            path_components: MAX_ARCHIVE_PATH_COMPONENTS,
        },
    )
}

#[derive(Clone, Copy)]
struct ArchiveLimits {
    entries: usize,
    file_bytes: u64,
    total_bytes: u64,
    compression_ratio: u64,
    path_components: usize,
}

fn extract_zip_with_limits(
    bytes: &[u8],
    dest: &std::path::Path,
    limits: ArchiveLimits,
) -> AppResult<()> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| AppError::bad_request(format!("Invalid or corrupted ZIP file: {e}")))?;
    if archive.len() > limits.entries {
        return Err(AppError::bad_request("ZIP contains too many entries"));
    }

    let mut total_written = 0u64;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| AppError::bad_request(format!("ZIP read error: {e}")))?;
        let rel = crate::utils::archive::canonical_zip_entry_path(&entry)
            .ok_or_else(|| AppError::bad_request("ZIP entry path is not canonical"))?;
        if rel.components().count() > limits.path_components {
            return Err(AppError::bad_request("ZIP entry path is too deep"));
        }
        if !entry.is_dir() {
            if entry.size() > limits.file_bytes {
                return Err(AppError::bad_request("ZIP entry is too large"));
            }
            let compressed = entry.compressed_size().max(1);
            if entry.size() > compressed.saturating_mul(limits.compression_ratio) {
                return Err(AppError::bad_request(
                    "ZIP entry compression ratio is too high",
                ));
            }
            if total_written.saturating_add(entry.size()) > limits.total_bytes {
                return Err(AppError::bad_request("ZIP expands beyond the size limit"));
            }
        }
        let out = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)
                .map_err(|e| AppError::internal(format!("create dir {}: {e}", out.display())))?;
        } else {
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    AppError::internal(format!("create dir {}: {e}", parent.display()))
                })?;
            }
            let mut f = std::fs::File::create(&out)
                .map_err(|e| AppError::internal(format!("create file {}: {e}", out.display())))?;
            // Enforce the actual decompressed byte count too; do not trust only the
            // central-directory size fields from an attacker-controlled archive.
            let remaining_total = limits.total_bytes.saturating_sub(total_written);
            let max_write = limits.file_bytes.min(remaining_total);
            let written =
                std::io::copy(&mut std::io::Read::take(&mut entry, max_write + 1), &mut f)
                    .map_err(|e| {
                        AppError::internal(format!("write file {}: {e}", out.display()))
                    })?;
            if written > max_write {
                return Err(AppError::bad_request("ZIP expands beyond the size limit"));
            }
            total_written = total_written.saturating_add(written);
        }
    }
    Ok(())
}

/// Re-zip `dir` (recursively) into an in-memory ZIP, each file added under its
/// path relative to `dir`. Deflate-compressed to match the upload format the
/// audit modal re-opens.
fn zip_dir_to_bytes(dir: &std::path::Path) -> AppResult<Vec<u8>> {
    let mut zw = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    // Iterative walk (explicit stack) mirrors git_sync::discover_challenges.
    let mut files_seen = 0usize;
    let mut total_bytes = 0u64;
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((current, depth)) = stack.pop() {
        if depth > MAX_ARCHIVE_PATH_COMPONENTS {
            return Err(AppError::bad_request("challenge archive is too deep"));
        }
        let entries = std::fs::read_dir(&current)
            .map_err(|e| AppError::internal(format!("zip read_dir {}: {e}", current.display())))?;
        for entry in entries {
            let entry =
                entry.map_err(|e| AppError::internal(format!("zip read dir entry: {e}")))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|e| AppError::internal(format!("zip stat {}: {e}", path.display())))?;
            if file_type.is_dir() {
                stack.push((path, depth + 1));
            } else if file_type.is_file() {
                files_seen += 1;
                if files_seen > MAX_ARCHIVE_ENTRIES {
                    return Err(AppError::bad_request(
                        "challenge archive has too many files",
                    ));
                }
                let declared = entry
                    .metadata()
                    .map_err(|e| AppError::internal(format!("zip stat {}: {e}", path.display())))?
                    .len();
                if declared > MAX_ARCHIVE_FILE_BYTES
                    || total_bytes.saturating_add(declared) > MAX_ARCHIVE_TOTAL_BYTES
                {
                    return Err(AppError::bad_request(
                        "challenge archive exceeds the size limit",
                    ));
                }
                // Path relative to `dir`, forward-slash normalized for the archive.
                let Ok(rel) = path.strip_prefix(dir) else {
                    continue;
                };
                let name = rel.to_string_lossy().replace('\\', "/");
                let data = std::fs::read(&path)
                    .map_err(|e| AppError::internal(format!("zip read {}: {e}", path.display())))?;
                let actual = data.len() as u64;
                if actual > MAX_ARCHIVE_FILE_BYTES
                    || total_bytes.saturating_add(actual) > MAX_ARCHIVE_TOTAL_BYTES
                {
                    return Err(AppError::bad_request(
                        "challenge archive exceeds the size limit",
                    ));
                }
                total_bytes = total_bytes.saturating_add(actual);
                zw.start_file(name, opts)
                    .map_err(|e| AppError::internal(format!("zip start_file: {e}")))?;
                zw.write_all(&data)
                    .map_err(|e| AppError::internal(format!("zip write: {e}")))?;
            }
        }
    }
    let cursor = zw
        .finish()
        .map_err(|e| AppError::internal(format!("zip finish: {e}")))?;
    Ok(cursor.into_inner())
}

/// Best-effort: re-zip the imported challenge's directory (the manifest's parent),
/// store it as a blob, and record the hash on a challenge that does not already
/// have an authoritative archive. Local image imports publish their exact Docker
/// build context before building; never replace that fingerprint with a later
/// audit ZIP, because an existing Success status belongs to that exact context.
/// Never fails the caller — a zip/store/update error is logged and swallowed (the
/// challenge is already created).
async fn persist_challenge_archive(
    st: &SharedState,
    challenge_id: i32,
    manifest: &std::path::Path,
) {
    let Some(dir) = manifest.parent() else {
        return;
    };
    let dir_name = dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("challenge");
    let already_has_archive = match sqlx::query_scalar::<_, bool>(
        r#"SELECT original_archive_blob_path IS NOT NULL
             FROM "GameChallenges"
            WHERE id = $1"#,
    )
    .bind(challenge_id)
    .fetch_optional(st.pg())
    .await
    {
        Ok(Some(value)) => value,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(%error, "audit archive: preflight {dir_name} failed");
            return;
        }
    };
    if already_has_archive {
        tracing::debug!(
            challenge_id,
            "audit archive: retained authoritative build/source fingerprint"
        );
        return;
    }
    let bytes = match zip_dir_to_bytes(dir) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("audit archive: zip {dir_name} failed: {e}");
            return;
        }
    };
    let persisted: AppResult<Option<String>> = async {
        let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        let current_hash = sqlx::query_as::<_, (Option<String>,)>(
            r#"SELECT original_archive_blob_path
                 FROM "GameChallenges"
                WHERE id = $1
                FOR UPDATE"#,
        )
        .bind(challenge_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::not_found("Challenge not found"))?
        .0;
        if current_hash.is_some() {
            transaction
                .commit()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            return Ok(None);
        }
        let (blob, _) = crate::services::blob_refs::store_and_acquire_in_transaction(
            st.storage.as_ref(),
            &mut transaction,
            &format!("{dir_name}.zip"),
            &bytes,
        )
        .await?;
        sqlx::query(
            r#"UPDATE "GameChallenges"
                  SET original_archive_blob_path = $2
                WHERE id = $1"#,
        )
        .bind(challenge_id)
        .bind(&blob.hash)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        Ok(Some(blob.hash))
    }
    .await;
    match persisted {
        Ok(Some(_)) => {}
        Ok(None) => {
            tracing::debug!(
                challenge_id,
                "audit archive: retained authoritative build/source fingerprint"
            );
        }
        Err(error) => {
            tracing::warn!(%error, "audit archive: persist {dir_name} failed");
        }
    }
}

/// Read the first non-empty multipart field into memory. The client posts the ZIP
/// under `archive`; we take the first non-empty part regardless of field name.
async fn read_first_archive_field(multipart: &mut Multipart) -> AppResult<Vec<u8>> {
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("multipart error: {e}")))?
    {
        let bytes = field
            .bytes()
            .await
            .map_err(|e| AppError::bad_request(format!("could not read archive: {e}")))?;
        if !bytes.is_empty() {
            return Ok(bytes.to_vec());
        }
    }
    Err(AppError::bad_request("No archive file provided"))
}

/// Extract an uploaded ZIP into a unique temp dir, import every manifest it
/// contains under `game_id`, then always remove the temp dir (even on error).
async fn import_archive_bytes(
    st: &SharedState,
    game_id: i32,
    bytes: &[u8],
    auto_approve: bool,
) -> AppResult<ChallengeImportResult> {
    let tmp = std::env::temp_dir().join(format!("rsctf-import-{}", Uuid::new_v4()));
    // Create only the unpredictable leaf and fail on any pre-existing entry;
    // never follow a final-component symlink in a shared temporary directory.
    tokio::fs::create_dir(&tmp)
        .await
        .map_err(|e| AppError::internal(format!("create temp dir: {e}")))?;
    // Keep the fallible work in one place so the temp-dir removal below always runs
    // — a `?` here must not jump over the cleanup.
    let archive = bytes.to_vec();
    let extract_dest = tmp.clone();
    let outcome = async {
        tokio::task::spawn_blocking(move || extract_zip(&archive, &extract_dest))
            .await
            .map_err(|e| AppError::internal(format!("ZIP extraction task failed: {e}")))??;
        Ok::<_, AppError>(import_from_dir(st, game_id, &tmp, auto_approve).await)
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&tmp).await;
    outcome
}

/// Validate the lexical shape of an optional repository subpath before cloning.
/// Canonical containment is checked separately after the checkout exists.
fn validate_subpath(subpath: Option<&str>) -> AppResult<Option<std::path::PathBuf>> {
    let Some(sp) = subpath.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let rel = std::path::Path::new(sp);
    for comp in rel.components() {
        match comp {
            std::path::Component::Normal(_) | std::path::Component::CurDir => {}
            _ => return Err(AppError::bad_request("invalid subpath")),
        }
    }
    Ok(Some(rel.to_path_buf()))
}

/// Resolve a validated subpath after clone, following repository symlinks and
/// requiring the resulting directory to remain under the canonical checkout.
fn resolve_subpath(
    base: &std::path::Path,
    subpath: Option<&std::path::Path>,
) -> AppResult<std::path::PathBuf> {
    let root = std::fs::canonicalize(base)
        .map_err(|e| AppError::internal(format!("canonicalize checkout: {e}")))?;
    let candidate = match subpath {
        Some(rel) => std::fs::canonicalize(base.join(rel))
            .map_err(|_| AppError::bad_request("repository subpath does not exist"))?,
        None => root.clone(),
    };
    if !candidate.starts_with(&root) {
        return Err(AppError::bad_request(
            "repository subpath escapes the checkout",
        ));
    }
    Ok(candidate)
}

/// `POST /api/edit/games/{id}/challenges/submit` — user-submitted challenge
/// archive. Mirrors RSCTF `EditController.SubmitChallenge` ([RequireUser] +
/// `game.AllowUserSubmissions`): ANY logged-in user may submit, so this is gated
/// on `CurrentUser` (not `AdminUser`). The uploaded ZIP is extracted and each
/// discovered `challenge.yml` is imported under the game.
pub async fn submit_challenge(
    State(st): State<SharedState>,
    _user: CurrentUser,
    Path(id): Path<i32>,
    mut multipart: Multipart,
) -> AppResult<RequestResponse<ChallengeImportResult>> {
    // 404 if the game is missing (RSCTF Game_NotFound).
    let game = game::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Game not found"))?;
    // Per-game gate: admins/game-admins bypass this via the Import endpoint; this
    // public Submit path is 403'd when the game disallows user submissions.
    if !game.allow_user_submissions {
        return Err(AppError::Coded {
            http: axum::http::StatusCode::FORBIDDEN,
            code: 403,
            title: "User submissions are disabled for this game.".into(),
        });
    }
    let bytes = read_first_archive_field(&mut multipart).await?;
    let result = import_archive_bytes(&st, id, &bytes, false).await?;
    Ok(RequestResponse::ok(result))
}

/// `POST /api/edit/games/{id}/challenges/import` — admin/game-admin ZIP import
/// (auto-approves). Mirrors RSCTF `EditController.ImportChallenge`.
pub async fn import_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    mut multipart: Multipart,
) -> AppResult<RequestResponse<ChallengeImportResult>> {
    manager_or_admin(&st, &user, id).await?;
    let bytes = read_first_archive_field(&mut multipart).await?;
    let result = import_archive_bytes(&st, id, &bytes, true).await?;
    Ok(RequestResponse::ok(result))
}

/// `POST /api/edit/games/{id}/challenges/importfromgithub` — admin/game-admin
/// bulk import from a git repo. Mirrors RSCTF
/// `EditController.ImportChallengeFromGitHub`: shallow-clone the repo into a temp
/// dir, then import every discovered manifest (optionally scoped to `subpath`).
pub async fn import_from_github(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    Json(model): Json<ImportFromGitHubModel>,
) -> AppResult<RequestResponse<ChallengeImportResult>> {
    manager_or_admin(&st, &user, id).await?;

    let repo_url = crate::services::git_sync::validate_github_repo_url(&model.repo_url)?;
    // Embed the PAT for a private repo (no-op when the token is empty/absent).
    let auth_url = crate::services::git_sync::GitCredentials::new(
        model.github_token.clone().unwrap_or_default(),
    )
    .apply(&repo_url);
    let git_ref = crate::services::git_sync::validate_git_ref(model.git_ref.as_deref())?;
    let branch = git_ref.as_deref();
    let subpath = validate_subpath(model.subpath.as_deref())?;

    let tmp = std::env::temp_dir().join(format!("rsctf-import-{}", Uuid::new_v4()));
    tokio::fs::create_dir(&tmp)
        .await
        .map_err(|e| AppError::internal(format!("create temp dir: {e}")))?;

    // Fallible work in one place so the temp-dir removal always runs.
    let outcome = async {
        // A clone failure is a client error (bad URL/ref/token); the error text is
        // already credential-scrubbed by git_sync.
        crate::services::git_sync::sync_repo(&auth_url, branch, &tmp)
            .await
            .map_err(|e| AppError::bad_request(format!("git clone failed: {e}")))?;
        let scan_root = resolve_subpath(&tmp, subpath.as_deref())?;
        Ok::<_, AppError>(import_from_dir(&st, id, &scan_root, true).await)
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&tmp).await;
    let result = outcome?;
    Ok(RequestResponse::ok(result))
}

#[cfg(test)]
mod tests;
