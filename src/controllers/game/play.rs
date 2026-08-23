//! Player-facing play surface: game listing/details, join/leave, challenge view + flag submission.
use super::membership::*;
use super::*;

#[path = "play_final_policy.rs"]
mod final_policy;

/// `GET /api/game/recent` — recent games ordered ongoing > upcoming > ended.
pub async fn recent_games(
    State(st): State<SharedState>,
    Query(q): Query<RecentQuery>,
) -> AppResult<RequestResponse<Vec<BasicGameInfoModel>>> {
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
        Some(u) => find_participation(&st, u, id).await?,
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
        vpn_access_required: g.vpn_access_required,
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
) -> AppResult<Response> {
    // Accepted participants keep a read-only challenge archive after closeout.
    // Mutation endpoints still call `context_info(..., true)` and remain closed.
    let ctx = context_info(&st, &user, id, false).await?;

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
    let visible_challenge_ids = model
        .challenges
        .values()
        .flatten()
        .map(|challenge| challenge.id)
        .collect();
    // Everything above is safe to prepare before retaining a pool connection.
    // The finalizer re-proves every returned challenge and its current division
    // permission on the roster transaction, then serializes under those locks.
    final_policy::finish_details_response(
        st.pg(),
        &user,
        id,
        ctx.participation.team_id,
        ctx.participation.id,
        visible_challenge_ids,
        model,
    )
    .await
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
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    user: CurrentUser,
    Path(id): Path<i32>,
    axum::Json(model): axum::Json<GameJoinModel>,
) -> AppResult<StatusCode> {
    let g = load_game(&st, id).await?;

    if !g.practice_mode && g.end_time_utc < Utc::now() {
        // RSCTF JoinGame returns the coded `ErrorCodes.GameEnded` (10002) here.
        return Err(AppError::game_ended());
    }

    let preflight_policy = crate::services::anti_cheat::load_policy_flags(st.pg()).await?;
    let fingerprint = crate::services::anti_cheat::validate_fingerprint_submission(
        &st,
        preflight_policy,
        model.fingerprint.as_deref(),
        model.fingerprint_proof.as_deref(),
    )
    .await?;
    let current_ip = crate::services::anti_cheat::client_ip(&headers, Some(peer.ip()));

    // Lock ordering is global and consistent across join + leave: local
    // `(user, game)` -> team gates first, followed by PostgreSQL user -> team ->
    // game advisory locks on one transaction. Combined paths deliberately rely
    // on the authoritative DB game lock rather than waiting for its local
    // coalescer while retaining a connection. The user/game lock serializes
    // cross-team joins; team -> game matches review and engine mutation paths.
    let mut membership_locks =
        MembershipMutationLocks::acquire(st.pg(), user.id, id, model.team_id, true).await?;

    // Every mutable join rule is re-read only after the shared game-control
    // lock is held. This closes stale invite/review/division/window requests and
    // keeps the global DB order user -> team -> game -> rows.
    let mut identity_scope = crate::services::anti_cheat::lock_game_join_identity_scope(
        membership_locks.transaction_mut(),
        st.config.as_ref(),
        user.id,
        current_ip.as_deref(),
        fingerprint.as_deref(),
    )
    .await?;
    membership_locks.acquire_game_advisory().await?;
    crate::services::anti_cheat::lock_game_join_observation_games(
        membership_locks.transaction_mut(),
        user.id,
        id,
        &mut identity_scope,
    )
    .await?;
    let policy = resolve_join_policy_locked(
        membership_locks.transaction_mut(),
        id,
        model.division_id,
        model.invite_code.as_deref(),
    )
    .await?;
    crate::services::anti_cheat::lock_live_request_account(
        membership_locks.transaction_mut(),
        user.id,
        &user.security_stamp,
    )
    .await?;
    let target_status = policy.target_status;

    // Re-read the team and caller membership after both locks are held. No
    // `team.locked` gate: that bit freezes the roster, not registration in a
    // second game.
    let team_name =
        load_join_team_locked(membership_locks.transaction_mut(), model.team_id, user.id).await?;

    // This read is protected by the team advisory lock but deliberately does
    // not row-lock before the A&D game lock. Taking a participation row lock
    // first would invert the review path's team -> game -> row ordering.
    let existing =
        existing_team_participation_locked(membership_locks.transaction_mut(), id, model.team_id)
            .await?;
    let will_write_accepted = target_status == ParticipationStatus::Accepted
        && match existing {
            None => true,
            Some(participation) => participation.status == ParticipationStatus::Rejected as i16,
        };
    if will_write_accepted {
        crate::controllers::edit::ensure_ad_roster_status_mutable(
            policy.scoring_started,
            existing
                .map(|participation| participation_status(participation.status))
                .transpose()?,
            ParticipationStatus::Accepted,
        )?;
    }

    let token = participation_token(&g, model.team_id)?;
    let identity_decision = crate::services::anti_cheat::evaluate_game_join_identity(
        membership_locks.transaction_mut(),
        user.id,
        &identity_scope,
    )
    .await?;
    if identity_decision.outcome() == crate::services::anti_cheat::AdmissionOutcome::Blocked {
        crate::services::anti_cheat::record_game_join_identity_decision(
            membership_locks.transaction_mut(),
            user.id,
            Some(&user.name),
            &identity_scope,
            &identity_decision,
        )
        .await?;
        membership_locks.release().await?;
        return Err(AppError::Coded {
            http: StatusCode::FORBIDDEN,
            code: 403,
            title: crate::services::anti_cheat::block_message().to_string(),
        });
    }
    let persisted = persist_game_join_locked(
        membership_locks.transaction_mut(),
        JoinMutation {
            user_id: user.id,
            game_id: id,
            team_id: model.team_id,
            division_id: policy.division_id,
            target_status,
            token: &token,
            member_limit: policy.member_limit,
            scoring_started: policy.scoring_started,
        },
    )
    .await?;
    crate::services::anti_cheat::record_game_join_identity_decision(
        membership_locks.transaction_mut(),
        user.id,
        Some(&user.name),
        &identity_scope,
        &identity_decision,
    )
    .await?;
    let part_id = persisted.participation_id;
    if persisted.is_accepted() {
        crate::services::anti_cheat::snapshot_recent_global_observations_for_game(
            membership_locks.transaction_mut(),
            user.id,
            id,
            model.team_id,
            part_id,
        )
        .await?;
    }
    let prepare_accepted_resources =
        target_status == ParticipationStatus::Accepted && persisted.is_accepted();

    if prepare_accepted_resources {
        sqlx::query(r#"UPDATE "Teams" SET locked = TRUE WHERE id = $1"#)
            .bind(model.team_id)
            .execute(&mut **membership_locks.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }

    // Commit the participation + membership + roster freeze before releasing
    // the scoring fence. A failed commit rolls every join row back together.
    membership_locks.release().await?;

    // Join / re-request changed this user's participation — drop any cached copy so the
    // next poll resolves fresh (also clears a stale non-accepted entry, though those
    // aren't cached today).
    st.cache
        .remove(&crate::controllers::game::ad::participation_cache_key(
            user.id, id,
        ))
        .await;

    crate::services::audit::info(
        &st,
        "GameController",
        Some(user.name.clone()),
        None,
        format!("{} has successfully joined game {}", team_name, g.title),
    )
    .await;

    // RSCTF ShouldAcceptWithoutReview -> UpdateParticipationStatus(Accepted)
    // (GameController.JoinGame): lock the team so its roster is frozen, then
    // provision the participation's play resources (EnsureInstances + self-hosted
    // A&D service containers). Mirrors the admin update_participation Accepted
    // branch; provisioning is best-effort so a Docker outage never fails the join.
    if prepare_accepted_resources {
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

    // Resolve a candidate team without retaining a transaction. It is only a
    // hint for which team gate to acquire; the row is re-read authoritatively
    // after the ordered user + team locks below.
    let initial: Option<(i32, i32)> = sqlx::query_as(
        r#"SELECT participation.id, participation.team_id
              FROM "UserParticipations" membership
              JOIN "Participations" participation
                ON participation.id = membership.participation_id
             WHERE membership.user_id = $1 AND membership.game_id = $2"#,
    )
    .bind(user.id)
    .bind(id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((part_id, team_id)) = initial else {
        return Err(AppError::bad_request(
            "Cannot leave a game you have not joined",
        ));
    };

    let mut membership_locks =
        MembershipMutationLocks::acquire(st.pg(), user.id, id, team_id, false).await?;
    leave_game_membership_locked(
        membership_locks.transaction_mut(),
        user.id,
        &user.security_stamp,
        id,
        team_id,
        part_id,
    )
    .await?;

    membership_locks.release().await?;

    // Left the game — drop the cached participation so access ends now, not on the TTL.
    st.cache
        .remove(&crate::controllers::game::ad::participation_cache_key(
            user.id, id,
        ))
        .await;

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
) -> AppResult<Response> {
    // Challenge content, hints, static attachments, final score, and solvers
    // remain readable after closeout. Operational context is stripped below.
    let ctx = context_info(&st, &user, id, false).await?;

    let challenge = load_playable_challenge(&st, id, challenge_id).await?;
    let variant = if challenge.variant_mode == ChallengeVariantMode::PerParticipation {
        Some(
            crate::services::event_security::variant_for_participation(
                &st,
                id,
                challenge_id,
                ctx.participation.id,
            )
            .await?
            .ok_or_else(|| {
                AppError::unavailable(
                    "This participation's deterministic challenge variant is not ready",
                )
            })?,
        )
    } else {
        None
    };
    let variant_manifest = variant
        .as_ref()
        .map(|row| crate::services::event_security::decode_manifest(&row.manifest))
        .transpose()?;
    let mut response_grant = final_policy::PreparedChallengeGrant::new(&challenge);

    // Division may restrict viewing this challenge (RSCTF GetChallenge gate):
    // lacking ViewChallenge hides it as a 404, mirroring the submit gate.
    let perm = effective_permission(&st, &ctx.participation, challenge_id).await?;
    if !perm.contains(GamePermission::VIEW_CHALLENGE) {
        return Err(AppError::not_found("Challenge not found"));
    }

    let mut context = ClientFlagContext::default();

    // Per-team instance -> running container connection entry.
    if !ctx.archived {
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
                response_grant.bind_per_team_runtime(instance, cont);
            }
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
        let prepared_attachment = if let Some(att_id) = challenge.attachment_id {
            attachment::Entity::find_by_id(att_id).one(&st.db).await?
        } else {
            None
        };
        let prepared_file = if let Some(att) = prepared_attachment.as_ref() {
            if let Some(local_file_id) = att.local_file_id {
                local_file::Entity::find_by_id(local_file_id)
                    .one(&st.db)
                    .await?
            } else {
                None
            }
        } else {
            None
        };
        if let Some(att) = prepared_attachment.as_ref() {
            match att.file_type {
                FileType::Remote => context.url = att.remote_url.clone(),
                FileType::Local => {
                    if let Some(lf) = prepared_file.as_ref() {
                        context.url = Some(format!("/assets/{}/{}", lf.hash, lf.name));
                        context.file_size = Some(lf.file_size);
                        context.sha256 = Some(lf.hash.clone());
                    }
                }
                FileType::None => {}
            }
        }
        response_grant.bind_attachment(prepared_attachment, prepared_file);
    }

    // Shared container: the challenge serves ONE container to every team, so the
    // team's own instance owns no container — surface the challenge-owned shared
    // container's connection (read-only for players; only an admin can stop it).
    // Mirrors RSCTF `GameController.GetChallenge` (UsesSharedContainer branch): sets
    // IsSharedInstance and overrides Entry/CloseTime while leaving any attachment Url.
    if !ctx.archived && uses_shared_container(&challenge) {
        context.is_shared_instance = true;
        if let Some(sid) = challenge.shared_container_id {
            if let Some(shared) = container::Entity::find_by_id(sid).one(&st.db).await? {
                context.instance_entry = Some(shared.entry());
                context.close_time = Some(shared.expect_stop_at);
                response_grant.bind_shared_runtime(shared);
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
        content: variant_manifest
            .as_ref()
            .and_then(|manifest| manifest.content.clone())
            .unwrap_or(challenge.content),
        category: challenge.category,
        challenge_type: challenge.challenge_type,
        hints: variant_manifest
            .as_ref()
            .and_then(|manifest| manifest.hints.as_ref())
            .map(|hints| serde_json::json!(hints))
            .or(challenge.hints),
        score: current_score,
        context,
        limit: challenge.submission_limit,
        attempts,
        deadline: challenge.deadline_utc,
        user_rating,
        user_comment,
        solve_receipt_mode: challenge.solve_receipt_mode,
        receipt_verifier_identity: challenge.receipt_verifier_identity,
        variant: variant.map(|row| ClientChallengeVariant {
            id: row.id,
            revision: row.revision,
            artifact_hash: hex::encode(row.artifact_hash),
        }),
    };

    // Final authority, current game/challenge/division policy, the response,
    // and the positive-interaction event share one transaction. Reads and
    // storage preparation stay above this boundary, so no nested pool checkout
    // is possible while the roster connection is retained.
    final_policy::finish_challenge_response(
        st.pg(),
        &user,
        final_policy::ChallengeResponseScope::new(
            id,
            ctx.participation.team_id,
            ctx.participation.id,
            challenge_id,
        ),
        response_grant,
        model,
    )
    .await
}
