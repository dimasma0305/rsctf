//! Player-facing play surface: game listing/details, join/leave, challenge view + flag submission.
use super::membership::*;
use super::*;

#[path = "play_final_policy.rs"]
mod final_policy;

#[path = "play_participant.rs"]
mod participant;
pub use participant::*;

#[path = "play_challenges.rs"]
mod challenges;
pub use challenges::{get_challenge, open_challenge};

const MAX_RECENT_GAMES: usize = 50;

// Ended games use exact time since end, upcoming games use exact time until
// start, and ongoing games use their nearest exact edge. This deterministically
// refines only the legacy integer-second key's sub-second ties; exactly equal
// distances remain ordered by id. Matching the branch and final precision is
// necessary because a capped branch cannot preserve an unbounded FLOOR tie bucket.
// The exact top K active rows must occur in the union of the top K nearest start
// edges and top K nearest end edges, so every branch can be capped before the
// final CASE sort. Those caps bound returned candidates and sort input. In a
// pathological schedule, an active-edge index scan can still filter historical
// or future rows before it finds K active rows; the bounded single-flight below
// prevents synchronized clients from multiplying that one indexed search.
const RECENT_GAMES_SQL: &str = r#"
    WITH candidate_edges AS (
        (
            SELECT id, start_time_utc, end_time_utc
              FROM "Games"
             WHERE hidden = FALSE
               AND end_time_utc <= $1::timestamptz
             ORDER BY end_time_utc DESC, id ASC
             LIMIT $2
        )
        UNION
        (
            SELECT id, start_time_utc, end_time_utc
              FROM "Games"
             WHERE hidden = FALSE
               AND start_time_utc >= $1::timestamptz
             ORDER BY start_time_utc ASC, id ASC
             LIMIT $2
        )
        UNION
        (
            SELECT id, start_time_utc, end_time_utc
              FROM "Games"
             WHERE hidden = FALSE
               AND start_time_utc < $1::timestamptz
               AND end_time_utc > $1::timestamptz
             ORDER BY start_time_utc DESC, id ASC
             LIMIT $2
        )
        UNION
        (
            SELECT id, start_time_utc, end_time_utc
              FROM "Games"
             WHERE hidden = FALSE
               AND start_time_utc < $1::timestamptz
               AND end_time_utc > $1::timestamptz
             ORDER BY end_time_utc ASC, id ASC
             LIMIT $2
        )
    ), nearest AS MATERIALIZED (
        SELECT id,
               CASE
                   WHEN end_time_utc <= $1::timestamptz THEN
                       $1::timestamptz - end_time_utc
                   WHEN start_time_utc >= $1::timestamptz THEN
                       start_time_utc - $1::timestamptz
                   ELSE LEAST(
                       $1::timestamptz - start_time_utc,
                       end_time_utc - $1::timestamptz
                   )
               END AS distance
          FROM candidate_edges
         ORDER BY distance ASC, id ASC
         LIMIT $2
    )
    SELECT game.id, game.title, game.summary, game.poster_hash,
           game.team_member_count_limit, game.start_time_utc, game.end_time_utc
      FROM nearest
      JOIN "Games" game ON game.id = nearest.id
     ORDER BY nearest.distance ASC, game.id ASC
"#;

#[derive(Clone, sqlx::FromRow)]
struct RecentGameRow {
    id: i32,
    title: String,
    summary: String,
    poster_hash: Option<String>,
    team_member_count_limit: i32,
    start_time_utc: DateTime<Utc>,
    end_time_utc: DateTime<Utc>,
}

type RecentGamesFlightResult = Option<Result<Vec<RecentGameRow>, String>>;
static RECENT_GAMES_FLIGHT: std::sync::LazyLock<
    crate::utils::single_flight::SingleFlight<RecentGamesFlightResult>,
> = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);

fn recent_games_limit(requested: usize) -> i64 {
    if requested == 0 {
        MAX_RECENT_GAMES as i64
    } else {
        requested.min(MAX_RECENT_GAMES) as i64
    }
}

async fn query_recent_games(
    pool: &sqlx::PgPool,
    ordering_time: DateTime<Utc>,
    limit: i64,
) -> AppResult<Vec<RecentGameRow>> {
    sqlx::query_as::<_, RecentGameRow>(RECENT_GAMES_SQL)
        .bind(ordering_time)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

async fn query_recent_games_coalesced(
    pool: &sqlx::PgPool,
    ordering_time: DateTime<Utc>,
    limit: i64,
) -> AppResult<Vec<RecentGameRow>> {
    let pool = pool.clone();
    // There are only MAX_RECENT_GAMES possible keys. The first synchronized
    // poll owns the query; followers receive the same rows while retaining a
    // request-local response timestamp below.
    let key = limit.to_string();
    match RECENT_GAMES_FLIGHT
        .run(&key, move || async move {
            Some(
                query_recent_games(&pool, ordering_time, limit)
                    .await
                    .map_err(|error| error.to_string()),
            )
        })
        .await
    {
        Some(Ok(rows)) => Ok(rows),
        Some(Err(error)) => Err(AppError::internal(error)),
        None => Err(AppError::internal("recent-games query timed out")),
    }
}

/// `GET /api/game/recent` — visible games ordered by temporal proximity.
pub async fn recent_games(
    State(st): State<SharedState>,
    Query(q): Query<RecentQuery>,
) -> AppResult<RequestResponse<Vec<BasicGameInfoModel>>> {
    let ordering_time = Utc::now();
    let rows =
        query_recent_games_coalesced(st.pg(), ordering_time, recent_games_limit(q.limit)).await?;

    // Stamp the payload after the bounded database read. Capturing
    // this before an arbitrarily slow query would make the receipt-anchored
    // browser estimate lag by the entire server processing interval.
    let response_time = Utc::now();
    let res = rows
        .into_iter()
        .map(|game| BasicGameInfoModel {
            id: game.id,
            title: game.title,
            summary: game.summary,
            poster: game
                .poster_hash
                .map(|hash| format!("/assets/{hash}/poster")),
            limit: game.team_member_count_limit,
            team_count: 0,
            user_count: 0,
            average_rating: 0.0,
            review_count: 0,
            joined: false,
            participation_status: None,
            start: game.start_time_utc,
            end: game.end_time_utc,
            server_time: response_time,
        })
        .collect();
    Ok(RequestResponse::ok(res))
}

#[cfg(test)]
#[path = "play_recent_games_tests.rs"]
mod recent_games_tests;

fn can_view_challenge_catalog(
    is_monitor: bool,
    participation_status: Option<ParticipationStatus>,
    start_time_utc: DateTime<Utc>,
    now: DateTime<Utc>,
) -> bool {
    is_monitor
        || (now >= start_time_utc
            && participation_status.is_some_and(|status| status == ParticipationStatus::Accepted))
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

    let team_count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint FROM "Participations"
            WHERE game_id = $1 AND status = $2"#,
    )
    .bind(id)
    .bind(ParticipationStatus::Accepted as i16)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

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

    // Challenge metadata follows the same kickoff and accepted-participation
    // boundary as playable challenge details. Practice mode permits post-event
    // reuse without changing official scores; it never upgrades pending/rejected
    // participation.
    let can_view = can_view_challenge_catalog(
        is_monitor,
        part.as_ref().map(|participation| participation.status),
        g.start_time_utc,
        Utc::now(),
    );
    let challenges = if can_view {
        let mut list = game_challenge::Entity::find()
            .filter(game_challenge::Column::GameId.eq(id))
            .filter(game_challenge::Column::IsEnabled.eq(true))
            .filter(game_challenge::Column::ReviewStatus.eq(ChallengeReviewStatus::Active))
            .all(&st.db)
            .await?;
        if let Some(participation) = part
            .as_ref()
            .filter(|participation| participation.status == ParticipationStatus::Accepted)
        {
            let challenge_ids = list
                .iter()
                .map(|challenge| challenge.id)
                .collect::<Vec<_>>();
            let permissions =
                effective_permissions_batch(&st, participation, &challenge_ids).await?;
            list.retain(|challenge| {
                permissions
                    .get(&challenge.id)
                    .is_none_or(|permission| permission.contains(GamePermission::VIEW_CHALLENGE))
            });
        }
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
        server_time: Utc::now(),
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
    let visible_challenge_ids = visible_challenge_ids(&challenges);
    let challenge_count = i32::try_from(visible_challenge_ids.len()).unwrap_or(i32::MAX);
    let visible_challenges: HashSet<i32> = visible_challenge_ids.iter().copied().collect();

    // The caller team's scoreboard row (rank/score/solvedChallenges). The React
    // ChallengePanel hides EVERY challenge behind a "scoreboard not ready" screen
    // until `rank.rank` (or `rank.divisionId`) is populated, so a null here means
    // players can't see any challenges. RSCTF returns the team's ScoreboardItem;
    // `build_scoreboard` ranks all accepted participants, so a participant always
    // resolves to a row with rank >= 1.
    let mut rank = board
        .items
        .into_iter()
        .find(|it| it.id == ctx.participation.team_id);
    if let Some(rank) = &mut rank {
        retain_visible_solves(rank, &visible_challenges);
    }

    let model = GameDetailModel {
        challenges,
        challenge_count,
        rank,
        team_token: ctx.participation.token.clone(),
        writeup_required: ctx.game.writeup_required,
        writeup_deadline: ctx.game.writeup_deadline,
    };
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

fn visible_challenge_ids(challenges: &BTreeMap<String, Vec<ChallengeInfo>>) -> Vec<i32> {
    challenges
        .values()
        .flatten()
        .map(|challenge| challenge.id)
        .collect()
}

fn retain_visible_solves(rank: &mut ScoreboardItem, visible_challenges: &HashSet<i32>) {
    rank.solved_challenges
        .retain(|solve| visible_challenges.contains(&solve.id));
    rank.solved_count = rank.solved_challenges.len();
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
    let creates_late_accepted_participation = policy.scoring_started
        && existing.is_none()
        && target_status == ParticipationStatus::Accepted;
    if will_write_accepted && !creates_late_accepted_participation {
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
        crate::controllers::team::roster_policy::lock_team_on_accept_if_enabled(
            membership_locks.transaction_mut(),
            model.team_id,
        )
        .await?;
        crate::controllers::edit::enqueue_accepted_provisioning(
            membership_locks.transaction_mut(),
            id,
            part_id,
        )
        .await?;
    }

    // Commit participation, membership, and any configured roster freeze before
    // releasing the scoring fence. A failed commit rolls every join row back.
    membership_locks.release().await?;

    if persisted.created_participation() {
        crate::controllers::team::flush_scoreboards_for_games(&st, &[id]).await;
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
        &st,
        "GameController",
        Some(user.name.clone()),
        None,
        format!("{} has successfully joined game {}", team_name, g.title),
    )
    .await;

    // RSCTF ShouldAcceptWithoutReview -> UpdateParticipationStatus(Accepted)
    // (GameController.JoinGame): apply the optional roster freeze, then provision
    // the participation's play resources (EnsureInstances + self-hosted A&D
    // service containers). Provisioning is best-effort so a Docker outage never
    // fails the join.
    if prepare_accepted_resources {
        if let Err(e) = crate::controllers::edit::run_accepted_provisioning_job(&st, part_id).await
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

#[cfg(test)]
#[path = "play_projection_tests.rs"]
mod detail_projection_tests;

#[cfg(test)]
mod catalog_access_tests {
    use super::*;

    #[test]
    fn catalog_requires_kickoff_and_accepted_participation() {
        let now = Utc::now();
        let started = now - chrono::Duration::seconds(1);
        let upcoming = now + chrono::Duration::seconds(1);

        assert!(!can_view_challenge_catalog(false, None, started, now));
        for status in [
            ParticipationStatus::Pending,
            ParticipationStatus::Rejected,
            ParticipationStatus::Suspended,
            ParticipationStatus::Unsubmitted,
        ] {
            assert!(!can_view_challenge_catalog(
                false,
                Some(status),
                started,
                now
            ));
        }
        assert!(!can_view_challenge_catalog(
            false,
            Some(ParticipationStatus::Accepted),
            upcoming,
            now
        ));
        assert!(can_view_challenge_catalog(
            false,
            Some(ParticipationStatus::Accepted),
            started,
            now
        ));
        assert!(can_view_challenge_catalog(true, None, upcoming, now));
    }
}
