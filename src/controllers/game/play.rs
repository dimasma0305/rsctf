//! Player-facing play surface: game listing/details, join/leave, challenge view + flag submission.
use super::*;

// ---------------------------------------------------------------------------
// Game listing
// ---------------------------------------------------------------------------

/// `GET /api/game` — paginated list of visible (non-hidden) games.
pub async fn games(
    State(st): State<SharedState>,
    Query(page): Query<PageParams>,
) -> AppResult<ArrayResponse<BasicGameInfoModel>> {
    let total = game::Entity::find()
        .filter(game::Column::Hidden.eq(false))
        .count(&st.db)
        .await? as i64;

    let rows = game::Entity::find()
        .filter(game::Column::Hidden.eq(false))
        .order_by_desc(game::Column::StartTimeUtc)
        .offset(page.skip)
        .limit(page.limit())
        .all(&st.db)
        .await?;

    let data = rows.iter().map(BasicGameInfoModel::from).collect();
    Ok(ArrayResponse::new(data, total))
}

/// `GET /api/game/recent` — recent games ordered ongoing > upcoming > ended.
pub async fn recent_games(
    State(st): State<SharedState>,
    Query(q): Query<RecentQuery>,
) -> AppResult<RequestResponse<Vec<BasicGameInfoModel>>> {
    let cache_key = "recent_games_list";
    if let Some(bytes) = st.cache.get(cache_key).await {
        if let Ok(data) = serde_json::from_slice::<Vec<BasicGameInfoModel>>(&bytes) {
            let mut res = data;
            if q.limit > 0 && res.len() > q.limit {
                res.truncate(q.limit);
            }
            return Ok(RequestResponse::ok(res));
        }
    }

    let now = Utc::now();
    let mut rows = game::Entity::find()
        .filter(game::Column::Hidden.eq(false))
        .all(&st.db)
        .await?;

    // Mirror RSCTF GenRecentGames ordering: ongoing games first (by proximity),
    // then upcoming (by start), then ended (most recent first).
    rows.sort_by_key(|g| recent_sort_key(g, now));
    rows.truncate(50);

    let data: Vec<BasicGameInfoModel> = rows.iter().map(BasicGameInfoModel::from).collect();
    if let Ok(json) = serde_json::to_vec(&data) {
        st.cache
            .set(cache_key, &json, Some(std::time::Duration::from_secs(10)))
            .await;
    }

    let mut res = data;
    if q.limit > 0 && res.len() > q.limit {
        res.truncate(q.limit);
    }
    Ok(RequestResponse::ok(res))
}

/// Sort key in seconds (RSCTF GenRecentGames): every game keyed by a raw
/// TimeSpan magnitude, sorted ascending. Ended games key on |now - end|,
/// upcoming on time-to-start, ongoing on the closest edge (start or end).
/// All three interleave by that magnitude — there is no ended-vs-live offset.
fn recent_sort_key(g: &game::Model, now: DateTime<Utc>) -> i64 {
    if g.end_time_utc <= now {
        // ended: keyed by |now - end| (most-recently-ended first). RSCTF
        // GenRecentGames sorts by the raw TimeSpan magnitude with no offset, so
        // ended games interleave with upcoming/ongoing by recency.
        (now - g.end_time_utc).num_seconds()
    } else if g.start_time_utc >= now {
        // upcoming: soonest start first.
        (g.start_time_utc - now).num_seconds()
    } else {
        // ongoing: closest edge (start or end) first.
        let since_start = (now - g.start_time_utc).num_seconds();
        let to_end = (g.end_time_utc - now).num_seconds();
        since_start.min(to_end)
    }
}

/// `GET /api/game/{id}` — detailed game info incl. caller's participation.
pub async fn game_details(
    State(st): State<SharedState>,
    MaybeUser(maybe): MaybeUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<DetailedGameInfoModel>> {
    let g = load_game_cached(&st, id).await?;

    let is_monitor = maybe.as_ref().is_some_and(|u| u.is_monitor());
    if g.hidden && !is_monitor {
        return Err(AppError::not_found("Game not found"));
    }

    let team_count = participation::Entity::find()
        .filter(participation::Column::GameId.eq(id))
        .count(&st.db)
        .await? as i64;

    let divisions = division::Entity::find()
        .filter(division::Column::GameId.eq(id))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|d| DivisionInfo {
            id: d.id,
            name: d.name,
            invite_code_required: d.invite_code.as_deref().is_some_and(|c| !c.is_empty()),
        })
        .collect::<Vec<_>>();

    // Caller's participation (if logged in).
    let part = match &maybe {
        Some(u) => find_participation(&st, u.id, id).await?,
        None => None,
    };
    let (status, division, team_name) = match &part {
        Some(p) => {
            let name = team::Entity::find_by_id(p.team_id)
                .one(&st.db)
                .await?
                .map(|t| t.name);
            (p.status, p.division_id, name)
        }
        None => (ParticipationStatus::Unsubmitted, None, None),
    };

    // Challenge panel — visible to accepted participants (and in practice mode).
    let can_view = matches!(&part, Some(p) if p.status == ParticipationStatus::Accepted)
        || (g.practice_mode && part.is_some());
    let challenges = if can_view {
        let list = game_challenge::Entity::find()
            .filter(game_challenge::Column::GameId.eq(id))
            .filter(game_challenge::Column::IsEnabled.eq(true))
            .filter(game_challenge::Column::ReviewStatus.eq(ChallengeReviewStatus::Active))
            .all(&st.db)
            .await?;
        // Challenges this participation has solved.
        let solved: HashSet<i32> = match &part {
            Some(p) => submission::Entity::find()
                .filter(submission::Column::ParticipationId.eq(p.id))
                .filter(submission::Column::Status.eq(AnswerResult::Accepted))
                .all(&st.db)
                .await?
                .into_iter()
                .map(|s| s.challenge_id)
                .collect(),
            None => HashSet::new(),
        };
        // Keyed by the ChallengeCategory *string* (e.g. "Misc", "PPC"), matching
        // RSCTF's `Record<string, ChallengeInfo[]>`; the React client groups by
        // each challenge's `.category` field, so the enum fields must be strings.
        let mut map: BTreeMap<String, Vec<ChallengeBrief>> = Default::default();
        for c in list {
            let cat = c.category;
            let key = serde_json::to_value(cat)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            map.entry(key).or_default().push(ChallengeBrief {
                solved: solved.contains(&c.id),
                id: c.id,
                title: c.title,
                category: cat,
                challenge_type: c.challenge_type,
                score: c.original_score,
            });
        }
        Some(map)
    } else {
        None
    };

    let model = DetailedGameInfoModel {
        id: g.id,
        title: g.title.clone(),
        summary: g.summary.clone(),
        content: g.content.clone(),
        hidden: g.hidden,
        divisions: if divisions.is_empty() {
            None
        } else {
            Some(divisions)
        },
        invite_code_required: g.invite_code.as_deref().is_some_and(|c| !c.is_empty()),
        writeup_required: g.writeup_required,
        poster: g.poster_url(),
        limit: g.team_member_count_limit,
        team_count,
        division,
        team_name,
        practice_mode: g.practice_mode,
        allow_user_submissions: g.allow_user_submissions,
        status,
        challenges,
        start: g.start_time_utc,
        end: g.end_time_utc,
    };
    Ok(RequestResponse::ok(model))
}

/// `GET /api/game/{id}/details` — challenge set + caller's rank + team token.
pub async fn game_details_with_challenges(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<GameDetailModel>> {
    // RSCTF `ChallengesWithTeamInfo` uses `denyAfterEnded = true`: an ended
    // non-practice game is denied here (practice-mode games remain accessible —
    // `context_info` handles that). The denial surfaces the coded GameEnded error
    // the React `TeamRank` keys on to redirect.
    let ctx = context_info(&st, &user, id, true).await?;

    // RSCTF `ChallengesWithTeamInfo` sources the challenge columns from the
    // SCOREBOARD (decayed score, live solve counts, bloods) rather than the raw
    // challenge rows, then drops the challenges the participation's division may
    // not view. Build the scoreboard once and reuse it for both. Non-monitors inside
    // the ICPC freeze window get the frozen projection (RSCTF `ChallengesWithTeamInfo`
    // honors the same freeze gate as `Scoreboard`).
    let board = build_scoreboard_cached(&st, &ctx.game, user.is_monitor()).await?;

    // Filter each category's challenges by the division's ViewChallenge permission
    // (RSCTF `FilterChallengesByPermission`); a participation not in a division keeps
    // every challenge. Permissions are batched (one query for the division's
    // overrides + one for its default) instead of up to 2 per challenge — this
    // `/details` is polled ~every 10s per client.
    let all_challenge_ids: Vec<i32> = board.challenges.values().flatten().map(|i| i.id).collect();
    let perms = effective_permissions_batch(&st, &ctx.participation, &all_challenge_ids).await?;
    let mut challenges: BTreeMap<String, Vec<ChallengeInfo>> = BTreeMap::new();
    for (cat, infos) in board.challenges {
        let kept: Vec<ChallengeInfo> = infos
            .into_iter()
            .filter(|info| {
                perms
                    .get(&info.id)
                    .is_none_or(|p| p.contains(GamePermission::VIEW_CHALLENGE))
            })
            .collect();
        if !kept.is_empty() {
            challenges.insert(cat, kept);
        }
    }
    // Mirrors RSCTF `ChallengeCount = challenges.Count` — the number of visible
    // *categories* (Dictionary key count), not the total challenge count.
    let challenge_count = challenges.len() as i32;

    // The caller team's scoreboard row (rank/score/solvedChallenges). The React
    // ChallengePanel hides EVERY challenge behind a "scoreboard not ready" screen
    // until `rank.rank` (or `rank.divisionId`) is populated, so a null here means
    // players can't see any challenges. RSCTF returns the team's ScoreboardItem;
    // `build_scoreboard` ranks all accepted participants, so a participant always
    // resolves to a row with rank >= 1.
    let rank = board
        .items
        .into_iter()
        .find(|it| it.id == ctx.participation.team_id);

    let model = GameDetailModel {
        challenges,
        challenge_count,
        rank,
        team_token: ctx.participation.token.clone(),
        writeup_required: ctx.game.writeup_required,
        writeup_deadline: ctx.game.writeup_deadline,
    };
    Ok(RequestResponse::ok(model))
}

// ---------------------------------------------------------------------------
// Join / check / leave
// ---------------------------------------------------------------------------

/// `GET /api/game/{id}/check` — teams the caller has joined + joinable divisions.
pub async fn join_check(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<GameJoinCheckInfoModel>> {
    let _ = load_game(&st, id).await?;

    let divisions = division::Entity::find()
        .filter(division::Column::GameId.eq(id))
        .all(&st.db)
        .await?;
    let joinable_divisions = divisions
        .iter()
        .filter(|d| GamePermission(d.default_permissions).contains(GamePermission::JOIN_GAME))
        .map(|d| d.id)
        .collect();

    // RSCTF GetJoinedTeams: every team the caller is a MEMBER of whose
    // participation in this game has a non-null DivisionId — not just the
    // single team from the caller's own user_participation link.
    let member_team_ids: Vec<i32> = team_member::Entity::find()
        .filter(team_member::Column::UserId.eq(user.id))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|m| m.team_id)
        .collect();

    let joined_teams: Vec<JoinedTeam> = if member_team_ids.is_empty() {
        Vec::new()
    } else {
        participation::Entity::find()
            .filter(participation::Column::GameId.eq(id))
            .filter(participation::Column::DivisionId.is_not_null())
            .filter(participation::Column::TeamId.is_in(member_team_ids))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|p| JoinedTeam {
                id: p.team_id,
                division: p.division_id.unwrap_or_default(),
            })
            .collect()
    };

    Ok(RequestResponse::ok(GameJoinCheckInfoModel {
        joined_teams,
        joinable_divisions,
    }))
}

/// `POST /api/game/{id}` — join a game.
pub async fn join_game(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    axum::Json(model): axum::Json<GameJoinModel>,
) -> AppResult<StatusCode> {
    let g = load_game(&st, id).await?;

    if !g.practice_mode && g.end_time_utc < Utc::now() {
        // RSCTF JoinGame returns the coded `ErrorCodes.GameEnded` (10002) here.
        return Err(AppError::game_ended());
    }

    // Serialize team joins with invite-based roster changes. If this join is
    // accepted immediately, setting `locked` under the same guard makes the
    // roster freeze atomic from the application's perspective.
    let roster_key = format!("team-roster:{}", model.team_id);
    let roster_guard = crate::utils::single_flight::coalesce(&roster_key).await;
    let distributed_roster =
        crate::utils::single_flight::PgAdvisoryLock::acquire(st.pg(), &roster_key).await?;
    let team = team::Entity::find_by_id(model.team_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Team not found"))?;

    // RSCTF gates joining with `team.Members.Contains(user)`. Use the team
    // roster (TeamMembers): any member — not only the captain — may register the
    // team for a game. The captain is always treated as a member.
    let is_member = team.captain_id == user.id
        || team_member::Entity::find()
            .filter(team_member::Column::TeamId.eq(team.id))
            .filter(team_member::Column::UserId.eq(user.id))
            .count(&st.db)
            .await?
            > 0;
    if !is_member {
        return Err(AppError::Forbidden);
    }
    // No `team.locked` guard: RSCTF's lock freezes the team ROSTER, not game
    // registration (GameController.JoinGame has no such check). A team accepted
    // (locked) in one game must still register for others. (Regression once
    // accept began locking teams.)

    // Resolve the joinable division (if the game defines divisions).
    let divisions = division::Entity::find()
        .filter(division::Column::GameId.eq(id))
        .all(&st.db)
        .await?;

    let mut div: Option<division::Model> = None;
    if !divisions.is_empty() {
        let div_id = model
            .division_id
            .ok_or_else(|| AppError::bad_request("A division must be selected"))?;
        let found = divisions
            .into_iter()
            .find(|d| d.id == div_id)
            .ok_or_else(|| AppError::bad_request("Invalid division"))?;
        if !GamePermission(found.default_permissions).contains(GamePermission::JOIN_GAME) {
            return Err(AppError::bad_request("Invalid division"));
        }
        div = Some(found);
    }

    // Validate invitation code (division code takes precedence over game code).
    let required_code = match &div {
        Some(d) => d.invite_code.clone().filter(|c| !c.is_empty()),
        None => g.invite_code.clone().filter(|c| !c.is_empty()),
    };
    if let Some(code) = required_code {
        if model.invite_code.as_deref() != Some(code.as_str()) {
            return Err(AppError::bad_request("Invalid invitation code"));
        }
    }

    // Reject if the user already participates in this game through any team,
    // EXCLUDING rejected participations (RSCTF CheckRepeatParticipation filters
    // `Status != Rejected`) — a rejected user may re-register.
    if let Some(p) = find_participation(&st, user.id, id).await? {
        if p.status != ParticipationStatus::Rejected {
            return Err(AppError::bad_request("Already participating in this game"));
        }
    }

    // Existing team participation in this game?
    let existing = participation::Entity::find()
        .filter(participation::Column::GameId.eq(id))
        .filter(participation::Column::TeamId.eq(team.id))
        .one(&st.db)
        .await?;

    let should_accept = match &div {
        None => g.accept_without_review,
        Some(d) => !GamePermission(d.default_permissions).contains(GamePermission::REQUIRE_REVIEW),
    };
    let target_status = if should_accept {
        ParticipationStatus::Accepted
    } else {
        ParticipationStatus::Pending
    };
    let validated_division_id = div.as_ref().map(|division| division.id);
    let will_write_accepted = target_status == ParticipationStatus::Accepted
        && match &existing {
            None => true,
            Some(participation) => participation.status == ParticipationStatus::Rejected,
        };
    let mut scoring_control = if will_write_accepted {
        let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
        let scoring_started = crate::controllers::edit::ad_epoch_scoring_started_locked(
            &mut **control.transaction_mut(),
            id,
        )
        .await?;
        crate::controllers::edit::ensure_ad_roster_status_mutable(
            scoring_started,
            existing.as_ref().map(|participation| participation.status),
            ParticipationStatus::Accepted,
        )?;
        Some(control)
    } else {
        None
    };

    // Always clean up the user's rejected participation link rows in this game so
    // re-registration re-adds the user fresh (RSCTF RemoveUserParticipations).
    // This runs only after the immutable-roster guard, so a rejected auto-accept
    // attempt has no side effects.
    user_participation::Entity::delete_many()
        .filter(user_participation::Column::UserId.eq(user.id))
        .filter(user_participation::Column::GameId.eq(id))
        .exec(&st.db)
        .await?;

    let part_id = match existing {
        Some(p) => {
            // Re-join after rejection: reset status/division.
            if p.status == ParticipationStatus::Rejected {
                let pid = p.id;
                let mut am: participation::ActiveModel = p.into();
                am.division_id = Set(validated_division_id);
                am.status = Set(target_status);
                am.update(&st.db).await?;
                pid
            } else if p.division_id != validated_division_id {
                return Err(AppError::bad_request("Invalid division"));
            } else {
                p.id
            }
        }
        None => {
            let token = participation_token(&g, team.id)?;
            let am = participation::ActiveModel {
                status: Set(target_status),
                token: Set(token),
                writeup_id: Set(None),
                game_id: Set(id),
                team_id: Set(team.id),
                division_id: Set(validated_division_id),
                suspicion_score: Set(0),
                ..Default::default()
            };
            am.insert(&st.db).await?.id
        }
    };

    // Link the user to this participation (UserParticipations join row).
    let already_linked = user_participation::Entity::find_by_id((user.id, id))
        .one(&st.db)
        .await?
        .is_some();
    if !already_linked {
        // Enforce the team member-count limit before adding a new member (RSCTF
        // GameController.JoinGame: `game.TeamMemberCountLimit > 0 &&
        // part.Members.Count >= game.TeamMemberCountLimit`). Members are the
        // UserParticipations already linked to this participation.
        if g.team_member_count_limit > 0 {
            let member_count = user_participation::Entity::find()
                .filter(user_participation::Column::ParticipationId.eq(part_id))
                .count(&st.db)
                .await?;
            if member_count >= g.team_member_count_limit as u64 {
                return Err(AppError::bad_request(
                    "The number of participants in the team exceeds the limit",
                ));
            }
        }

        let up = user_participation::ActiveModel {
            user_id: Set(user.id),
            game_id: Set(id),
            team_id: Set(team.id),
            participation_id: Set(part_id),
        };
        up.insert(&st.db).await?;
    }

    // Join / re-request changed this user's participation — drop any cached copy so the
    // next poll resolves fresh (also clears a stale non-accepted entry, though those
    // aren't cached today).
    st.cache
        .remove(&crate::controllers::game::ad::participation_cache_key(
            user.id, id,
        ))
        .await;

    crate::services::audit::info(
        &st.db,
        "GameController",
        Some(user.name.clone()),
        None,
        format!("{} has successfully joined game {}", team.name, g.title),
    )
    .await;

    // RSCTF ShouldAcceptWithoutReview -> UpdateParticipationStatus(Accepted)
    // (GameController.JoinGame): lock the team so its roster is frozen, then
    // provision the participation's play resources (EnsureInstances + self-hosted
    // A&D service containers). Mirrors the admin update_participation Accepted
    // branch; provisioning is best-effort so a Docker outage never fails the join.
    if target_status == ParticipationStatus::Accepted {
        let mut tm: team::ActiveModel = team.into();
        tm.locked = Set(true);
        tm.update(&st.db).await?;
        if let Some(control) = scoring_control.take() {
            control
                .release()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
        }
        distributed_roster.release().await?;
        drop(roster_guard);
        if let Err(e) =
            crate::controllers::edit::provision_accepted_participation(&st, id, part_id).await
        {
            tracing::warn!(
                game = id,
                participation = part_id,
                error = %e,
                "join_game: accept-without-review provisioning failed (best-effort; join committed)"
            );
        }
    } else {
        debug_assert!(scoring_control.is_none());
        distributed_roster.release().await?;
        drop(roster_guard);
    }

    Ok(StatusCode::OK)
}

/// `DELETE /api/game/{id}` — leave a game (only while Pending/Rejected).
pub async fn leave_game(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<StatusCode> {
    let _ = load_game(&st, id).await?;

    let part = find_participation(&st, user.id, id)
        .await?
        .ok_or_else(|| AppError::bad_request("Cannot leave a game you have not joined"))?;

    if part.status != ParticipationStatus::Pending && part.status != ParticipationStatus::Rejected {
        return Err(AppError::bad_request("Cannot leave after approval"));
    }

    // Remove this user's membership link.
    user_participation::Entity::delete_by_id((user.id, id))
        .exec(&st.db)
        .await?;
    // Left the game — drop the cached participation so access ends now, not on the TTL.
    st.cache
        .remove(&crate::controllers::game::ad::participation_cache_key(
            user.id, id,
        ))
        .await;

    // If no members remain, remove the participation entirely.
    let remaining = user_participation::Entity::find()
        .filter(user_participation::Column::ParticipationId.eq(part.id))
        .count(&st.db)
        .await?;
    if remaining == 0 {
        crate::services::ad_engine::revoke_koth_capabilities(&st.db, st.cache.as_ref(), &[part.id])
            .await?;
        participation::Entity::delete_by_id(part.id)
            .exec(&st.db)
            .await?;
    }

    Ok(StatusCode::OK)
}

// ---------------------------------------------------------------------------
// Challenge view + submission
// ---------------------------------------------------------------------------

/// `POST /api/game/{id}/challenge/{challengeId}/open` — unlock a challenge.
pub async fn open_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<StatusCode> {
    // RSCTF marks the challenge as opened for the team; rsctf exposes every
    // enabled challenge to accepted participants, so this is a no-op gate check.
    let ctx = context_info(&st, &user, id, true).await?;
    load_playable_challenge(&st, id, challenge_id).await?;
    let perm = effective_permission(&st, &ctx.participation, challenge_id).await?;
    if !perm.contains(GamePermission::VIEW_CHALLENGE) {
        return Err(AppError::not_found("Challenge not found"));
    }
    Ok(StatusCode::OK)
}

/// `GET /api/game/{id}/challenges/{challengeId}` — player challenge view.
pub async fn get_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<ChallengeDetailModel>> {
    let ctx = context_info(&st, &user, id, true).await?;

    let challenge = load_playable_challenge(&st, id, challenge_id).await?;

    // Division may restrict viewing this challenge (RSCTF GetChallenge gate):
    // lacking ViewChallenge hides it as a 404, mirroring the submit gate.
    let perm = effective_permission(&st, &ctx.participation, challenge_id).await?;
    if !perm.contains(GamePermission::VIEW_CHALLENGE) {
        return Err(AppError::not_found("Challenge not found"));
    }

    let mut context = ClientFlagContext::default();

    // Per-team instance -> running container connection entry.
    if let Some(instance) = game_instance::Entity::find()
        .filter(game_instance::Column::ParticipationId.eq(ctx.participation.id))
        .filter(game_instance::Column::ChallengeId.eq(challenge_id))
        .one(&st.db)
        .await?
    {
        if let Some(cont) = container::Entity::find()
            .filter(container::Column::GameInstanceId.eq(instance.id))
            .one(&st.db)
            .await?
        {
            context.instance_entry = Some(cont.entry());
            context.close_time = Some(cont.expect_stop_at);
        }
    }

    // Static attachment URL. Mirrors RSCTF `GameInstance.AttachmentUrl =
    // Challenge.Attachment.UrlWithName()`: resolve the challenge's attachment to
    // its LocalFile and emit the hash-addressed `/assets/{hash}/{name}` URL that
    // `AssetsController` serves (remote attachments surface their raw URL). The
    // previous `/assets/download/{id}/{name}` form had no matching route and hit
    // the SPA fallback (200 HTML). Dynamic-attachment per-flag files live on the
    // flag context, which this port never populates, so only the challenge-owned
    // attachment is resolved here.
    if context.instance_entry.is_none() {
        if let Some(att_id) = challenge.attachment_id {
            if let Some(att) = attachment::Entity::find_by_id(att_id).one(&st.db).await? {
                match att.file_type {
                    FileType::Remote => context.url = att.remote_url.clone(),
                    FileType::Local => {
                        if let Some(lf_id) = att.local_file_id {
                            if let Some(lf) =
                                local_file::Entity::find_by_id(lf_id).one(&st.db).await?
                            {
                                context.url = Some(format!("/assets/{}/{}", lf.hash, lf.name));
                                context.file_size = Some(lf.file_size);
                            }
                        }
                    }
                    FileType::None => {}
                }
            }
        }
    }

    // Shared container: the challenge serves ONE container to every team, so the
    // team's own instance owns no container — surface the challenge-owned shared
    // container's connection (read-only for players; only an admin can stop it).
    // Mirrors RSCTF `GameController.GetChallenge` (UsesSharedContainer branch): sets
    // IsSharedInstance and overrides Entry/CloseTime while leaving any attachment Url.
    if uses_shared_container(&challenge) {
        context.is_shared_instance = true;
        if let Some(sid) = challenge.shared_container_id {
            if let Some(shared) = container::Entity::find_by_id(sid).one(&st.db).await? {
                context.instance_entry = Some(shared.entry());
                context.close_time = Some(shared.expect_stop_at);
            }
        }
    }

    // Attempts so far for this participation+challenge.
    let attempts = submission::Entity::find()
        .filter(submission::Column::ParticipationId.eq(ctx.participation.id))
        .filter(submission::Column::ChallengeId.eq(challenge_id))
        .count(&st.db)
        .await? as i32;

    // Caller's own review of this challenge, if any (RSCTF surfaces this so the
    // player UI can pre-fill the like/dislike + comment controls).
    let review = challenge_review::Entity::find()
        .filter(challenge_review::Column::UserId.eq(user.id))
        .filter(challenge_review::Column::ChallengeId.eq(challenge_id))
        .one(&st.db)
        .await?;
    let (user_rating, user_comment) = match review {
        Some(r) => (r.rating, r.comment),
        None => (ReviewRating::None, None),
    };

    // Log the first time this team opens the challenge (RSCTF `GetChallenge` emits an
    // `EventType.ChallengeOpened` GameEvent once per team+challenge, deduped on the
    // event's `values[0]` — the challenge id string). Mirrors
    // `GameEventRepository.IsChallengeOpened(gameId, teamId, challengeId)`.
    let cid_str = challenge_id.to_string();
    // Has this team already opened this challenge? Push the challenge-id match into
    // SQL as an EXISTS (served by ix_gameevents_game_team_type + the `values[0]`
    // filter) instead of loading EVERY ChallengeOpened event for the team and
    // scanning them in memory on every challenge view.
    let already_opened: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
             SELECT 1 FROM "GameEvents"
             WHERE game_id = $1 AND team_id = $2 AND "Type" = $3 AND "values"->>0 = $4
           )"#,
    )
    .bind(id)
    .bind(ctx.participation.team_id)
    .bind(crate::utils::enums::EventType::ChallengeOpened as i16)
    .bind(&cid_str)
    .fetch_one(st.pg())
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;
    if !already_opened {
        let ev = game_event::ActiveModel {
            game_id: Set(id),
            event_type: Set(crate::utils::enums::EventType::ChallengeOpened),
            values: Set(serde_json::json!([cid_str, challenge.title.clone()])),
            publish_time_utc: Set(Utc::now()),
            user_id: Set(Some(user.id)),
            team_id: Set(ctx.participation.team_id),
            ..Default::default()
        };
        ev.insert(&st.db).await?;
    }

    // Project the score from the same board snapshot used by `/details` and the
    // solver list. In particular, a public viewer during the freeze must not learn
    // post-freeze solve activity by polling this modal's dynamic score.
    let board = build_scoreboard_cached(&st, &ctx.game, user.is_monitor()).await?;
    let current_score = board
        .challenges
        .values()
        .flatten()
        .find(|info| info.id == challenge_id)
        .map(|info| info.score)
        // The challenge passed the live visibility gate above. A miss can only be
        // a short-lived cache transition after an organizer edit; zero is the safe
        // non-leaking value until the five-second snapshot refreshes.
        .unwrap_or(0);

    let model = ChallengeDetailModel {
        id: challenge.id,
        title: challenge.title,
        content: challenge.content,
        category: challenge.category,
        challenge_type: challenge.challenge_type,
        hints: challenge.hints,
        score: current_score,
        context,
        limit: challenge.submission_limit,
        attempts,
        deadline: challenge.deadline_utc,
        user_rating,
        user_comment,
    };
    Ok(RequestResponse::ok(model))
}
