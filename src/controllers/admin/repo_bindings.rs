//! Repo-binding CRUD + scan + game-creation helpers.

use super::*;
use crate::utils::enums::ChallengeBuildStatus;

mod mutations;
pub(crate) use mutations::{
    delete_repo_binding_record, record_scan_completion, update_repo_binding_record,
};

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

async fn commit_already_applied(
    st: &SharedState,
    binding: &repo_binding::Model,
    commit_sha: Option<&str>,
) -> AppResult<bool> {
    let Some(commit_sha) = commit_sha else {
        return Ok(false);
    };
    if binding.last_commit_sha.as_deref() != Some(commit_sha) {
        return Ok(false);
    }
    sqlx::query_scalar::<_, bool>(
        r#"SELECT failures = 0
             FROM "RepoBindingScans"
            WHERE binding_id = $1 AND commit_sha = $2
            ORDER BY id DESC
            LIMIT 1"#,
    )
    .bind(binding.id)
    .bind(commit_sha)
    .fetch_optional(st.pg())
    .await
    .map(|result| result.unwrap_or(false))
    .map_err(|error| AppError::internal(error.to_string()))
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

/// Map a persisted binding row to the wire model (the token is never returned —
/// only its presence via `hasGitHubToken`/`tokenStatus`).
/// RSCTF blood-bonus default `(50<<20)+(30<<10)+10` — first/second/third-blood
/// bonus percentages packed into one i64; used when a `.gzevent` omits `bloodBonus`.
const DEFAULT_BLOOD_BONUS: i64 = (50 << 20) + (30 << 10) + 10;

/// Build the list DTO for one binding, enumerating its games by
/// `repo_binding_id` so a multi-event repository shows every event.
async fn to_repo_info(st: &SharedState, m: repo_binding::Model) -> AppResult<RepoBindingInfoModel> {
    let games: Vec<Value> = game::Entity::find()
        .filter(game::Column::RepoBindingId.eq(m.id))
        .order_by_asc(game::Column::Title)
        .all(&st.db)
        .await?
        .into_iter()
        .map(|g| {
            serde_json::json!({
                "id": g.id,
                "title": g.title,
                "eventManifestPath": g.event_manifest_path,
            })
        })
        .collect();
    let has_token = m.github_token.as_deref().is_some_and(|t| !t.is_empty());
    Ok(RepoBindingInfoModel {
        id: m.id,
        repo_url: m.repo_url,
        r#ref: m.git_ref,
        created_at_utc: m.created_at_utc,
        last_scan_utc: m.last_scan_utc,
        next_scan_utc: m.next_scan_utc,
        interval_seconds: m.interval_seconds,
        status: match m.status {
            RepoWatchStatus::Active => "Active",
            RepoWatchStatus::Paused => "Paused",
        }
        .to_string(),
        last_commit_sha: m.last_commit_sha,
        last_scan_message: m.last_scan_message,
        has_git_hub_token: has_token,
        token_status: if has_token { "Ok" } else { "NotConfigured" }.to_string(),
        current_activity: None,
        push_on_edit: m.push_on_edit,
        games,
    })
}

/// `GET /api/admin/repobindings` — every configured binding, newest first.
pub async fn list_repo_bindings(
    State(st): State<SharedState>,
    _admin: AdminUser,
) -> AppResult<RequestResponse<Vec<RepoBindingInfoModel>>> {
    let rows = repo_binding::Entity::find()
        .order_by_desc(repo_binding::Column::Id)
        .all(&st.db)
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(to_repo_info(&st, r).await?);
    }
    Ok(RequestResponse::ok(out))
}

/// `POST /api/admin/repobindings` — register a repo, optionally scanning at once.
pub async fn create_repo_binding(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Json(m): Json<RepoBindingCreateModel>,
) -> AppResult<RequestResponse<RepoBindingScanResultModel>> {
    let repo_url = crate::services::git_sync::validate_binding_repo_url(&m.repo_url)?;
    let git_ref = crate::services::git_sync::validate_git_ref(m.r#ref.as_deref())?;
    let now = Utc::now();
    let interval = m.interval_seconds.unwrap_or(3600).max(0);
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
        run_repo_scan_cancellation_safe(st.clone(), id)
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
    Ok(RequestResponse::ok(to_repo_info(&st, model).await?))
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
        run_repo_scan_cancellation_safe(st, id).await?,
    ))
}

/// `GET /api/admin/repobindings/{id}/scans` — scan history, newest first.
pub async fn repo_binding_scans(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<Vec<RepoBindingScanHistoryModel>>> {
    let rows = repo_binding_scan::Entity::find()
        .filter(repo_binding_scan::Column::BindingId.eq(id))
        .order_by_desc(repo_binding_scan::Column::Id)
        .all(&st.db)
        .await?;
    let data = rows
        .into_iter()
        .map(|s| RepoBindingScanHistoryModel {
            id: s.id,
            ran_at_utc: s.ran_at_utc,
            commit_sha: s.commit_sha,
            games_created: s.games_created,
            games_updated: s.games_updated,
            challenges_imported: s.challenges_imported,
            challenges_updated: s.challenges_updated,
            failures: s.failures,
            messages: s.messages,
        })
        .collect();
    Ok(RequestResponse::ok(data))
}

/// Run a real scan: clone/fetch the repo, read HEAD, discover challenge manifests,
/// record a truthful scan row + update the binding. Reports the actual manifest
/// count (never faked all-zeros); full per-game import is bounded by the manifests'
/// own game targets.
async fn run_repo_scan_cancellation_safe(
    st: SharedState,
    id: i32,
) -> AppResult<RepoBindingScanResultModel> {
    // Repository imports can build several images and outlive an HTTP client.
    // Awaiting a spawned task preserves the synchronous response when the client
    // stays connected, while dropping the JoinHandle on disconnect detaches the
    // scan instead of cancelling it halfway through the event tree.
    tokio::spawn(async move { run_repo_scan(&st, id).await })
        .await
        .map_err(|error| AppError::internal(format!("repository scan task failed: {error}")))?
}

async fn run_repo_scan(st: &SharedState, id: i32) -> AppResult<RepoBindingScanResultModel> {
    let dest = std::path::PathBuf::from(&st.config.storage_root)
        .join("repos")
        .join(id.to_string());
    // Hold one checkout lock through sync, discovery, imports, and their
    // path-based attachment/checker reads. Push-back uses the same lock.
    let _checkout_lock =
        crate::services::git_sync::lock_checkout_distributed(st.pg(), &dest).await?;
    // A concurrent scan may have completed while this task waited for the
    // checkout fence. Refresh the binding under that fence so same-SHA skip and
    // credentials/ref selection never use the waiter's stale snapshot.
    let binding = repo_binding::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Repo binding not found"))?;
    let now = Utc::now();
    let repo_url = crate::services::git_sync::validate_binding_repo_url(&binding.repo_url)?;
    let git_ref = crate::services::git_sync::validate_git_ref(binding.git_ref.as_deref())?;

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
                messages.push(format!(
                    "Commit {} was already imported successfully; no repository changes applied.",
                    commit_sha.clone().unwrap_or_else(|| "?".into())
                ));
            } else {
                // RSCTF `RepoBindingDiscoveryService`: walk for every `.gzevent`, make
                // ONE game (event) per manifest, and import the challenges UNDER that
                // manifest's directory into it. No `.gzevent` → nothing imported (no
                // challenge.yaml fallback), matching RSCTF exactly.
                match crate::services::git_sync::discover_events(&dest).await {
                    Ok(events) => match preflight_event_paths(st, id, &dest, events).await {
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
                                let ev_dir = ev_path.parent().unwrap_or(dest.as_path());
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
                                let failures_before_event = failures;
                                let mut event_counts = ChallengeSyncCounts::default();
                                let mut seen_challenge_ids =
                                    Vec::with_capacity(chal_manifests.len());
                                let mut build_jobs = Vec::new();
                                for m in &chal_manifests {
                                    match crate::services::git_sync::import_manifest(
                                        st,
                                        gid,
                                        m,
                                        crate::services::git_sync::ImportPolicy::Trusted,
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
                                configuration_lock
                                    .release()
                                    .await
                                    .map_err(|error| AppError::internal(error.to_string()))?;

                                // Builds acquire the definition fence and may perform
                                // slow external work. Run them only after releasing the
                                // per-game lock to preserve the global lock order.
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

                                let mut tombstoned = Vec::new();
                                if failures == failures_before_event {
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
            }
        }
        Err(e) => {
            failures += 1;
            messages.push(format!("clone/fetch failed: {e}"));
        }
    }

    // Persist the scan history row.
    let scan = repo_binding_scan::ActiveModel {
        binding_id: Set(id),
        ran_at_utc: Set(now),
        commit_sha: Set(commit_sha.clone()),
        games_created: Set(games_created),
        games_updated: Set(games_updated),
        challenges_imported: Set(challenge_counts.imported),
        challenges_updated: Set(challenge_counts.updated),
        failures: Set(failures),
        messages: Set(Some(messages.join("\n"))),
        ..Default::default()
    };
    scan.insert(&st.db).await?;

    let interval = binding.interval_seconds;
    record_scan_completion(
        st,
        id,
        now,
        commit_sha,
        messages.join("; "),
        now + Duration::seconds(interval as i64),
    )
    .await?;

    Ok(RepoBindingScanResultModel {
        games_created,
        games_updated,
        challenges_imported: challenge_counts.imported,
        challenges_updated: challenge_counts.updated,
        failures,
        messages,
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

    // Create: seed all settings from the manifest (sparse → entity defaults).
    let (gpub, gpriv) = crate::utils::crypto_utils::generate_game_keypair();
    let ad = manifest.ad.as_ref();
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
        start_time_utc: Set(manifest.start.unwrap_or(now + Duration::days(1))),
        end_time_utc: Set(manifest.end.unwrap_or(now + Duration::days(30))),
        writeup_deadline: Set(manifest
            .writeup_deadline
            .unwrap_or(now + Duration::days(30))),
        writeup_required: Set(manifest.writeup_required.unwrap_or(false)),
        writeup_note: Set(manifest.writeup_note.clone().unwrap_or_default()),
        team_member_count_limit: Set(manifest.team_member_count_limit.unwrap_or(0)),
        container_count_limit: Set(manifest.container_count_limit.unwrap_or(3)),
        blood_bonus_value: Set(manifest.blood_bonus.unwrap_or(DEFAULT_BLOOD_BONUS)),
        repo_binding_id: Set(Some(binding_id)),
        event_manifest_path: Set(Some(manifest_rel.to_string())),
        // A&D knobs: sparse — only set when the manifest's `ad:` names them.
        ad_tick_seconds: Set(ad.and_then(|a| a.tick_seconds)),
        ad_flag_lifetime_ticks: Set(ad
            .and_then(|a| a.flag_lifetime_ticks)
            .map(|ticks| ticks.clamp(1, 50))),
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

/// Refresh repository identity without allowing a scan to mutate a game whose
/// multi-stage hard deletion has already committed. The game advisory lock is
/// acquired before the raw row lock, matching every other game mutation.
async fn update_bound_game_manifest_path(
    db: &sea_orm::DatabaseConnection,
    game_id: i32,
    manifest_rel: &str,
) -> AppResult<()> {
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(db, game_id).await?;
    let result = async {
        let deletion_pending: Option<bool> =
            sqlx::query_scalar(r#"SELECT deletion_pending FROM "Games" WHERE id = $1 FOR UPDATE"#)
                .bind(game_id)
                .fetch_optional(&mut **control.transaction_mut())
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
        match deletion_pending {
            Some(false) => {}
            Some(true) => return Err(AppError::conflict("Game is being deleted")),
            None => return Err(AppError::not_found("Game not found")),
        }
        sqlx::query(r#"UPDATE "Games" SET event_manifest_path = $2 WHERE id = $1"#)
            .bind(game_id)
            .bind(manifest_rel)
            .execute(&mut **control.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        Ok(())
    }
    .await;
    if result.is_ok() {
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    result
}

#[cfg(test)]
#[path = "repo_bindings_tests.rs"]
mod tests;
