//! Durable A&D flag-publication receipts and participant-safe failure evidence.

use super::*;
use std::collections::{HashMap, HashSet};

pub(crate) const MINIMUM_AD_TICK_SECONDS: u64 = 30;
pub(crate) const FLAG_DELIVERY_PUBLICATION_RESERVE_SECONDS: u64 = 7;
pub(crate) const CHECKER_SCHEDULER_OUTER_MARGIN_SECONDS: u64 = 1;
pub(crate) const CHECKER_MINIMUM_RUNWAY_SECONDS: u64 = 4;

const DEFAULT_FLAG_PUSH_CONCURRENCY: usize = 64;
const DEFAULT_FLAG_PUSH_ATTEMPTS: usize = 3;
const DEFAULT_FLAG_PUSH_TIMEOUT_SECONDS: u64 = 2;
const FLAG_PUSH_RETRY_BACKOFF_MILLIS: u64 = 50;
const FLAG_DELIVERY_RECEIPT_MARGIN_MILLIS: u64 = 500;

const EXPIRED_DELIVERY_REASON: &str = "round pipeline expired before flag delivery completed";
pub(crate) const PUBLICATION_DEADLINE_REASON: &str =
    "platform publication deadline elapsed before flag delivery could start";
const INCOMPLETE_ATTEMPT_REASON: &str =
    "flag delivery attempt did not complete before the publication deadline";
const REPLACED_AFTER_PUBLICATION_REASON: &str =
    "service identity changed after immutable flag publication; participant sample offline";
const CHANGED_DURING_DELIVERY_REASON: &str =
    "service identity changed while flag delivery was in flight";

/// One validated policy shared by delivery, checker scheduling, and event
/// configuration validation. The seven-second publication reserve includes the
/// default three two-second attempts, their bounded retry backoff, and durable
/// receipt handoff. Configuration may reduce that work but cannot expand the
/// phase and steal checker runway from a 30-second round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FlagDeliveryPolicy {
    concurrency: usize,
    attempts: usize,
    attempt_timeout: std::time::Duration,
}

impl Default for FlagDeliveryPolicy {
    fn default() -> Self {
        Self {
            concurrency: DEFAULT_FLAG_PUSH_CONCURRENCY,
            attempts: DEFAULT_FLAG_PUSH_ATTEMPTS,
            attempt_timeout: std::time::Duration::from_secs(DEFAULT_FLAG_PUSH_TIMEOUT_SECONDS),
        }
    }
}

impl FlagDeliveryPolicy {
    pub(crate) fn from_env() -> Result<Self, String> {
        Self::from_values(
            std::env::var("RSCTF_AD_FLAG_PUSH_CONCURRENCY")
                .ok()
                .as_deref(),
            std::env::var("RSCTF_AD_FLAG_PUSH_ATTEMPTS").ok().as_deref(),
            std::env::var("RSCTF_AD_FLAG_PUSH_TIMEOUT_SECONDS")
                .ok()
                .as_deref(),
        )
    }

    fn from_values(
        concurrency: Option<&str>,
        attempts: Option<&str>,
        timeout_seconds: Option<&str>,
    ) -> Result<Self, String> {
        fn parse_bounded<T>(
            name: &str,
            value: Option<&str>,
            default: T,
            range: std::ops::RangeInclusive<T>,
        ) -> Result<T, String>
        where
            T: Copy + Ord + std::str::FromStr + std::fmt::Display,
        {
            let Some(value) = value else {
                return Ok(default);
            };
            let parsed = value.parse::<T>().map_err(|_| {
                format!(
                    "{name} must be an integer between {} and {}",
                    range.start(),
                    range.end()
                )
            })?;
            if !range.contains(&parsed) {
                return Err(format!(
                    "{name} must be between {} and {}",
                    range.start(),
                    range.end()
                ));
            }
            Ok(parsed)
        }

        let defaults = Self::default();
        let policy = Self {
            concurrency: parse_bounded(
                "RSCTF_AD_FLAG_PUSH_CONCURRENCY",
                concurrency,
                defaults.concurrency,
                1..=256,
            )?,
            attempts: parse_bounded(
                "RSCTF_AD_FLAG_PUSH_ATTEMPTS",
                attempts,
                defaults.attempts,
                1..=5,
            )?,
            attempt_timeout: std::time::Duration::from_secs(parse_bounded(
                "RSCTF_AD_FLAG_PUSH_TIMEOUT_SECONDS",
                timeout_seconds,
                defaults.attempt_timeout.as_secs(),
                1..=10,
            )?),
        };
        let required = policy.worst_case_attempt_window();
        if required > policy.delivery_work_budget() {
            return Err(format!(
                "A&D flag push attempts and timeout require {:.3}s, exceeding the {:.3}s publication work budget required by the minimum {}s tick",
                required.as_secs_f64(),
                policy.delivery_work_budget().as_secs_f64(),
                MINIMUM_AD_TICK_SECONDS,
            ));
        }
        Ok(policy)
    }

    pub(crate) const fn concurrency(self) -> usize {
        self.concurrency
    }

    pub(crate) const fn attempts(self) -> usize {
        self.attempts
    }

    pub(crate) const fn attempt_timeout(self) -> std::time::Duration {
        self.attempt_timeout
    }

    pub(crate) fn retry_backoff(self, completed_attempt: usize) -> std::time::Duration {
        std::time::Duration::from_millis(
            FLAG_PUSH_RETRY_BACKOFF_MILLIS.saturating_mul(completed_attempt as u64),
        )
    }

    pub(crate) const fn publication_reserve(self) -> std::time::Duration {
        std::time::Duration::from_secs(FLAG_DELIVERY_PUBLICATION_RESERVE_SECONDS)
    }

    /// Hard upper bound accepted for network/container work. The actual
    /// deadline uses [`Self::worst_case_attempt_window`], leaving the rounded
    /// remainder of the seven-second phase for receipt persistence.
    pub(crate) const fn delivery_work_budget(self) -> std::time::Duration {
        std::time::Duration::from_millis(
            FLAG_DELIVERY_PUBLICATION_RESERVE_SECONDS * 1_000 - FLAG_DELIVERY_RECEIPT_MARGIN_MILLIS,
        )
    }

    pub(crate) fn worst_case_attempt_window(self) -> std::time::Duration {
        let attempts = u32::try_from(self.attempts).unwrap_or(u32::MAX);
        let timeout = self.attempt_timeout.saturating_mul(attempts);
        let backoff_steps = self.attempts.saturating_sub(1);
        let triangular = backoff_steps.saturating_mul(backoff_steps + 1) / 2;
        timeout.saturating_add(std::time::Duration::from_millis(
            FLAG_PUSH_RETRY_BACKOFF_MILLIS.saturating_mul(triangular as u64),
        ))
    }
}

pub fn validate_flag_delivery_configuration() -> anyhow::Result<()> {
    FlagDeliveryPolicy::from_env()
        .map(|_| ())
        .map_err(anyhow::Error::msg)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FlagDeliveryKind {
    Managed,
    External,
}

impl FlagDeliveryKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Managed => "Managed",
            Self::External => "External",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FlagDeliveryOutcome {
    pub(crate) team_service_id: i32,
    pub(crate) kind: FlagDeliveryKind,
    pub(crate) container_id: Option<String>,
    pub(crate) delivered: bool,
    pub(crate) attempts: i16,
    pub(crate) failure_reason: Option<String>,
    pub(crate) completed_at: chrono::DateTime<Utc>,
}

impl FlagDeliveryOutcome {
    pub(crate) fn succeeded(
        team_service_id: i32,
        kind: FlagDeliveryKind,
        container_id: Option<String>,
        attempts: usize,
    ) -> Self {
        Self {
            team_service_id,
            kind,
            container_id,
            delivered: true,
            attempts: i16::try_from(attempts).unwrap_or(i16::MAX),
            failure_reason: None,
            completed_at: Utc::now(),
        }
    }

    pub(crate) fn failed(
        team_service_id: i32,
        kind: FlagDeliveryKind,
        container_id: Option<String>,
        attempts: usize,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            team_service_id,
            kind,
            container_id,
            delivered: false,
            attempts: i16::try_from(attempts).unwrap_or(i16::MAX),
            failure_reason: Some(reason.into()),
            completed_at: Utc::now(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FlagDeliveryPublication {
    pub(crate) failure_count: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FlagDeliveryReceipt {
    pub(crate) team_service_id: i32,
    pub(crate) completed_at: chrono::DateTime<Utc>,
}

fn validate_outcomes(outcomes: &[FlagDeliveryOutcome]) -> AppResult<()> {
    let mut service_ids = HashSet::with_capacity(outcomes.len());
    for outcome in outcomes {
        if !service_ids.insert(outcome.team_service_id) {
            return Err(AppError::conflict(format!(
                "duplicate flag-delivery outcome for service {}",
                outcome.team_service_id
            )));
        }
        if !(0..=5).contains(&outcome.attempts)
            || (outcome.delivered && (outcome.attempts == 0 || outcome.failure_reason.is_some()))
            || (!outcome.delivered
                && outcome
                    .failure_reason
                    .as_deref()
                    .is_none_or(|reason| reason.trim().is_empty()))
            || (outcome.kind == FlagDeliveryKind::External && outcome.container_id.is_some())
        {
            return Err(AppError::conflict(format!(
                "invalid flag-delivery outcome for service {}",
                outcome.team_service_id
            )));
        }
    }
    Ok(())
}

async fn complete_failed_check_results(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    round_id: i32,
) -> AppResult<()> {
    sqlx::query(
        r#"UPDATE "AdCheckResults" result
              SET status = CASE WHEN delivery.attempts > 0 THEN $2 ELSE $3 END,
                  message = delivery.failure_reason,
                  checked_at = delivery.completed_at,
                  sla_credit = 0.0,
                  flag_verified = FALSE
             FROM "AdFlagDeliveryResults" delivery
            WHERE delivery.round_id = $1
              AND delivery.delivered = FALSE
              AND result.round_id = delivery.round_id
              AND result.team_service_id = delivery.team_service_id
              AND result.sla_credit IS NULL"#,
    )
    .bind(round_id)
    .bind(AdCheckStatus::Offline as i16)
    .bind(AdCheckStatus::InternalError as i16)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(|error| AppError::internal(error.to_string()))
}

async fn complete_replaced_service_results(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    game_id: i32,
    round_id: i32,
) -> AppResult<()> {
    sqlx::query(
        r#"UPDATE "AdCheckResults" result
                  SET status = $3,
                  message = $4,
                  checked_at = LEAST(clock_timestamp(), round.end_time_utc),
                  sla_credit = 0.0,
                  flag_verified = FALSE
             FROM "AdFlagDeliveryResults" delivery
             JOIN "AdTeamServices" service
               ON service.id = delivery.team_service_id
              AND service.game_id = $1
             JOIN "AdRounds" round
               ON round.id = delivery.round_id
              AND round.game_id = service.game_id
            WHERE delivery.round_id = $2
              AND delivery.delivery_kind = 'Managed'
              AND delivery.delivered = TRUE
              AND delivery.container_id IS DISTINCT FROM service.container_id
              AND result.round_id = delivery.round_id
              AND result.team_service_id = delivery.team_service_id
              AND result.sla_credit IS NULL"#,
    )
    .bind(game_id)
    .bind(round_id)
    .bind(AdCheckStatus::Offline as i16)
    .bind(REPLACED_AFTER_PUBLICATION_REASON)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(|error| AppError::internal(error.to_string()))
}

async fn insert_missing_outcomes(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    game_id: i32,
    round_id: i32,
    reason: &str,
    attempted_service_ids: &[i32],
    completed_at: chrono::DateTime<Utc>,
) -> AppResult<()> {
    sqlx::query(
        r#"INSERT INTO "AdFlagDeliveryResults"
             (round_id, team_service_id, delivery_kind, container_id,
              delivered, attempts, failure_reason, completed_at)
           SELECT flag.round_id, flag.team_service_id,
                  CASE WHEN challenge.ad_self_hosted THEN 'External' ELSE 'Managed' END,
                  CASE WHEN challenge.ad_self_hosted THEN NULL ELSE service.container_id END,
                  FALSE,
                  CASE WHEN flag.team_service_id = ANY($4) THEN 1 ELSE 0 END,
                  CASE WHEN flag.team_service_id = ANY($4) THEN $5 ELSE $3 END,
                  $6
             FROM "AdFlags" flag
             JOIN "AdRounds" round ON round.id = flag.round_id
             JOIN "AdTeamServices" service ON service.id = flag.team_service_id
             JOIN "GameChallenges" challenge
               ON challenge.id = service.challenge_id
              AND challenge.game_id = service.game_id
            WHERE flag.round_id = $1
              AND round.game_id = $2
           ON CONFLICT (round_id, team_service_id) DO NOTHING"#,
    )
    .bind(round_id)
    .bind(game_id)
    .bind(reason)
    .bind(attempted_service_ids)
    .bind(INCOMPLETE_ATTEMPT_REASON)
    .bind(completed_at)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(|error| AppError::internal(error.to_string()))
}

async fn settle_round_publication(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    game_id: i32,
    round_id: i32,
    published_at: chrono::DateTime<Utc>,
) -> AppResult<FlagDeliveryPublication> {
    // A successful push is bound to the exact managed-container identity that
    // acknowledged it. Churn between that receipt and first settlement is
    // participant-controlled evidence, not a missing platform sample.
    complete_replaced_service_results(tx, game_id, round_id).await?;
    complete_failed_check_results(tx, round_id).await?;
    let row: Option<(i32,)> = sqlx::query_as(
        r#"UPDATE "AdRounds" round
              SET flags_published_at = COALESCE(round.flags_published_at, $3),
                  flag_delivery_failures = (
                    SELECT COUNT(*)::integer
                      FROM "AdFlagDeliveryResults" delivery
                     WHERE delivery.round_id = round.id
                       AND delivery.delivered = FALSE
                  )
            WHERE round.id = $1 AND round.game_id = $2
              AND round.finalized = FALSE
              AND NOT EXISTS (
                    SELECT 1 FROM "AdFlags" flag
                     WHERE flag.round_id = round.id
                       AND NOT EXISTS (
                         SELECT 1 FROM "AdFlagDeliveryResults" delivery
                          WHERE delivery.round_id = flag.round_id
                            AND delivery.team_service_id = flag.team_service_id
                       )
                  )
        RETURNING round.flag_delivery_failures"#,
    )
    .bind(round_id)
    .bind(game_id)
    .bind(published_at)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let (failure_count,) = row.ok_or_else(|| {
        AppError::conflict("flag delivery did not settle every service in the round")
    })?;
    Ok(FlagDeliveryPublication { failure_count })
}

#[allow(clippy::type_complexity)]
async fn record_flag_delivery_outcomes_transaction(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    game_id: i32,
    round_id: i32,
    outcomes: &[FlagDeliveryOutcome],
    settle_missing: Option<&str>,
    attempted_service_ids: &[i32],
) -> AppResult<FlagDeliveryPublication> {
    validate_outcomes(outcomes)?;
    let round: Option<(
        chrono::DateTime<Utc>,
        chrono::DateTime<Utc>,
        chrono::DateTime<Utc>,
        Option<chrono::DateTime<Utc>>,
        i32,
    )> = sqlx::query_as(
        r#"SELECT round.start_time_utc, round.end_time_utc, game.end_time_utc,
                  round.flags_published_at, round.flag_delivery_failures
             FROM "AdRounds" round
             JOIN "Games" game ON game.id = round.game_id
            WHERE round.id = $1 AND round.game_id = $2
              AND round.finalized = FALSE
            FOR UPDATE OF round"#,
    )
    .bind(round_id)
    .bind(game_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((round_start, round_end, game_end, published_at, failure_count)) = round else {
        return Err(AppError::conflict(
            "flag-publication round is no longer active",
        ));
    };
    if published_at.is_some() {
        complete_replaced_service_results(tx, game_id, round_id).await?;
        return Ok(FlagDeliveryPublication { failure_count });
    }
    let now: chrono::DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if now >= round_end || now >= game_end {
        return Err(AppError::conflict(
            "flag delivery crossed the authoritative round deadline",
        ));
    }
    if outcomes.iter().any(|outcome| {
        outcome.completed_at < round_start
            || outcome.completed_at >= round_end
            || outcome.completed_at >= game_end
    }) {
        return Err(AppError::conflict(
            "flag-delivery evidence falls outside the scoring window",
        ));
    }

    if !outcomes.is_empty() {
        let service_ids: Vec<i32> = outcomes
            .iter()
            .map(|outcome| outcome.team_service_id)
            .collect();
        let roster: Vec<(i32, bool, Option<String>)> = sqlx::query_as(
            r#"SELECT service.id, NOT challenge.ad_self_hosted, service.container_id
                 FROM "AdFlags" flag
                 JOIN "AdTeamServices" service ON service.id = flag.team_service_id
                 JOIN "GameChallenges" challenge
                   ON challenge.id = service.challenge_id
                  AND challenge.game_id = service.game_id
                WHERE flag.round_id = $1
                  AND service.game_id = $2
                  AND service.id = ANY($3)
                ORDER BY service.id
                FOR SHARE OF service, challenge"#,
        )
        .bind(round_id)
        .bind(game_id)
        .bind(&service_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        let roster: HashMap<_, _> = roster
            .into_iter()
            .map(|(service_id, managed, container_id)| (service_id, (managed, container_id)))
            .collect();
        // Do not let one participant's reset/reconnect race roll back valid peer
        // receipts in the same batch. A produced outcome for a stale identity is
        // immutable participant evidence: that service began publication work,
        // then changed its target before the result committed. Missing/deleted
        // rows are skipped here and handled by final settlement without affecting
        // authoritative peers.
        let authoritative_outcomes: Vec<FlagDeliveryOutcome> = outcomes
            .iter()
            .filter_map(|outcome| {
                let (managed, container_id) = roster.get(&outcome.team_service_id)?;
                let identity_matches = *managed == (outcome.kind == FlagDeliveryKind::Managed)
                    && (!*managed || container_id == &outcome.container_id);
                if identity_matches {
                    return Some(outcome.clone());
                }
                Some(FlagDeliveryOutcome {
                    team_service_id: outcome.team_service_id,
                    kind: outcome.kind,
                    container_id: outcome.container_id.clone(),
                    delivered: false,
                    attempts: outcome.attempts.max(1),
                    failure_reason: Some(CHANGED_DURING_DELIVERY_REASON.to_string()),
                    completed_at: outcome.completed_at,
                })
            })
            .collect();
        let kinds: Vec<&str> = authoritative_outcomes
            .iter()
            .map(|outcome| outcome.kind.as_str())
            .collect();
        let container_ids: Vec<Option<&str>> = authoritative_outcomes
            .iter()
            .map(|outcome| outcome.container_id.as_deref())
            .collect();
        let delivered: Vec<bool> = authoritative_outcomes
            .iter()
            .map(|outcome| outcome.delivered)
            .collect();
        let attempts: Vec<i16> = authoritative_outcomes
            .iter()
            .map(|outcome| outcome.attempts)
            .collect();
        let reasons: Vec<Option<&str>> = authoritative_outcomes
            .iter()
            .map(|outcome| outcome.failure_reason.as_deref())
            .collect();
        let completed_at: Vec<chrono::DateTime<Utc>> = authoritative_outcomes
            .iter()
            .map(|outcome| outcome.completed_at)
            .collect();
        let service_ids: Vec<i32> = authoritative_outcomes
            .iter()
            .map(|outcome| outcome.team_service_id)
            .collect();
        sqlx::query(
            r#"INSERT INTO "AdFlagDeliveryResults"
                 (round_id, team_service_id, delivery_kind, container_id,
                  delivered, attempts, failure_reason, completed_at)
               SELECT $1, input.team_service_id, input.delivery_kind,
                      input.container_id, input.delivered, input.attempts,
                      input.failure_reason, input.completed_at
                 FROM UNNEST(
                   $2::integer[], $3::text[], $4::text[], $5::boolean[],
                   $6::smallint[], $7::text[], $8::timestamptz[]
                 ) AS input(
                   team_service_id, delivery_kind, container_id, delivered,
                   attempts, failure_reason, completed_at
                 )
               ON CONFLICT (round_id, team_service_id) DO NOTHING"#,
        )
        .bind(round_id)
        .bind(&service_ids)
        .bind(&kinds)
        .bind(&container_ids)
        .bind(&delivered)
        .bind(&attempts)
        .bind(&reasons)
        .bind(&completed_at)
        .execute(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }

    if let Some(reason) = settle_missing {
        insert_missing_outcomes(tx, game_id, round_id, reason, attempted_service_ids, now).await?;
        settle_round_publication(tx, game_id, round_id, now).await
    } else {
        let failure_count = sqlx::query_scalar(
            r#"SELECT COUNT(*)::integer FROM "AdFlagDeliveryResults"
                WHERE round_id = $1 AND delivered = FALSE"#,
        )
        .bind(round_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        Ok(FlagDeliveryPublication { failure_count })
    }
}

/// Persist one immutable service receipt as soon as its push completes. A
/// successful receipt is returned for the streaming checker handoff. Replays
/// return the already-committed receipt and never mutate its timestamp.
pub(crate) async fn record_flag_delivery_outcome_batch(
    db: &DatabaseConnection,
    game_id: i32,
    round_id: i32,
    outcomes: &[FlagDeliveryOutcome],
) -> AppResult<Vec<FlagDeliveryReceipt>> {
    if outcomes.is_empty() {
        return Ok(Vec::new());
    }
    let mut control = super::koth_auth::acquire_game_lock(db, game_id).await?;
    record_flag_delivery_outcomes_transaction(
        control.transaction_mut(),
        game_id,
        round_id,
        outcomes,
        None,
        &[],
    )
    .await?;
    let service_ids: Vec<i32> = outcomes
        .iter()
        .map(|outcome| outcome.team_service_id)
        .collect();
    let receipts = sqlx::query_as::<_, (i32, chrono::DateTime<Utc>)>(
        r#"SELECT team_service_id, completed_at
             FROM "AdFlagDeliveryResults"
            WHERE round_id = $1 AND team_service_id = ANY($2) AND delivered = TRUE
            ORDER BY team_service_id"#,
    )
    .bind(round_id)
    .bind(&service_ids)
    .fetch_all(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .into_iter()
    .map(|(team_service_id, completed_at)| FlagDeliveryReceipt {
        team_service_id,
        completed_at,
    })
    .collect();
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(receipts)
}

/// Close the bounded publication phase. Targets that never produced a receipt
/// are platform-attributed (`attempts = 0`); a target that consumed at least
/// one complete attempt already has an Offline receipt and stays in the SLA
/// denominator.
pub(crate) async fn settle_flag_delivery_outcomes(
    db: &DatabaseConnection,
    game_id: i32,
    round_id: i32,
    attempted_service_ids: &[i32],
) -> AppResult<FlagDeliveryPublication> {
    let mut control = super::koth_auth::acquire_game_lock(db, game_id).await?;
    let publication = record_flag_delivery_outcomes_transaction(
        control.transaction_mut(),
        game_id,
        round_id,
        &[],
        Some(PUBLICATION_DEADLINE_REASON),
        attempted_service_ids,
    )
    .await?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(publication)
}

pub(super) async fn complete_missing_flag_delivery_outcomes_transaction(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    game_id: i32,
    round_id: i32,
) -> AppResult<()> {
    let completed_at: Option<chrono::DateTime<Utc>> = sqlx::query_scalar(
        r#"SELECT LEAST(clock_timestamp(), round.end_time_utc, game.end_time_utc)
             FROM "AdRounds" round
             JOIN "Games" game ON game.id = round.game_id
            WHERE round.id = $1 AND round.game_id = $2
              AND round.finalized = FALSE
            FOR UPDATE OF round"#,
    )
    .bind(round_id)
    .bind(game_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(completed_at) = completed_at else {
        return Ok(());
    };
    insert_missing_outcomes(
        tx,
        game_id,
        round_id,
        EXPIRED_DELIVERY_REASON,
        &[],
        completed_at,
    )
    .await?;
    settle_round_publication(tx, game_id, round_id, completed_at)
        .await
        .map(|_| ())
}

#[cfg(test)]
#[path = "flag_delivery/tests.rs"]
mod tests;
