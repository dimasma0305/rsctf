//! Round advance/finalize helpers: the pure round-advance planner and durable
//! transactional round preparation.
use super::*;

// ─────────────────────────────────────────────────────────────────────────────
// Round-advance planner — pure decisions separated from persistence.
//   The DB writes and container flag-injection are the integration seams (TODO).
// ─────────────────────────────────────────────────────────────────────────────

/// A flag the engine intends to plant this tick (before it hits the DB or the
/// container). Produced by [`plan_round`]; consumed by the (unmodeled) injector.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedFlag {
    pub team_service_id: i64,
    pub participation_id: i32,
    pub challenge_id: i32,
    pub value: String,
}

/// The pure output of advancing a round: the new round shell plus the flags to
/// plant. No side effects — persistence + injection happen in the caller (TODO).
#[derive(Clone, Debug, PartialEq)]
pub struct RoundPlan {
    pub round: Round,
    pub flags: Vec<PlannedFlag>,
}

/// Compute the next round and its flag plants, purely.
///
/// Mirrors the decisions in `AdRoundService.AdvanceAsync`: next number is
/// `prev + 1` and a fresh URL-safe flag per live team-service. It performs no
/// I/O; the caller wraps it in a transaction, inserts the rows, then plants the
/// flags. Crown-cycle capabilities are owned by the KotH lifecycle.
///
/// `flag_gen` is injected so tests can supply a deterministic generator; the
/// production path passes [`random_flag`].
#[allow(clippy::too_many_arguments)]
pub fn plan_round(
    game_id: i32,
    prev_number: i32,
    now: i64,
    services: &[TeamService],
    cfg: &AdScoringConfig,
    mut flag_gen: impl FnMut() -> String,
) -> RoundPlan {
    let next_number = prev_number + 1;
    let round = Round {
        id: 0, // assigned by the DB on insert (TODO persistence)
        game_id,
        number: next_number,
        started_at: now,
        ends_at: now + cfg.tick_seconds,
    };

    let flags = services
        .iter()
        // Only services with a live container get a plant this tick.
        .filter(|ts| ts.container_id.is_some())
        .map(|ts| PlannedFlag {
            team_service_id: ts.id,
            participation_id: ts.participation_id,
            challenge_id: ts.challenge_id,
            value: flag_gen(),
        })
        .collect();

    RoundPlan { round, flags }
}

/// Decide whether a game needs a round advance right now. Round 1 bootstraps
/// only after warmup; later rounds advance when the latest round's `ends_at` has
/// passed.
pub fn needs_advance(
    now: i64,
    game_start: i64,
    latest_round_ends_at: Option<i64>,
    cfg: &AdScoringConfig,
) -> bool {
    match latest_round_ends_at {
        None => now >= game_start + cfg.warmup_seconds,
        Some(ends_at) => ends_at <= now,
    }
}

/// RSCTF's flag format: `flag{<url-safe base64 of 24 random bytes, unpadded>}`.
/// The payload uses `_` and `-` rather than `+` and `/`, without padding. The
/// caller supplies the 24-byte buffer so the crypto RNG source remains explicit.
pub fn format_flag(random_bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-";
    // URL-safe base64 without padding.
    let mut out = String::new();
    for chunk in random_bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        let idxs = [(n >> 18) & 63, (n >> 12) & 63, (n >> 6) & 63, n & 63];
        let take = chunk.len() + 1; // 3 bytes→4 chars, 2→3, 1→2
        for &i in idxs.iter().take(take) {
            out.push(ALPHABET[i as usize] as char);
        }
    }
    format!("flag{{{out}}}")
}

/// Production flag generator: 24 cryptographically-random bytes → [`format_flag`].
///
/// TODO(rng): rsctf's `uuid` crate is available but is not a general CSPRNG
/// surface for arbitrary bytes; when a vetted RNG (e.g. `rand`/`getrandom`) is
/// wired into the workspace, swap the uuid-derived entropy below for it. Two v4
/// UUIDs give 32 random bytes; the flag payload uses the first 24.
pub fn random_flag() -> String {
    let mut bytes = [0u8; 24];
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();
    bytes[..16].copy_from_slice(a.as_bytes());
    bytes[16..24].copy_from_slice(&b.as_bytes()[..8]);
    format_flag(&bytes)
}

// ─────────────────────────────────────────────────────────────────────────────
// DB-backed engine ops for durable round advance. The official scoreboard reads
// persisted evidence through `services::ad::scoring`; checker execution lives in
// `run_checker`.
// ─────────────────────────────────────────────────────────────────────────────

/// One flag durably associated with an opened round. Callers use this exact stored
/// value for best-effort BYOC publication after the database transaction commits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdvancedRoundFlag {
    pub team_service_id: i32,
    pub participation_id: i32,
    pub challenge_id: i32,
    /// True for a platform-managed service that must receive its flag through
    /// the exact container identity. False means delivery uses the BYOC tunnel.
    pub managed: bool,
    pub container_id: Option<String>,
    pub flag: String,
}

/// The durable result of one round preparation. `created` is false when another
/// caller committed the same target round first; all child rows are still checked
/// and repaired before this snapshot is returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdvancedRound {
    pub id: i32,
    pub number: i32,
    pub started_at: chrono::DateTime<Utc>,
    pub ends_at: chrono::DateTime<Utc>,
    pub created: bool,
    pub flags: Vec<AdvancedRoundFlag>,
}

/// Optimistic identity of the scheduler's latest round. The preparation
/// transaction reloads every authoritative field under lock; callers only pass
/// this small cursor to detect races without constructing an ORM entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoundCursor {
    pub id: i32,
    pub number: i32,
}

type RoundRow = (i32, i32, chrono::DateTime<Utc>, chrono::DateTime<Utc>, bool);
#[derive(Debug, sqlx::FromRow)]
struct GameSettings {
    private_key: String,
    ad_tick_seconds: Option<i32>,
    ad_warmup_seconds: Option<i32>,
    ad_min_grace_period_seconds: Option<i32>,
    start_time_utc: chrono::DateTime<Utc>,
    end_time_utc: chrono::DateTime<Utc>,
    ad_scoring_paused: bool,
    ad_scoring_start_round: Option<i32>,
    koth_scoring_start_round: Option<i32>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum RoundTargetDisposition {
    Advance,
    Repair,
    Stale,
}

#[allow(clippy::too_many_arguments)]
fn complete_engine_scoring_roster(
    accepted_participations: &[i32],
    ad_challenges: &[i32],
    has_koth: bool,
    koth_targets_ready: bool,
    service_pairs: &HashSet<(i32, i32)>,
    checkers_ready: bool,
    koth_lifecycle_ready: bool,
) -> bool {
    checkers_ready
        && accepted_participations.len() >= 2
        && (!ad_challenges.is_empty() || has_koth)
        && (ad_challenges.is_empty()
            || accepted_participations.iter().all(|participation_id| {
                ad_challenges
                    .iter()
                    .all(|challenge_id| service_pairs.contains(&(*participation_id, *challenge_id)))
            }))
        && (!has_koth || (koth_targets_ready && koth_lifecycle_ready))
}

fn prepared_checker_exists(path: Option<&str>) -> bool {
    let Some(path) = path.map(str::trim).filter(|path| !path.is_empty()) else {
        return false;
    };
    let root = std::path::Path::new(path);
    root.join("venv/bin/python3").is_file() && root.join("src/run.py").is_file()
}

fn valid_service_endpoint(host: &str, port: i32) -> bool {
    !host.trim().is_empty() && (1..=65_535).contains(&port)
}

fn classify_round_target(
    latest: Option<(i32, i32)>,
    expected_latest: Option<(i32, i32)>,
) -> RoundTargetDisposition {
    let target_number = expected_latest.map_or(1, |round| round.1 + 1);
    match (latest, expected_latest) {
        (None, None) => RoundTargetDisposition::Advance,
        (Some(current), Some(expected)) if current.0 == expected.0 => {
            RoundTargetDisposition::Advance
        }
        (Some(current), _) if current.1 == target_number => RoundTargetDisposition::Repair,
        _ => RoundTargetDisposition::Stale,
    }
}

fn authoritative_round_window(
    game_start: chrono::DateTime<Utc>,
    game_end: chrono::DateTime<Utc>,
    warmup_seconds: i64,
    tick_seconds: i64,
    latest_end: Option<chrono::DateTime<Utc>>,
) -> Option<(chrono::DateTime<Utc>, chrono::DateTime<Utc>)> {
    if warmup_seconds < 0 || tick_seconds <= 0 || game_end <= game_start {
        return None;
    }
    let start = latest_end.unwrap_or_else(|| game_start + Duration::seconds(warmup_seconds));
    if start >= game_end {
        return None;
    }
    Some((
        start,
        (start + Duration::seconds(tick_seconds)).min(game_end),
    ))
}

fn playable_round_window(
    nominal: (chrono::DateTime<Utc>, chrono::DateTime<Utc>),
    event_end: chrono::DateTime<Utc>,
    tick_seconds: i64,
    now: chrono::DateTime<Utc>,
    minimum_duration_seconds: i64,
) -> Option<(chrono::DateTime<Utc>, chrono::DateTime<Utc>, bool)> {
    // A nominal boundary that has already passed was platform downtime, not a
    // playable slice of this round. Persist the actual durable preparation time
    // so every ordinary round gets one complete, truthful tick instead of losing
    // flag/checker runway to the scheduler's polling cadence.
    let reanchored = nominal.0 < now;
    let start = if reanchored { now } else { nominal.0 };
    let end = if reanchored {
        (start + Duration::seconds(tick_seconds)).min(event_end)
    } else {
        nominal.1.min(event_end)
    };
    (end > start
        && end.signed_duration_since(start) >= Duration::seconds(minimum_duration_seconds.max(1)))
    .then_some((start, end, reanchored))
}

/// Atomically finalize the expected current round and prepare its successor.
///
/// The prior implementation committed the round, flags, checks, holder credit,
/// and KotH tokens one statement at a time. Any error after the round insert made
/// the unique `(game_id, number)` gate reject retries, leaving a permanently
/// incomplete tick. This helper holds the short-lived KotH capability lock and a
/// database transaction while writing all durable state. Slow BYOC publication,
/// container reconciliation, and checker execution deliberately remain in cron
/// after commit.
///
/// `expected_latest` is the caller's optimistic snapshot. Two callers that raced
/// on the same snapshot both target the same number: the winner creates it and the
/// waiter repairs/returns that same round rather than advancing a second time.
pub async fn prepare_round(
    db: &DatabaseConnection,
    game_id: i32,
    expected_latest: Option<RoundCursor>,
    required_network_bound: Option<bool>,
    now: chrono::DateTime<Utc>,
) -> AppResult<AdvancedRound> {
    let mut control_lock = super::koth_auth::acquire_game_lock(db, game_id).await?;
    let result = prepare_round_transaction(
        control_lock.transaction_mut(),
        game_id,
        expected_latest,
        required_network_bound,
        now,
    )
    .await;
    match result {
        // Dropping the guard rolls its transaction back, so no earlier statement
        // from a failed preparation can leak into a partial round.
        Err(error) => Err(error),
        Ok(round) => {
            control_lock
                .release()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            Ok(round)
        }
    }
}

async fn prepare_round_transaction(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    game_id: i32,
    expected_latest: Option<RoundCursor>,
    required_network_bound: Option<bool>,
    _requested_at: chrono::DateTime<Utc>,
) -> AppResult<AdvancedRound> {
    let game_settings: GameSettings = sqlx::query_as(
        r#"SELECT private_key, ad_tick_seconds, ad_warmup_seconds,
                  ad_min_grace_period_seconds,
                  start_time_utc, end_time_utc,
                  ad_scoring_paused, ad_scoring_start_round,
                  koth_scoring_start_round
             FROM "Games"
            WHERE id = $1
            FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))?;
    let crown_settings: (i32, i32, i32, i32) = sqlx::query_as(
        r#"SELECT koth_epoch_ticks, koth_cycle_ticks, koth_champion_cooldown_ticks,
                  koth_claim_confirmation_ticks
             FROM "Games" WHERE id = $1"#,
    )
    .bind(game_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    // Sample wall time only after both the advisory lock and the Games row lock are
    // held. A caller may have waited behind slow probes or a concurrent game edit;
    // its scheduler timestamp is not authoritative by the time writes can begin.
    let now: chrono::DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if game_settings.ad_scoring_paused
        || game_settings.start_time_utc > now
        || game_settings.end_time_utc <= now
    {
        return Err(AppError::conflict(
            "Game is not active for round advancement",
        ));
    }

    let engine_challenges: Vec<(i32, i16, Option<String>, bool)> = sqlx::query_as(
        r#"SELECT id, "Type", ad_checker_image, ad_self_hosted
             FROM "GameChallenges"
            WHERE game_id = $1
              AND is_enabled = TRUE
              AND review_status = $2
              AND "Type" IN ($3, $4)
            ORDER BY id
            FOR SHARE"#,
    )
    .bind(game_id)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeType::AttackDefense as i16)
    .bind(ChallengeType::KingOfTheHill as i16)
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if engine_challenges.is_empty() {
        return Err(AppError::bad_request("Game has no enabled A&D challenges"));
    }
    let network_bound = engine_challenges
        .iter()
        .any(|challenge| challenge.1 == ChallengeType::AttackDefense as i16 && challenge.3);
    if !network_scope_matches(required_network_bound, network_bound) {
        return Err(AppError::conflict(
            "Game network ownership changed before round preparation",
        ));
    }
    let has_koth = engine_challenges
        .iter()
        .any(|challenge| challenge.1 == ChallengeType::KingOfTheHill as i16);
    let koth_challenge_ids: Vec<i32> = engine_challenges
        .iter()
        .filter(|challenge| challenge.1 == ChallengeType::KingOfTheHill as i16)
        .map(|challenge| challenge.0)
        .collect();

    let latest: Option<RoundRow> = sqlx::query_as(
        r#"SELECT id, number, start_time_utc, end_time_utc, finalized
             FROM "AdRounds"
            WHERE game_id = $1
            ORDER BY number DESC
            LIMIT 1
            FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let expected_identity = expected_latest.map(|round| (round.id, round.number));
    let latest_identity = latest.as_ref().map(|round| (round.0, round.1));
    let target_number = expected_identity.map_or(1, |round| round.1 + 1);
    if classify_round_target(latest_identity, expected_identity) == RoundTargetDisposition::Stale {
        return Err(AppError::conflict(
            "Round advanced beyond the requested target. Refresh and retry.",
        ));
    }

    if let Some(expected) = expected_latest {
        let pipeline_complete: bool = sqlx::query_scalar(
            r#"SELECT pipeline_completed_at IS NOT NULL
                 FROM "AdRounds" WHERE id = $1 AND game_id = $2"#,
        )
        .bind(expected.id)
        .bind(game_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::conflict("Expected round no longer exists"))?;
        if !pipeline_complete {
            return Err(AppError::conflict(
                "The current round checker pipeline is still in flight",
            ));
        }
        sqlx::query(
            r#"UPDATE "AdCheckResults"
                  SET status = $2,
                      message = 'checker pass incomplete when the next round opened',
                      checked_at = $3,
                      sla_credit = 0.0,
                      flag_verified = FALSE
                WHERE round_id = $1 AND sla_credit IS NULL"#,
        )
        .bind(expected.id)
        .bind(AdCheckStatus::InternalError as i16)
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

        sqlx::query(r#"UPDATE "AdRounds" SET finalized = TRUE WHERE id = $1"#)
            .bind(expected.id)
            .execute(&mut **tx)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;

        sqlx::query(
            r#"INSERT INTO "KothControlResults"
                 (game_id, challenge_id, ad_round_id, controlling_participation_id,
                  responsible_participation_id, marker_observed, status,
                  error_message, checked_at,
                  is_scorable, void_reason, cycle_id, container_id,
                  confirmation_streak, confirmed_participation_id,
                  token_window_attempt)
               SELECT $1, target.challenge_id, $2, NULL, participation.id,
                      FALSE, 3,
                      'checker result unavailable; scoring sample void', $3,
                      FALSE, 'checker result unavailable; scoring sample void',
                      crown.id, target.container_id,
                      CASE WHEN crown.id IS NULL THEN NULL ELSE 0 END,
                      target.holder_participation_id,
                      COALESCE(crown.reset_attempt, 0)
                 FROM "KothTargets" target
                 JOIN "GameChallenges" challenge
                   ON challenge.id = target.challenge_id
                  AND challenge.game_id = target.game_id
                 LEFT JOIN "Participations" participation
                   ON participation.id = target.holder_participation_id
                  AND participation.game_id = target.game_id
                  AND participation.status = $4
                 LEFT JOIN LATERAL (
                   SELECT cycle.id, cycle.reset_attempt FROM "KothCrownCycles" cycle
                    WHERE cycle.game_id = target.game_id
                      AND cycle.challenge_id = target.challenge_id
                      AND (
                        SELECT number FROM "AdRounds" WHERE id = $2
                      ) BETWEEN cycle.planned_start_round AND cycle.planned_end_round
                    ORDER BY cycle.cycle_number DESC LIMIT 1
                 ) crown ON TRUE
                WHERE target.game_id = $1
                  AND challenge.is_enabled = TRUE
                  AND challenge.review_status = $5
                  AND challenge."Type" = $6
               ON CONFLICT (game_id, challenge_id, ad_round_id) DO NOTHING"#,
        )
        .bind(game_id)
        .bind(expected.id)
        .bind(now)
        .bind(ParticipationStatus::Accepted as i16)
        .bind(ChallengeReviewStatus::Active as i16)
        .bind(ChallengeType::KingOfTheHill as i16)
        .execute(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }

    let defaults = AdScoringConfig::from_env();
    let tick_seconds = game_settings
        .ad_tick_seconds
        .map(i64::from)
        .filter(|seconds| (30..=600).contains(seconds))
        .unwrap_or(defaults.tick_seconds.clamp(30, 600));
    let warmup_seconds = game_settings
        .ad_warmup_seconds
        .map(i64::from)
        .filter(|seconds| *seconds >= 0)
        .unwrap_or(defaults.warmup_seconds.max(0));
    // Derive the next identity from the prior boundary, then persist the actual
    // durable preparation boundary when polling arrived late. A platform-delay
    // interval is not presented to players as if it were playable round time.
    let (nominal_start, nominal_end) = authoritative_round_window(
        game_settings.start_time_utc,
        game_settings.end_time_utc,
        warmup_seconds,
        tick_seconds,
        latest.as_ref().map(|round| round.3),
    )
    .ok_or_else(|| AppError::conflict("No scoring round remains before the event deadline"))?;
    let grace_seconds = i64::from(
        game_settings
            .ad_min_grace_period_seconds
            .unwrap_or(super::DEFAULT_CHECKER_GRACE_SECONDS)
            .clamp(1, 60),
    );
    let minimum_duration_seconds = grace_seconds.saturating_add(
        i64::try_from(
            super::FLAG_DELIVERY_PUBLICATION_RESERVE_SECONDS
                + super::CHECKER_MINIMUM_RUNWAY_SECONDS
                + super::CHECKER_SCHEDULER_OUTER_MARGIN_SECONDS,
        )
        .unwrap_or(i64::MAX),
    );
    let (scheduled_start, requested_ends_at, reanchored) = playable_round_window(
        (nominal_start, nominal_end),
        game_settings.end_time_utc,
        tick_seconds,
        now,
        minimum_duration_seconds,
    )
    .ok_or_else(|| {
        AppError::conflict(
            "No scoring round remains with enough publication and checker runway before the event deadline",
        )
    })?;
    if reanchored {
        // Do not replay elapsed time with live flags and lifecycle work after
        // scheduler delay. The visible boundary gap is field-wide platform
        // downtime; the next playable round starts at durable preparation.
        tracing::warn!(
            game = game_id,
            skipped_from = %nominal_start,
            recovered_at = %now,
            "re-anchoring A&D round after scheduler delay"
        );
    }
    if scheduled_start > now {
        return Err(AppError::conflict("The next round boundary is not due"));
    }
    let inserted: Option<RoundRow> = sqlx::query_as(
        r#"INSERT INTO "AdRounds"
             (game_id, number, start_time_utc, end_time_utc, finalized)
           VALUES ($1, $2, $3, $4, FALSE)
           ON CONFLICT (game_id, number) DO NOTHING
           RETURNING id, number, start_time_utc, end_time_utc, finalized"#,
    )
    .bind(game_id)
    .bind(target_number)
    .bind(scheduled_start)
    .bind(requested_ends_at)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let created = inserted.is_some();
    let round = match inserted {
        Some(round) => round,
        None => sqlx::query_as(
            r#"SELECT id, number, start_time_utc, end_time_utc, finalized
                 FROM "AdRounds"
                WHERE game_id = $1 AND number = $2
                FOR UPDATE"#,
        )
        .bind(game_id)
        .bind(target_number)
        .fetch_one(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?,
    };

    #[allow(clippy::type_complexity)]
    let services: Vec<(
        i32,
        i32,
        i32,
        Option<String>,
        Option<String>,
        f64,
        String,
        i32,
    )> = sqlx::query_as(
        r#"SELECT service.id, service.participation_id, service.challenge_id,
                  service.container_id, challenge.ad_checker_image,
                  LEAST(1.2, GREATEST(0.8, challenge.ad_scoring_weight))
                    AS service_weight, service.host, service.port
             FROM "AdTeamServices" service
             JOIN "Participations" participation
               ON participation.id = service.participation_id
              AND participation.game_id = service.game_id
             JOIN "GameChallenges" challenge
               ON challenge.id = service.challenge_id
              AND challenge.game_id = service.game_id
            WHERE service.game_id = $1
              AND participation.status = $2
              AND challenge.is_enabled = TRUE
              AND challenge.review_status = $3
              AND challenge."Type" = $4
            ORDER BY service.id
            FOR SHARE OF service, participation"#,
    )
    .bind(game_id)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeType::AttackDefense as i16)
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let ad_challenge_ids: Vec<i32> = engine_challenges
        .iter()
        .filter(|challenge| challenge.1 == ChallengeType::AttackDefense as i16)
        .map(|challenge| challenge.0)
        .collect();
    let checkers_ready = engine_challenges
        .iter()
        .all(|challenge| prepared_checker_exists(challenge.2.as_deref()));
    let accepted_participation_ids: Vec<i32> = sqlx::query_scalar(
        r#"SELECT id FROM "Participations"
                WHERE game_id = $1 AND status = $2
                ORDER BY id
                FOR SHARE"#,
    )
    .bind(game_id)
    .bind(ParticipationStatus::Accepted as i16)
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let koth_target_ids: HashSet<i32> = if koth_challenge_ids.is_empty() {
        HashSet::new()
    } else {
        sqlx::query_scalar::<_, i32>(
            r#"SELECT challenge_id FROM "KothTargets"
                WHERE game_id = $1 AND challenge_id = ANY($2)
                  AND NULLIF(BTRIM(host), '') IS NOT NULL
                  AND port BETWEEN 1 AND 65535
                  AND NULLIF(BTRIM(container_id), '') IS NOT NULL
                FOR SHARE"#,
        )
        .bind(game_id)
        .bind(&koth_challenge_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .into_iter()
        .collect()
    };
    let koth_targets_ready = koth_challenge_ids
        .iter()
        .all(|challenge_id| koth_target_ids.contains(challenge_id));
    let service_pairs: HashSet<(i32, i32)> = services
        .iter()
        .filter(|service| valid_service_endpoint(&service.6, service.7))
        .map(|service| (service.1, service.2))
        .collect();
    let crown_shape_ready = super::koth_cycle::valid_crown_shape(
        crown_settings.0,
        crown_settings.1,
        crown_settings.2,
        crown_settings.3,
    );
    let koth_lifecycle_ready = crown_shape_ready && crate::services::ad_vpn::enabled();
    let scoring_roster_ready = complete_engine_scoring_roster(
        &accepted_participation_ids,
        &ad_challenge_ids,
        has_koth,
        koth_targets_ready,
        &service_pairs,
        checkers_ready,
        koth_lifecycle_ready,
    );

    // A mutable template declares its boundary only when every engine challenge
    // has a prepared checker, every A&D service and KotH target exists, at least
    // two teams are frozen, and the crown-cycle configuration is valid and
    // enforceable through the VPN layer.
    let scoring_boundary_missing = game_settings.ad_scoring_start_round.is_none()
        || (has_koth && game_settings.koth_scoring_start_round.is_none());
    if scoring_roster_ready && scoring_boundary_missing {
        sqlx::query(
            r#"UPDATE "Games"
                  SET ad_scoring_start_round = COALESCE(ad_scoring_start_round, $2),
                      koth_scoring_start_round = CASE WHEN $3
                        THEN COALESCE(koth_scoring_start_round, $2)
                        ELSE koth_scoring_start_round END
                WHERE id = $1
                  AND (ad_scoring_start_round IS NULL
                       OR ($3 AND koth_scoring_start_round IS NULL))"#,
        )
        .bind(game_id)
        .bind(target_number)
        .bind(has_koth)
        .execute(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if has_koth {
            super::koth_cycle::snapshot_official_config(tx, game_id, target_number).await?;
        }
    }

    if !services.is_empty() {
        let salt = crate::utils::flag_generator::team_hash_salt(&game_settings.private_key);
        let service_ids: Vec<i32> = services.iter().map(|service| service.0).collect();
        let checker_qualified: Vec<bool> = services
            .iter()
            .map(|service| prepared_checker_exists(service.4.as_deref()))
            .collect();
        let service_weights: Vec<f64> = services.iter().map(|service| service.5).collect();
        let generated_flags: Vec<String> = services
            .iter()
            .map(|service| {
                let seed = crate::utils::flag_generator::team_challenge_hash(
                    &salt,
                    service.2,
                    &format!("{}:{}", service.1, target_number),
                );
                crate::utils::flag_generator::generate_flag(None, &seed)
            })
            .collect();
        sqlx::query(
            r#"INSERT INTO "AdFlags"
                 (round_id, team_service_id, flag, planted_at, checker_qualified,
                  service_weight)
               SELECT $1, planted.team_service_id, planted.flag, $6,
                      planted.checker_qualified, planted.service_weight
                 FROM UNNEST($2::integer[], $3::text[], $4::boolean[], $5::float8[])
                      AS planted(team_service_id, flag, checker_qualified, service_weight)
               ON CONFLICT (round_id, team_service_id) DO NOTHING"#,
        )
        .bind(round.0)
        .bind(&service_ids)
        .bind(&generated_flags)
        .bind(&checker_qualified)
        .bind(&service_weights)
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

        sqlx::query(
            r#"INSERT INTO "AdCheckResults"
                 (round_id, team_service_id, status, message, checked_at, sla_credit)
               SELECT $1, pending.team_service_id, $3, $4, $5, NULL
                 FROM UNNEST($2::integer[]) AS pending(team_service_id)
               ON CONFLICT (round_id, team_service_id) DO NOTHING"#,
        )
        .bind(round.0)
        .bind(&service_ids)
        .bind(AdCheckStatus::InternalError as i16)
        .bind("checker not yet executed (pending k8s/docker runner)")
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }

    let flags: Vec<AdvancedRoundFlag> =
        sqlx::query_as::<_, (i32, i32, i32, bool, Option<String>, String)>(
            r#"SELECT DISTINCT ON (service.id)
                  service.id, service.participation_id, service.challenge_id,
                  NOT challenge.ad_self_hosted, service.container_id, flag.flag
             FROM "AdTeamServices" service
             JOIN "Participations" participation
               ON participation.id = service.participation_id
              AND participation.game_id = service.game_id
             JOIN "GameChallenges" challenge
               ON challenge.id = service.challenge_id
              AND challenge.game_id = service.game_id
             JOIN "AdFlags" flag
               ON flag.team_service_id = service.id
              AND flag.round_id = $1
            WHERE service.game_id = $2
              AND participation.status = $3
              AND challenge.is_enabled = TRUE
              AND challenge.review_status = $4
              AND challenge."Type" = $5
            ORDER BY service.id, flag.id"#,
        )
        .bind(round.0)
        .bind(game_id)
        .bind(ParticipationStatus::Accepted as i16)
        .bind(ChallengeReviewStatus::Active as i16)
        .bind(ChallengeType::AttackDefense as i16)
        .fetch_all(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .into_iter()
        .map(
            |(team_service_id, participation_id, challenge_id, managed, container_id, flag)| {
                AdvancedRoundFlag {
                    team_service_id,
                    participation_id,
                    challenge_id,
                    managed,
                    container_id,
                    flag,
                }
            },
        )
        .collect();

    Ok(AdvancedRound {
        id: round.0,
        number: round.1,
        started_at: round.2,
        ends_at: round.3,
        created,
        flags,
    })
}

fn network_scope_matches(required_network_bound: Option<bool>, network_bound: bool) -> bool {
    required_network_bound.is_none_or(|required| required == network_bound)
}

#[cfg(test)]
#[path = "rounds/tests.rs"]
mod atomicity_tests;
