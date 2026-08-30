//! edit: test containers + imports (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;

mod archive;
pub(super) mod import_jobs;
mod import_policy;
mod path;
use archive::persist_challenge_archive;
#[cfg(test)]
use archive::{extract_zip_with_limits, ArchiveLimits};
use import_policy::validate_import_batch;
use path::{resolve_subpath, validate_subpath};

const MAX_PENDING_CHALLENGES_PER_USER_GAME: i64 = 10;
const TEST_IMAGE_PREPARE_ATTEMPTS: usize = 3;
pub use import_jobs::start_worker as start_import_job_worker;

const PENDING_CHALLENGE_COUNT_SQL: &str = r#"SELECT COUNT(*)
      FROM "GameChallenges"
     WHERE game_id = $1
       AND submitted_by_user_id = $2
       AND review_status = $3"#;

/// Materialize or repair the exact immutable image before the admin preview
/// takes its provisioning lock. Safe image cleanup deliberately leaves the
/// authoritative source archive behind, so a successful-but-pruned local
/// image follows the same first-demand repair path as a player container.
async fn prepare_test_container_image(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<()> {
    for _ in 0..TEST_IMAGE_PREPARE_ATTEMPTS {
        let candidate = load_challenge(st, game_id, challenge_id).await?;
        if !candidate.challenge_type.is_container() {
            return Err(AppError::bad_request(
                "Container creation is not allowed for this challenge",
            ));
        }
        if crate::controllers::game::prepare_queued_image(st, &candidate).await? {
            continue;
        }

        // Snapshot the published immutable identity under the definition
        // fence, then release it before a potentially slow Docker rebuild.
        let definition_lock = crate::services::challenge_workloads::acquire_definition_lock(
            st.pg(),
            game_id,
            challenge_id,
        )
        .await?;
        let snapshot: AppResult<_> = async {
            let challenge = load_challenge(st, game_id, challenge_id).await?;
            if !challenge.challenge_type.is_container() {
                return Err(AppError::bad_request(
                    "Container creation is not allowed for this challenge",
                ));
            }
            let runtime = crate::services::challenge_workloads::resolve_runtime(st, &challenge)?;
            Ok((challenge, runtime.legacy_image))
        }
        .await;
        let released = definition_lock.release().await;
        let (challenge, legacy_image) = snapshot?;
        released?;

        let Some(image) = legacy_image.as_deref() else {
            return Ok(());
        };
        if crate::controllers::game::repair_missing_legacy_image(st, &challenge, image).await? {
            // Repair can publish a new immutable ID. Reload rather than
            // launching the stale ID captured above.
            continue;
        }
        if crate::services::image_storage::reserve_runtime_image(st, &challenge, image).await?
            == crate::services::image_storage::RuntimeImageReservation::Missing
        {
            // Cleanup won the image lock just before this reservation.
            continue;
        }
        return Ok(());
    }
    Err(AppError::unavailable(
        "The repaired challenge image could not be verified on this container host.",
    ))
}

/// Spawn and persist the challenge's throwaway test container.
pub async fn create_test_container(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<ContainerInfoModel>> {
    manager_or_admin(&st, &user, id).await?;

    // Trusted repository imports may leave recoverable images queued or later
    // prune a successful daemon-local image. Materialize/repair it before
    // retaining the provisioning/definition connections: Docker builds can be
    // slow, and the build publication path needs the definition lock itself.
    prepare_test_container_image(&st, id, c_id).await?;

    let lock_key = format!("test-containers-game:{id}");
    let _local = crate::utils::single_flight::coalesce(&lock_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &lock_key)
            .await?;
    let definition_lock =
        crate::services::challenge_workloads::acquire_definition_lock(st.pg(), id, c_id).await?;
    super::challenges::reject_pending_mutation(st.pg(), id, c_id).await?;
    let mut challenge = load_challenge(&st, id, c_id).await?;
    // Revalidate the type after the build and definition-lock wait; an editor
    // may have replaced the candidate while the slow image work was running.
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

    let game_kind = crate::services::container::game_kind_for_challenge(challenge.challenge_type);
    let platform_proxy =
        crate::controllers::admin::container_port_mapping(&st).await == "PlatformProxy";
    let is_proxy = crate::services::container::should_use_platform_proxy(
        game_kind,
        st.containers.requires_proxy(),
        platform_proxy,
        false,
    );
    let container_uuid = Uuid::new_v4();
    let operation_id = Some(format!("container:{container_uuid}"));
    let info = match workload {
        Some(spec) => {
            st.containers
                .create_workload(spec, operation_id, flag.clone(), is_proxy)
                .await?
        }
        None => {
            st.containers
                .create(ContainerSpec {
                    game_kind,
                    image: legacy_image
                        .clone()
                        .expect("a legacy definition has an immutable launch image"),
                    memory_limit: challenge.memory_limit.unwrap_or(64),
                    cpu_count: challenge.cpu_count.unwrap_or(1),
                    storage_limit: crate::services::container::storage_limit_or_default(
                        challenge.storage_limit,
                    ),
                    expose_port: challenge.expose_port.unwrap_or(80),
                    publish_port: true,
                    proxy_only: is_proxy,
                    env: Vec::new(),
                    flag: flag.clone(),
                    ad_network: None,
                    allow_egress: challenge
                        .network_mode
                        .unwrap_or(crate::utils::enums::NetworkMode::Open)
                        == crate::utils::enums::NetworkMode::Open,
                    control_plane_callback_ports: Vec::new(),
                    network_mode: challenge
                        .network_mode
                        .unwrap_or(crate::utils::enums::NetworkMode::Open),
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
            ad_team_service_id: Set(None),
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
        &st,
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
/// A first-seen source manifest is counted as `imported`; recovery of the same
/// durable source identity is counted as `updated`. `messages` collects one line
/// per failed manifest, prefixed with the manifest's parent directory name.
#[derive(Debug, Default, Serialize, Deserialize)]
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
    pub operation_id: Option<Uuid>,
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
pub(super) async fn import_from_dir(
    st: &SharedState,
    game_id: i32,
    dir: &std::path::Path,
    policy: crate::services::git_sync::ImportPolicy,
    source_revision: &str,
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
    let pending_submitter = match policy {
        crate::services::git_sync::ImportPolicy::PendingReview {
            submitted_by_user_id,
        } => Some(submitted_by_user_id),
        crate::services::git_sync::ImportPolicy::Trusted => None,
    };
    if let Err(error) = validate_import_batch(policy, manifests.len()) {
        result.failed = manifests.len() as i32;
        result.messages.push(error.to_string());
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
    if let Some(submitted_by_user_id) = pending_submitter {
        let pending_count = sqlx::query_scalar::<_, i64>(PENDING_CHALLENGE_COUNT_SQL)
            .bind(game_id)
            .bind(submitted_by_user_id)
            .bind(ChallengeReviewStatus::Pending as i16)
            .fetch_one(&mut **configuration_lock.transaction_mut())
            .await;
        match pending_count {
            Ok(count) if count < MAX_PENDING_CHALLENGES_PER_USER_GAME => {}
            Ok(_) => {
                result.failed = manifests.len() as i32;
                result.messages.push(format!(
                    "At most {MAX_PENDING_CHALLENGES_PER_USER_GAME} pending challenges may be submitted per user and game."
                ));
                let _ = configuration_lock.release().await;
                return result;
            }
            Err(error) => {
                result.failed = manifests.len() as i32;
                result.messages.push(error.to_string());
                let _ = configuration_lock.release().await;
                return result;
            }
        }
    }
    let mut build_jobs = Vec::new();
    let mut generator_build_jobs = Vec::new();
    let mut archive_jobs = Vec::new();
    for manifest in manifests {
        let relative = manifest
            .strip_prefix(dir)
            .unwrap_or(manifest.as_path())
            .to_string_lossy()
            .replace('\\', "/");
        let source_identity = format!(
            "import/{}",
            crate::utils::codec::sha256_hex(format!("{source_revision}\0{relative}").as_bytes())
        );
        match crate::services::git_sync::import_manifest_with_source_identity(
            st,
            game_id,
            &manifest,
            policy,
            &source_identity,
        )
        .await
        {
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
                if imported.generator_build_queued {
                    generator_build_jobs.push(imported.challenge_id);
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
    let container_policy = if build_jobs.is_empty() {
        None
    } else {
        match crate::services::container_policy::ContainerPolicy::load(st.pg()).await {
            Ok(policy) => Some(policy),
            Err(error) => {
                result.failed += build_jobs.len() as i32;
                result
                    .messages
                    .push(format!("container policy read failed: {error}"));
                return result;
            }
        }
    };
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
        if matches!(policy, crate::services::git_sync::ImportPolicy::Trusted)
            && container_policy.as_ref().is_some_and(|policy| {
                crate::services::image_storage::lazy_build_eligible(policy, &challenge)
            })
        {
            continue;
        }
        // Durable import retries are an ensure operation. The cross-replica
        // build lock rechecks an already-published immutable image and avoids
        // rebuilding the same recovered source revision.
        let outcome = ensure_challenge_image(st, &challenge).await;
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
    for challenge_id in generator_build_jobs {
        if let Err(error) = run_import_variant_generator_build(st, challenge_id).await {
            result.failed += 1;
            result.messages.push(format!(
                "challenge #{challenge_id}: import generator build failed: {error}"
            ));
        }
    }
    if result.imported > 0 || result.updated > 0 {
        flush_game_scoreboards(st, game_id).await;
    }
    result
}

/// Read the bounded archive and optional idempotency identity. Older clients
/// that omit `operationId` remain supported with a server-generated identity.
async fn read_archive_fields(multipart: &mut Multipart) -> AppResult<(Vec<u8>, Uuid)> {
    let mut archive = None;
    let mut operation_id = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("multipart error: {e}")))?
    {
        match field.name() {
            Some("operationId") => {
                let value = field
                    .text()
                    .await
                    .map_err(|e| AppError::bad_request(format!("invalid operationId: {e}")))?;
                operation_id = Some(
                    Uuid::parse_str(value.trim())
                        .map_err(|_| AppError::bad_request("operationId must be a UUID"))?,
                );
            }
            Some("archive") | None if archive.is_none() => {
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| AppError::bad_request(format!("could not read archive: {e}")))?;
                if !bytes.is_empty() {
                    if bytes.len() > crate::utils::upload::ARCHIVE_FILE_BYTES {
                        return Err(AppError::bad_request("Challenge archive is too large"));
                    }
                    archive = Some(bytes.to_vec());
                }
            }
            _ => {}
        }
    }
    Ok((
        archive.ok_or_else(|| AppError::bad_request("No archive file provided"))?,
        operation_id.unwrap_or_else(Uuid::new_v4),
    ))
}

/// Validate the lexical shape of an optional repository subpath before cloning.
/// Canonical containment is checked separately after the checkout exists.
/// `POST /api/edit/games/{id}/challenges/submit` — user-submitted challenge
/// archive. Mirrors RSCTF `EditController.SubmitChallenge` ([RequireUser] +
/// `game.AllowUserSubmissions`): ANY logged-in user may submit, so this is gated
/// on `CurrentUser` (not `AdminUser`). The uploaded ZIP is extracted and each
/// discovered `challenge.yml` is imported under the game.
pub async fn submit_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    mut multipart: Multipart,
) -> AppResult<Response> {
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
    let upload_reservation =
        match crate::utils::upload::reserve_buffered(crate::utils::upload::ARCHIVE_BODY_BYTES) {
            Ok(reservation) => reservation,
            Err(AppError::ServiceUnavailable(_)) => return Ok(import_jobs::busy()),
            Err(error) => return Err(error),
        };
    // Buffer and validate the bounded upload before occupying the scarce import
    // worker; a slow client must not monopolize submission capacity.
    let (bytes, operation_id) = read_archive_fields(&mut multipart).await?;
    import_jobs::enqueue_zip(
        &st,
        id,
        user.id,
        operation_id,
        bytes,
        crate::services::git_sync::ImportPolicy::PendingReview {
            submitted_by_user_id: user.id,
        },
        upload_reservation,
    )
    .await
}

/// `POST /api/edit/games/{id}/challenges/import` — admin/game-admin ZIP import
/// (auto-approves). Mirrors RSCTF `EditController.ImportChallenge`.
pub async fn import_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    mut multipart: Multipart,
) -> AppResult<Response> {
    manager_or_admin(&st, &user, id).await?;
    let upload_reservation =
        match crate::utils::upload::reserve_buffered(crate::utils::upload::ARCHIVE_BODY_BYTES) {
            Ok(reservation) => reservation,
            Err(AppError::ServiceUnavailable(_)) => return Ok(import_jobs::busy()),
            Err(error) => return Err(error),
        };
    let (bytes, operation_id) = read_archive_fields(&mut multipart).await?;
    import_jobs::enqueue_zip(
        &st,
        id,
        user.id,
        operation_id,
        bytes,
        crate::services::git_sync::ImportPolicy::Trusted,
        upload_reservation,
    )
    .await
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
) -> AppResult<Response> {
    manager_or_admin(&st, &user, id).await?;

    let repo_url = crate::services::git_sync::validate_github_repo_url(&model.repo_url)?;
    let git_ref = crate::services::git_sync::validate_git_ref(model.git_ref.as_deref())?;
    let subpath = validate_subpath(model.subpath.as_deref())?;
    import_jobs::enqueue_git(
        &st,
        id,
        user.id,
        model.operation_id.unwrap_or_else(Uuid::new_v4),
        import_jobs::GitImportSource {
            repo_url,
            git_ref,
            subpath,
            token: model.github_token.unwrap_or_default(),
        },
    )
    .await
}

#[cfg(test)]
mod tests;
