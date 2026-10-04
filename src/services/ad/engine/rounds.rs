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
/// flags. KotH capabilities are owned by the source-aware KotH lifecycle.
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
    ad_tick_seconds: Option<i32>,
    ad_warmup_seconds: Option<i32>,
    ad_min_grace_period_seconds: Option<i32>,
    start_time_utc: chrono::DateTime<Utc>,
    end_time_utc: chrono::DateTime<Utc>,
    ad_scoring_paused: bool,
    practice_mode: bool,
    ad_scoring_start_round: Option<i32>,
    koth_scoring_start_round: Option<i32>,
}

#[derive(Debug, sqlx::FromRow)]
struct RoundServiceRow {
    id: i32,
    checker_dir: Option<String>,
    service_weight: f64,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum RoundTargetDisposition {
    Advance,
    Repair,
    Stale,
}

fn scoring_roster_size_ready(accepted_participations: &[i32], practice_mode: bool) -> bool {
    accepted_participations.len() >= if practice_mode { 1 } else { 2 }
}

fn complete_ad_scoring_roster(
    accepted_participations: &[i32],
    ad_challenges: &[i32],
    checkers_ready: bool,
    practice_mode: bool,
) -> bool {
    checkers_ready
        && scoring_roster_size_ready(accepted_participations, practice_mode)
        && !ad_challenges.is_empty()
}

/// Find the first round containing flags for every service in the roster that
/// is about to become official. This lets an upgraded scheduler recover a
/// previously blocked event without discarding already-recorded Offline/SLA
/// evidence. On a new event there is no earlier complete round, so the caller
/// uses the round it is currently preparing.
async fn earliest_complete_ad_roster_round<'e, E>(
    executor: E,
    game_id: i32,
    latest_round: i32,
    service_ids: &[i32],
) -> AppResult<Option<i32>>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    if service_ids.is_empty() {
        return Ok(None);
    }
    sqlx::query_scalar(
        r#"SELECT MIN(complete_round.number)::integer
             FROM (
                   SELECT round.number
                     FROM "AdRounds" round
                     JOIN "AdFlags" flag ON flag.round_id = round.id
                    WHERE round.game_id = $1
                      AND round.number <= $2
                      AND flag.team_service_id = ANY($3::integer[])
                    GROUP BY round.id, round.number
                   HAVING COUNT(*) = CARDINALITY($3::integer[])
             ) complete_round"#,
    )
    .bind(game_id)
    .bind(latest_round)
    .bind(service_ids)
    .fetch_one(executor)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

fn complete_koth_scoring_roster(
    accepted_participations: &[i32],
    has_koth: bool,
    targets_ready: bool,
    checkers_ready: bool,
    lifecycle_ready: bool,
    practice_mode: bool,
) -> bool {
    has_koth
        && targets_ready
        && checkers_ready
        && lifecycle_ready
        && scoring_roster_size_ready(accepted_participations, practice_mode)
}

fn prepared_checker_exists(path: Option<&str>) -> bool {
    let Some(path) = path.map(str::trim).filter(|path| !path.is_empty()) else {
        return false;
    };
    let root = std::path::Path::new(path);
    root.join("venv/bin/python3").is_file() && root.join("src/run.py").is_file()
}

fn koth_scoring_lifecycle_ready(
    crown_shape_ready: bool,
    has_marker_hill: bool,
    champion_cooldown_ticks: i32,
    accepted_participation_count: usize,
    vpn_enabled: bool,
) -> bool {
    crown_shape_ready
        && (!has_marker_hill
            || champion_cooldown_ticks == 0
            || accepted_participation_count < 2
            || vpn_enabled)
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
    // The five-second round poll normally observes a boundary up to one full
    // polling interval late. Preserve that nominal boundary (plus one second of
    // transaction jitter) so a configured 30-second event stays on a 30-second
    // cadence. Longer delay is material platform downtime and receives a fresh
    // full tick instead of replaying live flags into an expired window.
    let ordinary_poll_delay = Duration::seconds(super::ROUND_SCHEDULER_POLL_SECONDS as i64 + 1);
    let reanchored = now.signed_duration_since(nominal.0) > ordinary_poll_delay;
    let start = if reanchored { now } else { nominal.0 };
    let end = if reanchored {
        (start + Duration::seconds(tick_seconds)).min(event_end)
    } else {
        nominal.1.min(event_end)
    };
    let end = absorb_short_terminal_tail(end, event_end, minimum_duration_seconds);
    (end > start
        && end.signed_duration_since(start) >= Duration::seconds(minimum_duration_seconds.max(1)))
    .then_some((start, end, reanchored))
}

fn absorb_short_terminal_tail(
    round_end: chrono::DateTime<Utc>,
    event_end: chrono::DateTime<Utc>,
    minimum_duration_seconds: i64,
) -> chrono::DateTime<Utc> {
    let tail = event_end.signed_duration_since(round_end);
    if tail > Duration::zero() && tail < Duration::seconds(minimum_duration_seconds.max(1)) {
        event_end
    } else {
        round_end
    }
}

fn minimum_round_duration_seconds(grace_seconds: i64, has_api_hill: bool) -> i64 {
    let checker_minimum = grace_seconds.saturating_add(
        i64::try_from(
            super::FLAG_DELIVERY_PUBLICATION_RESERVE_SECONDS
                + super::CHECKER_MINIMUM_RUNWAY_SECONDS
                + super::CHECKER_SCHEDULER_OUTER_MARGIN_SECONDS,
        )
        .unwrap_or(i64::MAX),
    );
    if has_api_hill {
        checker_minimum.max(
            crate::services::ad::engine::koth_api::API_WAVE_SETTLEMENT_LAG_SECONDS
                .saturating_add(1),
        )
    } else {
        checker_minimum
    }
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
        r#"SELECT ad_tick_seconds, ad_warmup_seconds,
                  ad_min_grace_period_seconds,
                  start_time_utc, end_time_utc,
                  ad_scoring_paused, practice_mode, ad_scoring_start_round,
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

    let engine_challenges: Vec<(i32, i16, Option<String>, bool, bool)> = sqlx::query_as(
        r#"SELECT challenge.id, challenge."Type", challenge.ad_checker_image,
                  challenge.ad_self_hosted,
                  EXISTS (
                    SELECT 1 FROM "KothApiObservers" observer
                     WHERE observer.game_id = challenge.game_id
                       AND observer.challenge_id = challenge.id
                  ) AS api_arena
             FROM "GameChallenges" challenge
            WHERE challenge.game_id = $1
              AND challenge.is_enabled = TRUE
              AND challenge.review_status = $2
              AND challenge."Type" IN ($3, $4)
            ORDER BY challenge.id
            FOR SHARE OF challenge"#,
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
    let has_marker_hill = engine_challenges
        .iter()
        .any(|challenge| challenge.1 == ChallengeType::KingOfTheHill as i16 && !challenge.4);
    let has_api_hill = engine_challenges
        .iter()
        .any(|challenge| challenge.1 == ChallengeType::KingOfTheHill as i16 && challenge.4);
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
    let minimum_duration_seconds = minimum_round_duration_seconds(grace_seconds, has_api_hill);
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

    // Enrollment is not a scoring prerequisite. Freeze one durable Offline
    // identity for every accepted team/challenge pair; service publication can
    // fill that row before or after this transaction without changing identity.
    super::super::service_lifecycle::ensure_scoring_placeholders(&mut **tx, game_id).await?;

    let services = sqlx::query_as::<_, RoundServiceRow>(
        r#"SELECT service.id, challenge.ad_checker_image AS checker_dir,
                  LEAST(1.2, GREATEST(0.8, challenge.ad_scoring_weight))
                    AS service_weight
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
    let ad_checkers_ready = engine_challenges
        .iter()
        .filter(|challenge| challenge.1 == ChallengeType::AttackDefense as i16)
        .all(|challenge| prepared_checker_exists(challenge.2.as_deref()));
    let koth_checkers_ready = engine_challenges
        .iter()
        .filter(|challenge| challenge.1 == ChallengeType::KingOfTheHill as i16)
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
    let crown_shape_ready = super::koth_cycle::valid_crown_shape(
        crown_settings.0,
        crown_settings.1,
        crown_settings.2,
        crown_settings.3,
    );
    let koth_lifecycle_ready = koth_scoring_lifecycle_ready(
        crown_shape_ready,
        has_marker_hill,
        crown_settings.2,
        accepted_participation_ids.len(),
        crate::services::ad_vpn::enabled(),
    );
    let ad_scoring_ready = complete_ad_scoring_roster(
        &accepted_participation_ids,
        &ad_challenge_ids,
        ad_checkers_ready,
        game_settings.practice_mode,
    );
    let koth_scoring_ready = complete_koth_scoring_roster(
        &accepted_participation_ids,
        has_koth,
        koth_targets_ready,
        koth_checkers_ready,
        koth_lifecycle_ready,
        game_settings.practice_mode,
    );

    // A&D and KotH freeze independent scoring boundaries. An unavailable BYOC
    // service is a scored Offline service and must not suppress A&D or a healthy
    // shared hill. An unavailable hill must not suppress a prepared A&D checker.
    // Practice events may freeze one accepted team; competitive events retain
    // the two-team minimum.
    if ad_scoring_ready && game_settings.ad_scoring_start_round.is_none() {
        let service_ids: Vec<i32> = services.iter().map(|service| service.id).collect();
        let scoring_start_round =
            earliest_complete_ad_roster_round(&mut **tx, game_id, target_number, &service_ids)
                .await?
                .unwrap_or(target_number);
        sqlx::query(
            r#"UPDATE "Games"
                  SET ad_scoring_start_round = $2
                WHERE id = $1
                  AND ad_scoring_start_round IS NULL"#,
        )
        .bind(game_id)
        .bind(scoring_start_round)
        .execute(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    if koth_scoring_ready {
        let koth_start_round = match game_settings.koth_scoring_start_round {
            Some(start_round) => start_round,
            None => sqlx::query_scalar(
                r#"UPDATE "Games"
                          SET koth_scoring_start_round = $2
                        WHERE id = $1
                          AND koth_scoring_start_round IS NULL
                    RETURNING koth_scoring_start_round"#,
            )
            .bind(game_id)
            .bind(target_number)
            .fetch_one(&mut **tx)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?,
        };
        // This is intentionally idempotent so a round can repair a boundary
        // whose official snapshot was interrupted on an older deployment.
        super::koth_cycle::snapshot_official_config(tx, game_id, koth_start_round).await?;
    }

    if !services.is_empty() {
        let service_ids: Vec<i32> = services.iter().map(|service| service.id).collect();
        let checker_qualified: Vec<bool> = services
            .iter()
            .map(|service| prepared_checker_exists(service.checker_dir.as_deref()))
            .collect();
        let service_weights: Vec<f64> = services
            .iter()
            .map(|service| service.service_weight)
            .collect();
        let generated_flags: Vec<String> = services
            .iter()
            .map(|_| crate::utils::flag_generator::generate_ad_flag())
            .collect::<AppResult<Vec<_>>>()?;
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
              AND OCTET_LENGTH(flag.flag) = 38
              AND flag.flag ~ '^flag[{][A-Za-z0-9_-]{32}[}]$'
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
