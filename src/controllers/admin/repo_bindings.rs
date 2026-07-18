//! Repo-binding CRUD + scan + game-creation helpers.

use super::*;

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
        run_repo_scan(&st, id)
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
    let existing = repo_binding::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Repo binding not found"))?;
    let mut am: repo_binding::ActiveModel = existing.into();
    if let Some(r) = m.r#ref {
        am.git_ref = Set(crate::services::git_sync::validate_git_ref(Some(&r))?);
    }
    if let Some(i) = m.interval_seconds {
        am.interval_seconds = Set(i.max(0));
    }
    if let Some(s) = m.status {
        am.status = Set(if s == "Active" {
            RepoWatchStatus::Active
        } else {
            RepoWatchStatus::Paused
        });
    }
    if let Some(t) = m.github_token {
        am.github_token = Set(Some(t).filter(|s| !s.trim().is_empty()));
    }
    if let Some(p) = m.push_on_edit {
        am.push_on_edit = Set(p);
    }
    let model = am.update(&st.db).await?;
    Ok(RequestResponse::ok(to_repo_info(&st, model).await?))
}

/// `DELETE /api/admin/repobindings/{id}` — drop the binding and its scan history.
pub async fn delete_repo_binding(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
) -> AppResult<MessageResponse> {
    repo_binding_scan::Entity::delete_many()
        .filter(repo_binding_scan::Column::BindingId.eq(id))
        .exec(&st.db)
        .await?;
    repo_binding::Entity::delete_by_id(id).exec(&st.db).await?;
    Ok(MessageResponse::ok(""))
}

/// `POST /api/admin/repobindings/{id}/scan` — clone + import now.
pub async fn scan_repo_binding(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<RepoBindingScanResultModel>> {
    Ok(RequestResponse::ok(run_repo_scan(&st, id).await?))
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
async fn run_repo_scan(st: &SharedState, id: i32) -> AppResult<RepoBindingScanResultModel> {
    let binding = repo_binding::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Repo binding not found"))?;
    let now = Utc::now();
    let repo_url = crate::services::git_sync::validate_binding_repo_url(&binding.repo_url)?;
    let git_ref = crate::services::git_sync::validate_git_ref(binding.git_ref.as_deref())?;
    let dest = std::path::PathBuf::from(&st.config.storage_root)
        .join("repos")
        .join(id.to_string());
    // Hold one checkout lock through sync, discovery, imports, and their
    // path-based attachment/checker reads. Push-back uses the same lock.
    let _checkout_lock =
        crate::services::git_sync::lock_checkout_distributed(st.pg(), &dest).await?;

    let mut messages: Vec<String> = Vec::new();
    let mut failures = 0;
    let mut challenges_imported = 0;
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
            // RSCTF `RepoBindingDiscoveryService`: walk for every `.gzevent`, make
            // ONE game (event) per manifest, and import the challenges UNDER that
            // manifest's directory into it. No `.gzevent` → nothing imported (no
            // challenge.yaml fallback), matching RSCTF exactly.
            match crate::services::git_sync::discover_events(&dest).await {
                Ok(events) if events.is_empty() => {
                    messages.push(format!(
                        "Cloned {} @ {}; no .gzevent manifests found in repo.",
                        binding.repo_url,
                        commit_sha.clone().unwrap_or_else(|| "?".into())
                    ));
                }
                Ok(events) => {
                    for ev_path in &events {
                        let rel = ev_path
                            .strip_prefix(&dest)
                            .ok()
                            .and_then(|p| p.to_str())
                            .map(|s| s.replace('\\', "/"))
                            .unwrap_or_else(|| ev_path.display().to_string());

                        let manifest =
                            match crate::services::git_sync::parse_event_manifest(ev_path).await {
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
                            match upsert_event_game(st, id, &manifest, &rel, now).await {
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
                        let chal_manifests = crate::services::git_sync::discover_challenges(ev_dir)
                            .await
                            .unwrap_or_default();
                        let mut configuration_lock =
                            crate::services::ad_engine::acquire_ad_game_lock(&st.db, gid).await?;
                        if crate::controllers::edit::ad_epoch_scoring_started_locked(
                            &mut **configuration_lock.transaction_mut(),
                            gid,
                        )
                        .await?
                        {
                            return Err(AppError::bad_request(
                                "Repository re-scan cannot replace challenges after A&D epoch scoring has started.",
                            ));
                        }
                        // Re-sync cleanly: drop this event's challenges so a
                        // re-scan mirrors the repo rather than stacking dupes.
                        clear_game_challenges(st, gid).await?;
                        let mut imported_here = 0;
                        for m in &chal_manifests {
                            match crate::services::git_sync::import_manifest(
                                st,
                                gid,
                                m,
                                crate::services::git_sync::ImportPolicy::Trusted,
                            )
                            .await
                            {
                                Ok(_) => {
                                    challenges_imported += 1;
                                    imported_here += 1;
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
                        crate::controllers::edit::flush_ad_scoreboard(st, gid).await;
                        messages.push(format!(
                            "Event '{title}' (#{gid}, {}): imported {imported_here} of {} challenge(s).",
                            if created { "created" } else { "updated" },
                            chal_manifests.len()
                        ));
                    }
                    messages.push(format!(
                        "Cloned {} @ {}; {} event(s): +{games_created} ~{games_updated} games, {challenges_imported} challenge(s) imported.",
                        binding.repo_url,
                        commit_sha.clone().unwrap_or_else(|| "?".into()),
                        events.len()
                    ));
                }
                Err(e) => {
                    failures += 1;
                    messages.push(format!(".gzevent discovery failed: {e}"));
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
        challenges_imported: Set(challenges_imported),
        challenges_updated: Set(0),
        failures: Set(failures),
        messages: Set(Some(messages.join("\n"))),
        ..Default::default()
    };
    let _ = scan.insert(&st.db).await;

    // Update the binding's scan state. Event ownership lives on Games.repo_binding_id.
    let interval = binding.interval_seconds;
    let mut am: repo_binding::ActiveModel = binding.into();
    am.last_scan_utc = Set(Some(now));
    if commit_sha.is_some() {
        am.last_commit_sha = Set(commit_sha);
    }
    am.last_scan_message = Set(Some(messages.join("; ")));
    am.next_scan_utc = Set(Some(now + Duration::seconds(interval as i64)));
    let _ = am.update(&st.db).await;

    Ok(RepoBindingScanResultModel {
        games_created,
        games_updated,
        challenges_imported,
        challenges_updated: 0,
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
        let mut am: game::ActiveModel = g.into();
        am.event_manifest_path = Set(Some(manifest_rel.to_string()));
        am.update(&st.db).await?;
        return Ok((id, false));
    }

    let title = manifest.title.clone().unwrap_or_default();

    // Adopt a detached game (no binding) whose title matches — avoids a duplicate
    // when a binding was deleted keeping-games then re-created.
    if !title.trim().is_empty() {
        if let Some(orphan) = game::Entity::find()
            .filter(game::Column::RepoBindingId.is_null())
            .filter(game::Column::Title.eq(title.clone()))
            .one(&st.db)
            .await?
        {
            let id = orphan.id;
            let mut am: game::ActiveModel = orphan.into();
            am.repo_binding_id = Set(Some(binding_id));
            am.event_manifest_path = Set(Some(manifest_rel.to_string()));
            am.update(&st.db).await?;
            return Ok((id, false));
        }
    }

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
        ..Default::default()
    };
    Ok((am.insert(&st.db).await?.id, true))
}

/// Remove a game's challenges (and their flags) so a repo re-scan re-imports the
/// current manifest set cleanly rather than stacking duplicates.
async fn clear_game_challenges(st: &SharedState, game_id: i32) -> AppResult<()> {
    let scoring_started = sqlx::query_scalar::<_, bool>(
        r#"SELECT ad_scoring_start_round IS NOT NULL
                  OR koth_scoring_start_round IS NOT NULL
             FROM "Games" WHERE id = $1"#,
    )
    .bind(game_id)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if scoring_started {
        return Err(AppError::bad_request(
            "Repository re-scan cannot replace challenges after A&D/KotH epoch scoring has started.",
        ));
    }
    let attachment_ids: Vec<i32> = sqlx::query_scalar(
        r#"SELECT challenge.attachment_id
             FROM "GameChallenges" challenge
            WHERE challenge.game_id = $1
              AND challenge.attachment_id IS NOT NULL
           UNION
           SELECT flag.attachment_id
             FROM "FlagContexts" flag
             JOIN "GameChallenges" challenge ON challenge.id = flag.challenge_id
            WHERE challenge.game_id = $1
              AND flag.attachment_id IS NOT NULL"#,
    )
    .bind(game_id)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let removed =
        crate::services::blob_refs::delete_game_challenges(st.pg(), st.storage.as_ref(), game_id)
            .await?
            .deleted;
    if removed == 0 {
        return Ok(());
    }
    for attachment_id in attachment_ids {
        let still_used = sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS(
                   SELECT 1 FROM "GameChallenges" WHERE attachment_id = $1
                   UNION ALL
                   SELECT 1 FROM "FlagContexts" WHERE attachment_id = $1
                   UNION ALL
                   SELECT 1 FROM "ExerciseChallenges" WHERE attachment_id = $1
               )"#,
        )
        .bind(attachment_id)
        .fetch_one(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if !still_used {
            crate::controllers::edit::delete_attachment(st, attachment_id).await?;
        }
    }
    Ok(())
}
