//! Repo-binding CRUD + scan + game-creation helpers.

use super::*;
use crate::utils::enums::ChallengeBuildStatus;

mod mutations;
mod queries;
pub(crate) use mutations::{
    commit_already_applied, delete_repo_binding_record, update_bound_game_manifest_path,
    update_repo_binding_record,
};
pub use queries::{list_repo_bindings, repo_binding_scans};

/// RSCTF `RepoBindingScanResultModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoBindingScanResultModel {
    pub games_created: i32,
    pub games_updated: i32,
    pub challenges_imported: i32,
    pub challenges_updated: i32,
    pub failures: i32,
    pub messages: Vec<String>,
}

pub(crate) struct RepoBindingScanExecution {
    pub result: RepoBindingScanResultModel,
    pub ran_at: DateTime<Utc>,
    pub commit_sha: Option<String>,
    pub message: String,
}

/// RSCTF `RepoBindingInfoModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoBindingInfoModel {
    pub id: i32,
    pub repo_url: String,
    pub r#ref: Option<String>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub created_at_utc: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub last_scan_utc: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub next_scan_utc: Option<DateTime<Utc>>,
    pub interval_seconds: i32,
    pub status: String,
    pub last_commit_sha: Option<String>,
    pub last_scan_message: Option<String>,
    pub has_git_hub_token: bool,
    pub token_status: String,
    pub current_activity: Option<String>,
    pub push_on_edit: bool,
    pub push_backlog: i64,
    pub push_last_error: Option<String>,
    pub games: Vec<Value>,
}

/// `RepoBindingCreateModel` — POST /api/admin/repobindings.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoBindingCreateModel {
    pub repo_url: String,
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default)]
    pub github_token: Option<String>,
    #[serde(default)]
    pub interval_seconds: Option<i32>,
    #[serde(default)]
    pub run_immediately: Option<bool>,
}

/// `RepoBindingUpdateModel` — PUT /api/admin/repobindings/{id} (patch semantics).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoBindingUpdateModel {
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default)]
    pub interval_seconds: Option<i32>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub github_token: Option<String>,
    #[serde(default)]
    pub push_on_edit: Option<bool>,
}

/// `RepoBindingScanHistoryModel` — one past scan of a binding.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoBindingScanHistoryModel {
    pub id: i32,
    #[serde(with = "crate::utils::datetime::millis")]
    pub ran_at_utc: DateTime<Utc>,
    pub commit_sha: Option<String>,
    pub games_created: i32,
    pub games_updated: i32,
    pub challenges_imported: i32,
    pub challenges_updated: i32,
    pub failures: i32,
    pub messages: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ChallengeSyncCounts {
    imported: i32,
    updated: i32,
}

impl ChallengeSyncCounts {
    fn record(&mut self, result: crate::services::git_sync::ManifestImportResult) {
        if result.created {
            self.imported += 1;
        } else {
            self.updated += 1;
        }
    }
}

const MAX_SCAN_MESSAGES: usize = 256;
const MAX_SCAN_MESSAGE_CHARS: usize = 2_000;
const MAX_SCAN_HISTORY_CHARS: usize = 64 * 1024;

fn bounded_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn bound_scan_messages(mut messages: Vec<String>) -> Vec<String> {
    let omitted = messages.len().saturating_sub(MAX_SCAN_MESSAGES);
    messages.truncate(MAX_SCAN_MESSAGES);
    for message in &mut messages {
        *message = bounded_chars(message, MAX_SCAN_MESSAGE_CHARS);
    }
    if omitted > 0 {
        messages.push(format!("{omitted} additional scan message(s) omitted"));
    }
    messages
}

fn missing_challenge_reconciliation_is_safe(unresolved_manifests: usize) -> bool {
    unresolved_manifests == 0
}

fn validate_event_preflight(discovered: &[String], existing: &[String]) -> AppResult<()> {
    let discovered_set = discovered.iter().collect::<std::collections::BTreeSet<_>>();
    if discovered_set.len() != discovered.len() {
        return Err(AppError::bad_request(
            "repository contains duplicate .gzevent identities",
        ));
    }
    for (index, left) in discovered.iter().enumerate() {
        let left_root = std::path::Path::new(left)
            .parent()
            .unwrap_or_else(|| std::path::Path::new(""));
        for right in discovered.iter().skip(index + 1) {
            let right_root = std::path::Path::new(right)
                .parent()
                .unwrap_or_else(|| std::path::Path::new(""));
            if left_root.starts_with(right_root) || right_root.starts_with(left_root) {
                return Err(AppError::bad_request(format!(
                    "nested .gzevent roots are not supported: {left} overlaps {right}"
                )));
            }
        }
    }
    let missing = existing
        .iter()
        .filter(|path| !discovered_set.contains(path))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(AppError::conflict(format!(
            "previously bound .gzevent manifest(s) are missing: {}; explicitly migrate, detach, or archive those games before rescanning",
            missing.join(", ")
        )));
    }
    Ok(())
}

async fn preflight_event_paths(
    st: &SharedState,
    binding_id: i32,
    checkout: &std::path::Path,
    events: Vec<std::path::PathBuf>,
) -> AppResult<Vec<(std::path::PathBuf, String)>> {
    let mut discovered = Vec::with_capacity(events.len());
    for event in events {
        let relative = event
            .strip_prefix(checkout)
            .ok()
            .and_then(|path| path.to_str())
            .map(|path| path.replace('\\', "/"))
            .ok_or_else(|| AppError::bad_request(".gzevent path is outside the repository"))?;
        discovered.push((event, relative));
    }
    discovered.sort_by(|left, right| left.1.cmp(&right.1));
    let existing = sqlx::query_scalar::<_, String>(
        r#"SELECT event_manifest_path
             FROM "Games"
            WHERE repo_binding_id = $1
              AND event_manifest_path IS NOT NULL
            ORDER BY event_manifest_path"#,
    )
    .bind(binding_id)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let paths = discovered
        .iter()
        .map(|(_, path)| path.clone())
        .collect::<Vec<_>>();
    validate_event_preflight(&paths, &existing)?;
    Ok(discovered)
}

async fn challenge_runtime_present(st: &SharedState, challenge_id: i32) -> AppResult<bool> {
    sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(
                   SELECT 1 FROM "GameChallenges"
                    WHERE id = $1
                      AND (shared_container_id IS NOT NULL OR test_container_id IS NOT NULL)
               )
               OR EXISTS(
                   SELECT 1 FROM "GameInstances"
                    WHERE challenge_id = $1 AND container_id IS NOT NULL
               )
               OR EXISTS(
                   SELECT 1 FROM "AdTeamServices"
                    WHERE challenge_id = $1
                      AND (container_id IS NOT NULL OR host <> '' OR port <> 0)
               )
               OR EXISTS(
                   SELECT 1 FROM "KothTargets"
                    WHERE challenge_id = $1 AND container_id IS NOT NULL
               )"#,
    )
    .bind(challenge_id)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

/// RSCTF blood-bonus default `(50<<20)+(30<<10)+10` — first/second/third-blood
/// bonus percentages packed into one i64; used when a `.gzevent` omits `bloodBonus`.
const DEFAULT_BLOOD_BONUS: i64 = (50 << 20) + (30 << 10) + 10;

/// `POST /api/admin/repobindings` — register a repo, optionally scanning at once.
pub async fn create_repo_binding(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Json(m): Json<RepoBindingCreateModel>,
) -> AppResult<RequestResponse<RepoBindingScanResultModel>> {
    let repo_url = crate::services::git_sync::validate_binding_repo_url(&m.repo_url)?;
    let git_ref = crate::services::git_sync::validate_git_ref(m.r#ref.as_deref())?;
    let now = sqlx::query_scalar::<_, DateTime<Utc>>("SELECT clock_timestamp()")
        .fetch_one(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let interval = crate::services::repo_binding_scheduler::validate_interval(
        m.interval_seconds.unwrap_or(3600),
    )?;
    let am = repo_binding::ActiveModel {
        repo_url: Set(repo_url),
        git_ref: Set(git_ref),
        github_token: Set(m.github_token.filter(|s| !s.trim().is_empty())),
        interval_seconds: Set(interval),
        status: Set(RepoWatchStatus::Active),
        last_commit_sha: Set(None),
        last_scan_message: Set(None),
        last_scan_utc: Set(None),
        next_scan_utc: Set(Some(now + Duration::seconds(interval as i64))),
        created_at_utc: Set(now),
        push_on_edit: Set(false),
        ..Default::default()
    };
    let model = am.insert(&st.db).await?;
    let id = model.id;
    // RSCTF's create returns the scan result, not the binding: when scanning at
    // once, hand back the real counts; otherwise a zeroed result the client's
    // success toast can read (gamesCreated/... rather than NaN).
    let result = if m.run_immediately.unwrap_or(false) {
        // Best-effort: the binding exists whether or not the first scan succeeds.
        crate::services::repo_binding_scheduler::run_manual(st.clone(), id)
            .await
            .unwrap_or_else(|e| RepoBindingScanResultModel {
                games_created: 0,
                games_updated: 0,
                challenges_imported: 0,
                challenges_updated: 0,
                failures: 1,
                messages: vec![format!("scan failed: {e}")],
            })
    } else {
        RepoBindingScanResultModel {
            games_created: 0,
            games_updated: 0,
            challenges_imported: 0,
            challenges_updated: 0,
            failures: 0,
            messages: vec!["Repo binding created; scan not run.".to_string()],
        }
    };
    Ok(RequestResponse::ok(result))
}

/// `PUT /api/admin/repobindings/{id}` — patch only the provided fields.
pub async fn update_repo_binding(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
    Json(m): Json<RepoBindingUpdateModel>,
) -> AppResult<RequestResponse<RepoBindingInfoModel>> {
    let model = update_repo_binding_record(&st, id, m).await?;
    Ok(RequestResponse::ok(
        queries::repo_info_after_update(&st, model).await?,
    ))
}

/// `DELETE /api/admin/repobindings/{id}` — detach retained games, then drop the
/// binding and its scan history.
pub async fn delete_repo_binding(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
) -> AppResult<MessageResponse> {
    delete_repo_binding_record(&st, id).await?;
    Ok(MessageResponse::ok(""))
}

/// `POST /api/admin/repobindings/{id}/scan` — clone + import now.
pub async fn scan_repo_binding(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<RepoBindingScanResultModel>> {
    Ok(RequestResponse::ok(
        crate::services::repo_binding_scheduler::run_manual(st, id).await?,
    ))
}

/// Run a real scan: clone/fetch the repo, read HEAD, discover challenge manifests,
/// record a truthful scan row + update the binding. Reports the actual manifest
/// count (never faked all-zeros); full per-game import is bounded by the manifests'
/// own game targets.
pub(crate) async fn execute_repo_binding_scan(
    st: &SharedState,
    claim: &crate::services::repo_binding_scheduler::RepoScanClaim,
    checkout_lock: crate::services::git_sync::CheckoutLockGuard,
) -> AppResult<RepoBindingScanExecution> {
    let id = claim.id;
    let dest = std::path::PathBuf::from(&st.config.storage_root)
        .join("repos")
        .join(id.to_string());
    // The durable scheduler transfers its checkout fence into this scan. The
    // fence is consumed only after producing an immutable snapshot, so later
    // fetches and push-back cannot change files during import/build staging.
    let mut checkout_lock = Some(checkout_lock);
    // Refresh after checkout admission so same-SHA skip and credentials/ref
    // selection never use a queued task's stale binding snapshot.
    let binding = repo_binding::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Repo binding not found"))?;
    let now = claim.claimed_at;
    let repo_url = crate::services::git_sync::validate_binding_repo_url(&binding.repo_url)?;
    let git_ref = crate::services::git_sync::validate_git_ref(binding.git_ref.as_deref())?;

    if !crate::services::repo_binding_scheduler::set_activity(st.pg(), claim, "Fetching repository")
        .await?
    {
        return Err(AppError::conflict("Repository binding scan lease was lost"));
    }
    let mut messages: Vec<String> = Vec::new();
    let mut failures = 0;
    let mut challenge_counts = ChallengeSyncCounts::default();
    let mut commit_sha: Option<String> = None;

    // Embed the token (if any) as Basic-auth userinfo, like RSCTF's fetch URL.
    let url = match &binding.github_token {
        Some(t) if !t.is_empty() => {
            crate::services::git_sync::GitCredentials::new(t.clone()).apply(&repo_url)
        }
        _ => repo_url.clone(),
    };

    let mut games_created = 0;
    let mut games_updated = 0;
    match crate::services::git_sync::sync_repo(&url, git_ref.as_deref(), &dest).await {
        Ok(()) => {
            commit_sha = crate::services::git_sync::head_sha(&dest).await.ok();
            if commit_already_applied(st, &binding, commit_sha.as_deref()).await? {
                drop(checkout_lock.take());
                messages.push(format!(
                    "Commit {} was already imported successfully; no repository changes applied.",
                    commit_sha.clone().unwrap_or_else(|| "?".into())
                ));
            } else {
                let snapshot = checkout_lock
                    .take()
                    .expect("repository checkout lock is retained until snapshot creation")
                    .immutable_snapshot(&dest, id)
                    .await?;
                let import_root = snapshot.path().to_path_buf();
                // RSCTF `RepoBindingDiscoveryService`: walk for every `.gzevent`, make
                // ONE game (event) per manifest, and import the challenges UNDER that
                // manifest's directory into it. No `.gzevent` → nothing imported (no
                // challenge.yaml fallback), matching RSCTF exactly.
                match crate::services::git_sync::discover_events(&import_root).await {
                    Ok(events) => match preflight_event_paths(st, id, &import_root, events).await {
                        Err(error) => {
                            failures += 1;
                            messages.push(format!("repository event preflight failed: {error}"));
                        }
                        Ok(events) if events.is_empty() => {
                            messages.push(format!(
                                "Cloned {} @ {}; no .gzevent manifests found in repo.",
                                binding.repo_url,
                                commit_sha.clone().unwrap_or_else(|| "?".into())
                            ));
                        }
                        Ok(events) => {
                            for (ev_path, rel) in &events {
                                let manifest =
                                    match crate::services::git_sync::parse_event_manifest(ev_path)
                                        .await
                                    {
                                        Ok(m) => m,
                                        Err(e) => {
                                            failures += 1;
                                            messages.push(format!("{rel}: {e}"));
                                            continue;
                                        }
                                    };
                                let title = manifest.title.clone().unwrap_or_default();
                                if title.trim().is_empty() {
                                    failures += 1;
                                    messages.push(format!("{rel}: manifest missing 'title'."));
                                    continue;
                                }

                                // One game per event, keyed on (binding, manifest path).
                                // CREATE seeds settings from the manifest; UPDATE leaves
                                // operator-owned settings alone (only refreshes the path).
                                let (gid, created) =
                                    match upsert_event_game(st, id, &manifest, rel, now).await {
                                        Ok(x) => x,
                                        Err(e) => {
                                            failures += 1;
                                            messages.push(format!("{rel}: {e}"));
                                            continue;
                                        }
                                    };
                                if created {
                                    games_created += 1;
                                } else {
                                    games_updated += 1;
                                }
                                // Challenges scoped to THIS event's directory only.
                                let ev_dir = ev_path.parent().unwrap_or(import_root.as_path());
                                let chal_manifests =
                                    match crate::services::git_sync::discover_challenges(ev_dir)
                                        .await
                                    {
                                        Ok(manifests) => manifests,
                                        Err(error) => {
                                            failures += 1;
                                            messages.push(format!(
                                                "{rel}: challenge discovery failed: {error}"
                                            ));
                                            continue;
                                        }
                                    };
                                let mut configuration_lock =
                                    crate::services::ad_engine::acquire_ad_game_lock(&st.db, gid)
                                        .await?;
                                if crate::controllers::edit::ad_epoch_scoring_started_locked(
                                    configuration_lock.transaction_mut(),
                                    gid,
                                )
                                .await?
                                {
                                    failures += 1;
                                    messages.push(format!(
                                "{rel}: repository sync is locked after A&D/KotH epoch scoring has started"
                            ));
                                    configuration_lock
                                        .release()
                                        .await
                                        .map_err(|error| AppError::internal(error.to_string()))?;
                                    continue;
                                }
                                // This is only a batch preflight. Each manifest
                                // takes its own short two-phase game/definition
                                // fence after staging immutable artifacts.
                                configuration_lock
                                    .release()
                                    .await
                                    .map_err(|error| AppError::internal(error.to_string()))?;
                                let mut event_counts = ChallengeSyncCounts::default();
                                let mut seen_challenge_ids =
                                    Vec::with_capacity(chal_manifests.len());
                                let mut unresolved_manifests = 0;
                                let mut build_jobs = Vec::new();
                                let mut generator_build_jobs = Vec::new();
                                for m in &chal_manifests {
                                    let source_path = match snapshot.manifest_identity(m).await {
                                        Ok(source_path) => source_path,
                                        Err(error) => {
                                            unresolved_manifests += 1;
                                            failures += 1;
                                            messages.push(format!(
                                                "skip {}: {error}",
                                                m.file_name()
                                                    .and_then(|name| name.to_str())
                                                    .unwrap_or("manifest")
                                            ));
                                            continue;
                                        }
                                    };
                                    match crate::services::git_sync::import_repository_snapshot_manifest(
                                        st,
                                        gid,
                                        m,
                                        crate::services::git_sync::ImportPolicy::Trusted,
                                        id,
                                        &source_path,
                                    )
                                    .await
                                    {
                                        Ok(imported) => {
                                            challenge_counts.record(imported);
                                            event_counts.record(imported);
                                            seen_challenge_ids.push(imported.challenge_id);
                                            if imported.build_queued {
                                                build_jobs.push(imported.challenge_id);
                                            }
                                            if imported.generator_build_queued {
                                                generator_build_jobs.push(imported.challenge_id);
                                            }
                                            if imported.runtime_update_deferred {
                                                failures += 1;
                                                messages.push(format!(
                                            "challenge #{}: the enabled live runtime was retained because repository runtime equivalence differs or could not be verified; disable, rescan/build, then re-enable",
                                            imported.challenge_id
                                        ));
                                            }
                                            if imported.grading_update_deferred {
                                                failures += 1;
                                                messages.push(format!(
                                                    "challenge #{}: repository grading/scoring changes were retained because the Jeopardy game has started or accepted evidence exists",
                                                    imported.challenge_id
                                                ));
                                            }
                                            if !imported.attachment_synced {
                                                failures += 1;
                                                messages.push(format!(
                                            "challenge #{}: repository attachment did not synchronize; the scan remains retryable",
                                            imported.challenge_id
                                        ));
                                            }
                                        }
                                        Err(e) => {
                                            unresolved_manifests += 1;
                                            failures += 1;
                                            messages.push(format!(
                                                "skip {}: {e}",
                                                m.file_name()
                                                    .and_then(|s| s.to_str())
                                                    .unwrap_or("manifest")
                                            ));
                                        }
                                    }
                                }
                                // Builds acquire the definition fence and may perform
                                // slow external work. Run them only after releasing the
                                // per-game lock to preserve the global lock order.
                                let container_policy =
                                    crate::services::container_policy::ContainerPolicy::load(
                                        st.pg(),
                                    )
                                    .await?;
                                for challenge_id in build_jobs {
                                    let Some(challenge) =
                                        game_challenge::Entity::find_by_id(challenge_id)
                                            .one(&st.db)
                                            .await?
                                    else {
                                        failures += 1;
                                        messages.push(format!(
                                    "challenge #{challenge_id}: disappeared before its import build"
                                ));
                                        continue;
                                    };
                                    if crate::services::image_storage::lazy_build_eligible(
                                        &container_policy,
                                        &challenge,
                                    ) {
                                        tracing::info!(
                                            game = gid,
                                            challenge = challenge_id,
                                            "repository image build deferred until first player start"
                                        );
                                        continue;
                                    }
                                    let (outcome, _) =
                                        crate::controllers::edit::run_challenge_build(
                                            st, &challenge, "Import", 1,
                                        )
                                        .await;
                                    if outcome.status != ChallengeBuildStatus::Success {
                                        failures += 1;
                                        messages.push(format!(
                                            "challenge #{challenge_id}: import build failed: {}",
                                            outcome
                                                .log
                                                .unwrap_or_else(|| format!("{:?}", outcome.status))
                                        ));
                                    }
                                }
                                for challenge_id in generator_build_jobs {
                                    if let Err(error) =
                                        crate::controllers::edit::run_import_variant_generator_build(
                                            st,
                                            challenge_id,
                                        )
                                        .await
                                    {
                                        failures += 1;
                                        messages.push(format!(
                                            "challenge #{challenge_id}: import generator build failed: {error}"
                                        ));
                                    }
                                }

                                let mut tombstoned = Vec::new();
                                // A retained runtime/grading update, attachment warning, or
                                // build failure still resolved that manifest's durable ID and
                                // cannot make a different missing path ambiguous. Only an
                                // import error leaves the seen-ID set incomplete and must block
                                // removal reconciliation for this event.
                                if missing_challenge_reconciliation_is_safe(unresolved_manifests) {
                                    let configuration_lock =
                                        crate::services::ad_engine::acquire_ad_game_lock(
                                            &st.db, gid,
                                        )
                                        .await?;
                                    let result =
                                        crate::services::git_sync::tombstone_missing_challenges(
                                            st,
                                            gid,
                                            &seen_challenge_ids,
                                        )
                                        .await;
                                    // KotH checker/capture owns runtime provisioning
                                    // before it briefly takes this game lock. Release
                                    // the broad configuration fence before cleanup
                                    // enters any per-runtime provisioning section.
                                    configuration_lock
                                        .release()
                                        .await
                                        .map_err(|error| AppError::internal(error.to_string()))?;
                                    match result {
                                        Ok(ids) => tombstoned = ids,
                                        Err(error) => {
                                            failures += 1;
                                            messages.push(format!(
                                        "{rel}: removed challenge reconciliation failed: {error}"
                                        ));
                                        }
                                    }
                                }
                                let mut refresh_ad_network = false;
                                let tombstoned_count = tombstoned.len();
                                for challenge_id in &tombstoned {
                                    // Serialize the entire cleanup with false -> true
                                    // edits. Whichever side wins this challenge fence
                                    // completes first; cleanup then re-reads the durable
                                    // disabled marker before touching any runtime.
                                    let transition = match crate::services::challenge_workloads::acquire_runtime_transition_lock(st.pg(), *challenge_id).await {
                                        Ok(lock) => lock,
                                        Err(error) => {
                                            failures += 1;
                                            messages.push(format!(
                                                "challenge #{challenge_id}: cleanup transition lock failed: {error}"
                                            ));
                                            continue;
                                        }
                                    };
                                    let challenge = match game_challenge::Entity::find()
                                        .filter(game_challenge::Column::Id.eq(*challenge_id))
                                        .filter(game_challenge::Column::GameId.eq(gid))
                                        .filter(game_challenge::Column::IsEnabled.eq(false))
                                        .filter(
                                            game_challenge::Column::SourceYamlPath.is_not_null(),
                                        )
                                        .one(&st.db)
                                        .await
                                    {
                                        Ok(Some(challenge)) => challenge,
                                        Ok(None) => {
                                            transition.release().await.map_err(|error| {
                                                AppError::internal(error.to_string())
                                            })?;
                                            continue;
                                        }
                                        Err(error) => {
                                            failures += 1;
                                            messages.push(format!(
                                                "challenge #{challenge_id}: cleanup lookup failed: {error}"
                                            ));
                                            transition.release().await.map_err(|error| {
                                                AppError::internal(error.to_string())
                                            })?;
                                            continue;
                                        }
                                    };
                                    if challenge.ad_self_hosted {
                                        if let Err(error) =
                                            st.byoc.disconnect_challenge(&st.db, challenge.id).await
                                        {
                                            failures += 1;
                                            messages.push(format!(
                                                "challenge #{}: BYOC cleanup failed: {error}",
                                                challenge.id
                                            ));
                                        }
                                    }
                                    refresh_ad_network |= challenge.challenge_type.uses_ad_engine();
                                    if challenge.challenge_type.is_container() {
                                        let _ =
                                            crate::controllers::edit::destroy_challenge_containers(
                                                st, &challenge, true, false,
                                            )
                                            .await;
                                        if challenge_runtime_present(st, challenge.id).await? {
                                            failures += 1;
                                            messages.push(format!(
                                                "challenge #{}: runtime cleanup remains incomplete and will be retried",
                                                challenge.id
                                            ));
                                        }
                                    }
                                    transition
                                        .release()
                                        .await
                                        .map_err(|error| AppError::internal(error.to_string()))?;
                                }
                                if refresh_ad_network {
                                    if let Err(error) =
                                        crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await
                                    {
                                        failures += 1;
                                        messages.push(format!(
                                            "{rel}: A&D network cleanup failed: {error}"
                                        ));
                                    }
                                }
                                crate::controllers::edit::flush_game_scoreboards(st, gid).await;
                                messages.push(format!(
                            "Event '{title}' (#{gid}, {}): imported {}, updated {} of {} challenge(s); {} removed challenge(s) disabled with history retained.",
                            if created { "created" } else { "updated" },
                            event_counts.imported,
                            event_counts.updated,
                            chal_manifests.len(),
                            tombstoned_count,
                        ));
                            }
                            messages.push(format!(
                        "Cloned {} @ {}; {} event(s): +{games_created} ~{games_updated} games, {} challenge(s) imported and {} updated.",
                        binding.repo_url,
                        commit_sha.clone().unwrap_or_else(|| "?".into()),
                        events.len(),
                        challenge_counts.imported,
                        challenge_counts.updated,
                    ));
                        }
                    },
                    Err(e) => {
                        failures += 1;
                        messages.push(format!(".gzevent discovery failed: {e}"));
                    }
                }
                if let Err(error) = snapshot.cleanup().await {
                    failures += 1;
                    messages.push(format!(
                        "immutable repository snapshot cleanup was deferred: {error}"
                    ));
                }
            }
        }
        Err(e) => {
            failures += 1;
            messages.push(format!("clone/fetch failed: {e}"));
        }
    }

    let messages = bound_scan_messages(messages);
    let history_message = bounded_chars(&messages.join("\n"), MAX_SCAN_HISTORY_CHARS);
    // Persist the bounded scan history row.
    let scan = repo_binding_scan::ActiveModel {
        binding_id: Set(id),
        ran_at_utc: Set(now),
        commit_sha: Set(commit_sha.clone()),
        games_created: Set(games_created),
        games_updated: Set(games_updated),
        challenges_imported: Set(challenge_counts.imported),
        challenges_updated: Set(challenge_counts.updated),
        failures: Set(failures),
        messages: Set(Some(history_message)),
        ..Default::default()
    };
    scan.insert(&st.db).await?;

    crate::services::repo_binding_scheduler::retain_history(st.pg(), id).await?;
    let message = bounded_chars(&messages.join("; "), MAX_SCAN_MESSAGE_CHARS);
    Ok(RepoBindingScanExecution {
        result: RepoBindingScanResultModel {
            games_created,
            games_updated,
            challenges_imported: challenge_counts.imported,
            challenges_updated: challenge_counts.updated,
            failures,
            messages,
        },
        ran_at: now,
        commit_sha,
        message,
    })
}

/// RSCTF `RepoBindingDiscoveryService.UpsertGameAsync`: one game per `.gzevent`,
/// keyed on `(repo_binding_id, event_manifest_path)`.
///
/// Game-level settings are **create-only** from the manifest — they seed a fresh
/// event, but once the game exists the operator owns them via the Info page, so a
/// re-scan must NOT re-apply manifest values over live edits (that's what made
/// hand-set end-times "keep reverting"). Update touches only the manifest path.
/// Returns `(game_id, created)`.
async fn upsert_event_game(
    st: &SharedState,
    binding_id: i32,
    manifest: &crate::services::git_sync::GzEventModel,
    manifest_rel: &str,
    now: DateTime<Utc>,
) -> AppResult<(i32, bool)> {
    // Already bound to this (binding, manifest path): update-only.
    if let Some(g) = game::Entity::find()
        .filter(game::Column::RepoBindingId.eq(binding_id))
        .filter(game::Column::EventManifestPath.eq(manifest_rel))
        .one(&st.db)
        .await?
    {
        let id = g.id;
        update_bound_game_manifest_path(&st.db, id, manifest_rel).await?;
        return Ok((id, false));
    }

    let title = manifest.title.clone().unwrap_or_default();
    let start_time_utc = manifest.start.unwrap_or(now + Duration::days(1));
    let end_time_utc = manifest.end.unwrap_or(now + Duration::days(30));
    let team_member_count_limit = manifest.team_member_count_limit.unwrap_or(0);
    let container_count_limit = manifest.container_count_limit.unwrap_or(3);
    let ad = manifest.ad.as_ref();
    let configuration = crate::services::game_config::GameConfiguration {
        start_time_utc,
        end_time_utc,
        freeze_time_utc: None,
        team_member_count_limit,
        container_count_limit,
        ad_warmup_seconds: ad.and_then(|value| value.warmup_seconds),
        ad_snapshot_retention_days: ad.and_then(|value| value.snapshot_retention_days),
        ad_tick_seconds: ad.and_then(|value| value.tick_seconds),
        ad_flag_lifetime_ticks: ad.and_then(|value| value.flag_lifetime_ticks),
        ad_reset_cooldown_minutes: ad.and_then(|value| value.reset_cooldown_minutes),
        ad_getflag_window_fraction: ad.and_then(|value| value.getflag_window_fraction),
        ad_min_grace_period_seconds: ad.and_then(|value| value.min_grace_period_seconds),
        ad_epoch_ticks: 8,
        koth_epoch_ticks: 12,
        koth_cycle_ticks: 3,
        koth_champion_cooldown_ticks: 1,
        koth_claim_confirmation_ticks: 2,
    };
    configuration.validate()?;

    // Create: seed all settings from the manifest (sparse → entity defaults).
    let (gpub, gpriv) = crate::utils::crypto_utils::generate_game_keypair();
    let am = game::ActiveModel {
        title: Set(title),
        public_key: Set(gpub),
        private_key: Set(gpriv),
        summary: Set(manifest.summary.clone().unwrap_or_default()),
        content: Set(manifest.content.clone().unwrap_or_default()),
        hidden: Set(manifest.hidden.unwrap_or(false)),
        practice_mode: Set(manifest.practice_mode.unwrap_or(true)),
        accept_without_review: Set(manifest.accept_without_review.unwrap_or(false)),
        allow_user_submissions: Set(false),
        invite_code: Set(manifest.invite_code.clone().filter(|s| !s.is_empty())),
        start_time_utc: Set(start_time_utc),
        end_time_utc: Set(end_time_utc),
        writeup_deadline: Set(manifest
            .writeup_deadline
            .unwrap_or(now + Duration::days(30))),
        writeup_required: Set(manifest.writeup_required.unwrap_or(false)),
        writeup_note: Set(manifest.writeup_note.clone().unwrap_or_default()),
        team_member_count_limit: Set(team_member_count_limit),
        container_count_limit: Set(container_count_limit),
        blood_bonus_value: Set(manifest.blood_bonus.unwrap_or(DEFAULT_BLOOD_BONUS)),
        repo_binding_id: Set(Some(binding_id)),
        event_manifest_path: Set(Some(manifest_rel.to_string())),
        // A&D knobs: sparse — only set when the manifest's `ad:` names them.
        ad_tick_seconds: Set(ad.and_then(|a| a.tick_seconds)),
        ad_flag_lifetime_ticks: Set(ad.and_then(|a| a.flag_lifetime_ticks)),
        ad_warmup_seconds: Set(ad.and_then(|a| a.warmup_seconds)),
        ad_reset_cooldown_minutes: Set(ad.and_then(|a| a.reset_cooldown_minutes)),
        ad_snapshot_retention_days: Set(ad.and_then(|a| a.snapshot_retention_days)),
        ad_getflag_window_fraction: Set(ad.and_then(|a| a.getflag_window_fraction)),
        ad_min_grace_period_seconds: Set(ad.and_then(|a| a.min_grace_period_seconds)),
        ad_allow_snapshot_download: Set(ad.and_then(|a| a.allow_snapshot_download).unwrap_or(true)),
        ad_scoring_paused: Set(false),
        // Keep repository-created games aligned with `add_game`. Fresh schemas
        // can contain these NOT NULL columns without their migration defaults
        // because m0001 derives them from the current entity before m0046's
        // `ADD COLUMN IF NOT EXISTS` statements run.
        ad_epoch_ticks: Set(8),
        koth_epoch_ticks: Set(12),
        koth_cycle_ticks: Set(3),
        koth_champion_cooldown_ticks: Set(1),
        koth_claim_confirmation_ticks: Set(2),
        ..Default::default()
    };
    Ok((am.insert(&st.db).await?.id, true))
}

#[cfg(test)]
#[path = "repo_bindings_tests.rs"]
mod tests;
