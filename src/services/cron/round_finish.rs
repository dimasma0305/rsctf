use crate::app_state::SharedState;
use crate::models::data::game;
use crate::utils::error::AppResult;
use futures::StreamExt;

const FLAG_DELIVERY_BATCH_SIZE: usize = 64;
const FLAG_DELIVERY_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);
const MANAGED_BACKEND_FAILURE_REASON: &str =
    "managed container backend unavailable during flag delivery";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeliveryAttempt {
    Delivered,
    ParticipantFailure,
    PlatformFailure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DeliveryAttemptSummary {
    delivered: bool,
    participant_attempts: usize,
    saw_platform_failure: bool,
}

#[derive(Default)]
struct DeliveryAttemptTracker {
    /// `false` is provisional work whose backend admission is tracked by its
    /// guard. Cancellation after admission promotes it to participant evidence;
    /// pre-admission cancellation or a completed typed platform failure removes
    /// it unless an earlier participant attempt already completed.
    services: std::sync::Mutex<std::collections::HashMap<i32, bool>>,
}

impl DeliveryAttemptTracker {
    fn begin(
        &self,
        service_id: i32,
        admission: crate::services::container::ContainerExecAdmission,
    ) -> DeliveryAttemptGuard<'_> {
        self.services.lock().unwrap().entry(service_id).or_default();
        DeliveryAttemptGuard {
            tracker: self,
            service_id,
            admission,
            completed: false,
        }
    }

    fn participant_completed(&self, service_id: i32) {
        self.services.lock().unwrap().insert(service_id, true);
    }

    fn platform_completed(&self, service_id: i32) {
        let mut services = self.services.lock().unwrap();
        if services.get(&service_id) == Some(&false) {
            services.remove(&service_id);
        }
    }

    fn service_ids(&self) -> Vec<i32> {
        self.services.lock().unwrap().keys().copied().collect()
    }
}

struct DeliveryAttemptGuard<'a> {
    tracker: &'a DeliveryAttemptTracker,
    service_id: i32,
    admission: crate::services::container::ContainerExecAdmission,
    completed: bool,
}

impl DeliveryAttemptGuard<'_> {
    fn participant(mut self) {
        self.tracker.participant_completed(self.service_id);
        self.completed = true;
    }

    fn platform(mut self) {
        self.tracker.platform_completed(self.service_id);
        self.completed = true;
    }
}

impl Drop for DeliveryAttemptGuard<'_> {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        if self.admission.is_admitted() {
            self.tracker.participant_completed(self.service_id);
        } else {
            self.tracker.platform_completed(self.service_id);
        }
    }
}

async fn run_delivery_attempts<F, Fut>(
    policy: crate::services::ad_engine::FlagDeliveryPolicy,
    deadline: tokio::time::Instant,
    concurrency: &tokio::sync::Semaphore,
    mut attempt_delivery: F,
) -> DeliveryAttemptSummary
where
    F: FnMut(crate::services::container::ContainerExecAdmission) -> Fut,
    Fut: std::future::Future<Output = DeliveryAttempt>,
{
    let mut started_attempts = 0;
    let mut participant_attempts = 0;
    let mut saw_platform_failure = false;
    while started_attempts < policy.attempts() {
        let Some(latest_start) = deadline.checked_sub(policy.attempt_timeout()) else {
            break;
        };
        let permit = match tokio::time::timeout_at(latest_start, concurrency.acquire()).await {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) | Err(_) => break,
        };
        let now = tokio::time::Instant::now();
        if deadline.saturating_duration_since(now) < policy.attempt_timeout() {
            // No shortened probe is started. If a prior full attempt consumed
            // the window this remains participant-attributed (attempts > 0);
            // capacity starvation before attempt one is platform-attributed.
            break;
        }
        started_attempts += 1;
        let attempt_deadline = (now + policy.attempt_timeout()).min(deadline);
        let admission = crate::services::container::ContainerExecAdmission::default();
        let attempt =
            tokio::time::timeout_at(attempt_deadline, attempt_delivery(admission.clone())).await;
        drop(permit);
        match attempt {
            Ok(DeliveryAttempt::Delivered) => {
                participant_attempts += 1;
                return DeliveryAttemptSummary {
                    delivered: true,
                    participant_attempts,
                    saw_platform_failure,
                };
            }
            Ok(DeliveryAttempt::ParticipantFailure) => participant_attempts += 1,
            Ok(DeliveryAttempt::PlatformFailure) => saw_platform_failure = true,
            Err(_) if admission.is_admitted() => participant_attempts += 1,
            Err(_) => saw_platform_failure = true,
        }
        if started_attempts == policy.attempts() {
            break;
        }
        let backoff = policy.retry_backoff(started_attempts);
        if deadline.saturating_duration_since(tokio::time::Instant::now())
            < backoff.saturating_add(policy.attempt_timeout())
        {
            break;
        }
        tokio::time::sleep(backoff).await;
    }
    DeliveryAttemptSummary {
        delivered: false,
        participant_attempts,
        saw_platform_failure,
    }
}

async fn deliver_external_flag(
    state: &SharedState,
    round_id: i32,
    planted: &crate::services::ad_engine::AdvancedRoundFlag,
    policy: crate::services::ad_engine::FlagDeliveryPolicy,
    deadline: tokio::time::Instant,
    concurrency: &tokio::sync::Semaphore,
    attempted_services: &DeliveryAttemptTracker,
) -> DeliveryAttemptSummary {
    run_delivery_attempts(policy, deadline, concurrency, |admission| {
        let guard = attempted_services.begin(planted.team_service_id, admission.clone());
        async move {
            // BYOC already has its own application-level ACK. Preserve its
            // existing participant attribution once the push operation starts.
            admission.mark_admitted();
            let delivered = crate::services::byoc_tunnel::push_flag(
                state,
                planted.participation_id,
                planted.challenge_id,
                round_id,
                &planted.flag,
            )
            .await;
            guard.participant();
            if delivered {
                DeliveryAttempt::Delivered
            } else {
                DeliveryAttempt::ParticipantFailure
            }
        }
    })
    .await
}

async fn deliver_managed_flag(
    state: &SharedState,
    planted: &crate::services::ad_engine::AdvancedRoundFlag,
    container_id: &str,
    policy: crate::services::ad_engine::FlagDeliveryPolicy,
    deadline: tokio::time::Instant,
    concurrency: &tokio::sync::Semaphore,
    attempted_services: &DeliveryAttemptTracker,
) -> DeliveryAttemptSummary {
    run_delivery_attempts(policy, deadline, concurrency, |admission| {
        let guard = attempted_services.begin(planted.team_service_id, admission.clone());
        let command = managed_flag_command(&planted.flag);
        async move {
            match state
                .containers
                .exec_classified(container_id, command, admission)
                .await
            {
                Ok(output) => {
                    guard.participant();
                    if output.trim_end_matches(['\r', '\n']) == planted.flag {
                        DeliveryAttempt::Delivered
                    } else {
                        DeliveryAttempt::ParticipantFailure
                    }
                }
                Err(crate::services::container::ContainerExecError::Participant(error)) => {
                    guard.participant();
                    tracing::debug!(
                        service = planted.team_service_id,
                        %error,
                        "cron: managed flag target rejected exec"
                    );
                    DeliveryAttempt::ParticipantFailure
                }
                Err(crate::services::container::ContainerExecError::Platform(error)) => {
                    guard.platform();
                    tracing::warn!(
                        service = planted.team_service_id,
                        %error,
                        "cron: managed flag container backend failed"
                    );
                    DeliveryAttempt::PlatformFailure
                }
            }
        }
    })
    .await
}

fn managed_flag_command(flag: &str) -> Vec<String> {
    // The flag is passed as argv[1], never interpolated into the shell
    // program. Existing containers without RSCTF_FLAG_FILE use /flag.
    vec![
        "sh".to_string(),
        "-c".to_string(),
        "umask 022 && printf '%s\\n' \"$1\" > \"${RSCTF_FLAG_FILE:-/flag}\" && cat \"${RSCTF_FLAG_FILE:-/flag}\"".to_string(),
        "rsctf-flag".to_string(),
        flag.to_string(),
    ]
}

async fn deliver_round_flag(
    state: &SharedState,
    round_id: i32,
    planted: crate::services::ad_engine::AdvancedRoundFlag,
    policy: crate::services::ad_engine::FlagDeliveryPolicy,
    deadline: tokio::time::Instant,
    concurrency: std::sync::Arc<tokio::sync::Semaphore>,
    attempted_services: std::sync::Arc<DeliveryAttemptTracker>,
) -> crate::services::ad_engine::FlagDeliveryOutcome {
    let kind = if planted.managed {
        crate::services::ad_engine::FlagDeliveryKind::Managed
    } else {
        crate::services::ad_engine::FlagDeliveryKind::External
    };
    let container_id = planted
        .container_id
        .as_deref()
        .filter(|id| !id.trim().is_empty());
    let attempted = match (planted.managed, container_id) {
        (true, Some(container_id)) => {
            deliver_managed_flag(
                state,
                &planted,
                container_id,
                policy,
                deadline,
                &concurrency,
                &attempted_services,
            )
            .await
        }
        (true, None) => {
            return crate::services::ad_engine::FlagDeliveryOutcome::failed(
                planted.team_service_id,
                kind,
                None,
                0,
                "managed service has no active container identity",
            );
        }
        (false, _) => {
            deliver_external_flag(
                state,
                round_id,
                &planted,
                policy,
                deadline,
                &concurrency,
                &attempted_services,
            )
            .await
        }
    };
    if attempted.delivered {
        crate::services::ad_engine::FlagDeliveryOutcome::succeeded(
            planted.team_service_id,
            kind,
            planted.managed.then(|| container_id.unwrap().to_string()),
            attempted.participant_attempts,
        )
    } else {
        crate::services::ad_engine::FlagDeliveryOutcome::failed(
            planted.team_service_id,
            kind,
            planted.managed.then(|| container_id.unwrap().to_string()),
            attempted.participant_attempts,
            if attempted.participant_attempts == 0 && attempted.saw_platform_failure {
                MANAGED_BACKEND_FAILURE_REASON
            } else if attempted.participant_attempts == 0 {
                crate::services::ad_engine::PUBLICATION_DEADLINE_REASON
            } else if planted.managed {
                "managed container rejected the round flag"
            } else {
                "BYOC tunnel did not acknowledge the round flag"
            },
        )
    }
}

async fn deliver_initial_round_flags<F, Fut>(
    flags: Vec<crate::services::ad_engine::AdvancedRoundFlag>,
    publication_settled: bool,
    deliver: F,
    sender: tokio::sync::mpsc::Sender<crate::services::ad_engine::FlagDeliveryOutcome>,
) -> AppResult<()>
where
    F: Fn(crate::services::ad_engine::AdvancedRoundFlag) -> Fut,
    Fut: std::future::Future<Output = crate::services::ad_engine::FlagDeliveryOutcome>,
{
    if publication_settled {
        return Ok(());
    }
    let pending = flags.len().max(1);
    let mut pushes = futures::stream::iter(flags)
        .map(deliver)
        .buffer_unordered(pending);
    while let Some(outcome) = pushes.next().await {
        if sender.send(outcome).await.is_err() {
            return Err(crate::utils::error::AppError::internal(
                "flag-delivery receipt writer stopped before publication completed",
            ));
        }
    }
    Ok(())
}

async fn drain_flag_delivery_outcomes(
    state: &SharedState,
    game_id: i32,
    round_id: i32,
    mut receiver: tokio::sync::mpsc::Receiver<crate::services::ad_engine::FlagDeliveryOutcome>,
    receipt_sender: tokio::sync::mpsc::UnboundedSender<
        crate::services::ad_engine::FlagDeliveryReceipt,
    >,
) -> AppResult<()> {
    let mut closed = false;
    while !closed {
        let Some(first) = receiver.recv().await else {
            break;
        };
        let mut batch = Vec::with_capacity(FLAG_DELIVERY_BATCH_SIZE);
        batch.push(first);
        let flush_at = tokio::time::Instant::now() + FLAG_DELIVERY_FLUSH_INTERVAL;
        while batch.len() < FLAG_DELIVERY_BATCH_SIZE {
            match tokio::time::timeout_at(flush_at, receiver.recv()).await {
                Ok(Some(outcome)) => batch.push(outcome),
                Ok(None) => {
                    closed = true;
                    break;
                }
                Err(_) => break,
            }
        }
        let receipts = crate::services::ad_engine::record_flag_delivery_outcome_batch(
            &state.db, game_id, round_id, &batch,
        )
        .await?;
        // Checker failure must not prevent publication evidence from settling.
        // The owning pipeline reports the checker error separately.
        for receipt in receipts {
            let _ = receipt_sender.send(receipt);
        }
    }
    Ok(())
}

fn wall_deadline_to_instant(
    deadline: chrono::DateTime<chrono::Utc>,
    wall_now: chrono::DateTime<chrono::Utc>,
    monotonic_now: tokio::time::Instant,
) -> tokio::time::Instant {
    monotonic_now
        + deadline
            .signed_duration_since(wall_now)
            .to_std()
            .unwrap_or_default()
}

fn delivery_order_key(seed: u64, service_id: i32) -> u64 {
    // SplitMix64 supplies a cheap keyed permutation for the small in-memory
    // roster. A fresh seed per publication prevents stable service-id order from
    // deciding which targets reach bounded admission first under saturation.
    let mut value = seed ^ u64::from(service_id as u32);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn cap_flag_publication_deadlines(
    policy: crate::services::ad_engine::FlagDeliveryPolicy,
    publication_from_anchor: tokio::time::Instant,
    delivery_from_anchor: tokio::time::Instant,
    pipeline_deadline: tokio::time::Instant,
    grace_seconds: i32,
) -> (tokio::time::Instant, tokio::time::Instant) {
    // `pipeline_deadline` already preserves the scheduler's one-second outer
    // margin. Keep the configured grace plus the minimum jitter/probe/durable
    // checker runway ahead of it, even if repair consumed part of this tick.
    let checker_reserve = std::time::Duration::from_secs(
        u64::try_from(grace_seconds.clamp(1, 60)).unwrap_or(3)
            + crate::services::ad_engine::CHECKER_MINIMUM_RUNWAY_SECONDS,
    );
    let checker_safe_cap = pipeline_deadline
        .checked_sub(checker_reserve)
        .unwrap_or_else(tokio::time::Instant::now);
    let publication_deadline = publication_from_anchor
        .min(checker_safe_cap)
        .min(pipeline_deadline);
    let delivery_deadline = delivery_from_anchor.min(publication_deadline);
    debug_assert!(
        delivery_deadline <= publication_deadline,
        "delivery work must finish before receipt settlement"
    );
    debug_assert_eq!(
        policy.publication_reserve().as_secs(),
        crate::services::ad_engine::FLAG_DELIVERY_PUBLICATION_RESERVE_SECONDS
    );
    (publication_deadline, delivery_deadline)
}

#[allow(clippy::too_many_arguments)]
async fn publish_round_flags(
    state: &SharedState,
    game_id: i32,
    round_id: i32,
    round_number: i32,
    publication_anchor: chrono::DateTime<chrono::Utc>,
    grace_seconds: i32,
    mut flags: Vec<crate::services::ad_engine::AdvancedRoundFlag>,
    publication_settled: bool,
    pipeline_deadline: tokio::time::Instant,
    receipt_sender: tokio::sync::mpsc::UnboundedSender<
        crate::services::ad_engine::FlagDeliveryReceipt,
    >,
) -> AppResult<crate::services::ad_engine::FlagDeliveryPublication> {
    let policy = crate::services::ad_engine::FlagDeliveryPolicy::from_env()
        .map_err(crate::utils::error::AppError::internal)?;
    if !publication_settled {
        let recorded: std::collections::HashSet<i32> = sqlx::query_scalar::<_, i32>(
            r#"SELECT team_service_id FROM "AdFlagDeliveryResults" WHERE round_id = $1"#,
        )
        .bind(round_id)
        .fetch_all(state.pg())
        .await
        .map_err(|error| crate::utils::error::AppError::internal(error.to_string()))?
        .into_iter()
        .collect();
        flags.retain(|flag| !recorded.contains(&flag.team_service_id));
    }
    let order_seed = rand::random::<u64>();
    flags.sort_unstable_by_key(|flag| delivery_order_key(order_seed, flag.team_service_id));

    let wall_now = chrono::Utc::now();
    let monotonic_now = tokio::time::Instant::now();
    let publication_from_anchor = wall_deadline_to_instant(
        publication_anchor
            + chrono::Duration::from_std(policy.publication_reserve()).unwrap_or_default(),
        wall_now,
        monotonic_now,
    );
    let delivery_from_anchor = wall_deadline_to_instant(
        publication_anchor
            + chrono::Duration::from_std(policy.worst_case_attempt_window()).unwrap_or_default(),
        wall_now,
        monotonic_now,
    );
    let (publication_deadline, delivery_deadline) = cap_flag_publication_deadlines(
        policy,
        publication_from_anchor,
        delivery_from_anchor,
        pipeline_deadline,
        grace_seconds,
    );
    let capacity = policy.concurrency().saturating_mul(2).clamp(1, 512);
    let (outcome_sender, outcome_receiver) = tokio::sync::mpsc::channel(capacity);
    let delivery_concurrency =
        std::sync::Arc::new(tokio::sync::Semaphore::new(policy.concurrency()));
    let attempted_services = std::sync::Arc::new(DeliveryAttemptTracker::default());
    let producer = deliver_initial_round_flags(
        flags,
        publication_settled,
        |flag| {
            deliver_round_flag(
                state,
                round_id,
                flag,
                policy,
                delivery_deadline,
                delivery_concurrency.clone(),
                attempted_services.clone(),
            )
        },
        outcome_sender,
    );
    let writer =
        drain_flag_delivery_outcomes(state, game_id, round_id, outcome_receiver, receipt_sender);
    let publication_work = tokio::time::timeout_at(publication_deadline, async {
        tokio::try_join!(producer, writer).map(|_| ())
    })
    .await;
    let delivery_error = match publication_work {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error),
        Err(_) => {
            tracing::warn!(
                game = game_id,
                round = round_number,
                reserve_seconds = policy.publication_reserve().as_secs(),
                "cron: flag publication reached its absolute deadline"
            );
            None
        }
    };
    let mut attempted_service_ids = attempted_services.service_ids();
    attempted_service_ids.sort_unstable();
    let publication = crate::services::ad_engine::settle_flag_delivery_outcomes(
        &state.db,
        game_id,
        round_id,
        &attempted_service_ids,
    )
    .await?;
    if let Some(error) = delivery_error {
        return Err(error);
    }
    Ok(publication)
}

pub(super) enum PipelineDrive {
    Complete,
    InFlight,
    Finished,
    Expired,
}

const SCORE_ROLLUP_REFRESH_ATTEMPTS: usize = 3;

/// Refresh derived epoch prefixes after authoritative evidence is sealed. This
/// deliberately runs outside player requests; the advisory rollup writers are
/// idempotent and this single-flight collapses same-replica retry queues.
pub(super) async fn refresh_score_rollups(state: &SharedState, game_id: i32) -> bool {
    static SCORE_ROLLUP_REFRESH: std::sync::LazyLock<
        crate::utils::single_flight::SingleFlight<bool>,
    > = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);
    let key = format!("score-rollups:{game_id}");
    for attempt in 1..=SCORE_ROLLUP_REFRESH_ATTEMPTS {
        let state = state.clone();
        let complete = SCORE_ROLLUP_REFRESH
            .run(&key, move || async move {
                let now = chrono::Utc::now();
                let ad_complete = match crate::services::ad::scoring::refresh_epoch_rollups(
                    state.pg(),
                    game_id,
                    now,
                )
                .await
                {
                    Ok(()) => true,
                    Err(error) => {
                        tracing::warn!(game = game_id, %error, "cron: A&D rollup refresh failed");
                        false
                    }
                };
                let koth_complete = match crate::controllers::game::koth::refresh_epoch_rollups(
                    state.pg(),
                    game_id,
                    now,
                )
                .await
                {
                    Ok(()) => true,
                    Err(error) => {
                        tracing::warn!(game = game_id, %error, "cron: KotH rollup refresh failed");
                        false
                    }
                };
                ad_complete && koth_complete
            })
            .await;
        if complete {
            return true;
        }
        if attempt < SCORE_ROLLUP_REFRESH_ATTEMPTS {
            tokio::time::sleep(std::time::Duration::from_millis(250 * attempt as u64)).await;
        }
    }
    tracing::warn!(
        game = game_id,
        attempts = SCORE_ROLLUP_REFRESH_ATTEMPTS,
        "cron: score rollup refresh exhausted retries"
    );
    false
}

fn schedule_score_rollup_refresh(state: &SharedState, game_id: i32) {
    let state = state.clone();
    tokio::spawn(async move {
        let _ = refresh_score_rollups(&state, game_id).await;
    });
}

/// Claim and finish one durable round. A caller may pass the snapshot it just
/// prepared; recovery callers rebuild the same snapshot from committed rows.
pub(super) async fn drive_round_pipeline(
    state: &SharedState,
    game: &game::Model,
    round_id: i32,
    prepared: Option<crate::services::ad_engine::AdvancedRound>,
    budget: std::time::Duration,
) -> AppResult<PipelineDrive> {
    let lease =
        match crate::services::ad_engine::claim_round_finish(&state.db, game.id, round_id).await? {
            crate::services::ad_engine::RoundFinishDisposition::Complete => {
                schedule_score_rollup_refresh(state, game.id);
                return Ok(PipelineDrive::Complete);
            }
            crate::services::ad_engine::RoundFinishDisposition::InFlight => {
                return Ok(PipelineDrive::InFlight);
            }
            crate::services::ad_engine::RoundFinishDisposition::Claimed(lease) => lease,
        };
    let prepared = match prepared {
        Some(prepared) => prepared,
        None => {
            crate::services::ad_engine::prepared_round_snapshot(&state.db, game.id, round_id)
                .await?
        }
    };
    let round_number = prepared.number;
    if prepared.ends_at <= chrono::Utc::now() {
        return if crate::services::ad_engine::expire_overdue_round_finish(
            &state.db, game.id, round_id, &lease,
        )
        .await?
        {
            schedule_score_rollup_refresh(state, game.id);
            Ok(PipelineDrive::Expired)
        } else {
            // Another owner replaced or completed this lease. Never start live
            // flag/checker/reset work with a stale deadline token.
            Ok(PipelineDrive::InFlight)
        };
    }
    let pipeline_deadline = tokio::time::Instant::now() + budget;
    let finished = tokio::time::timeout(
        budget,
        finish_prepared_round(state, game, prepared, &lease, pipeline_deadline),
    )
    .await;
    let error = match finished {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error),
        Err(_) => {
            if crate::services::ad_engine::expire_overdue_round_finish(
                &state.db, game.id, round_id, &lease,
            )
            .await?
            {
                schedule_score_rollup_refresh(state, game.id);
                return Ok(PipelineDrive::Expired);
            }
            Some(crate::utils::error::AppError::internal(format!(
                "round {round_number} finishing exceeded its execution budget"
            )))
        }
    };
    if let Some(error) = error {
        if let Err(release_error) =
            crate::services::ad_engine::abandon_round_finish(&state.db, game.id, round_id, &lease)
                .await
        {
            tracing::warn!(
                game = game.id,
                round = round_number,
                %release_error,
                "cron: failed to release errored round pipeline lease"
            );
        }
        return Err(error);
    }
    crate::services::ad_engine::complete_round_finish(&state.db, game.id, round_id, &lease).await?;
    schedule_score_rollup_refresh(state, game.id);
    Ok(PipelineDrive::Finished)
}

/// Finish a prepared round by planting external flags, probing services, and
/// publishing the fresh board state. The caller owns the durable pipeline lease.
pub(super) async fn finish_prepared_round(
    state: &SharedState,
    game: &game::Model,
    prepared: crate::services::ad_engine::AdvancedRound,
    lease: &crate::services::ad_engine::RoundFinishLease,
    pipeline_deadline: tokio::time::Instant,
) -> AppResult<()> {
    let round_id = prepared.id;
    let next_number = prepared.number;
    let flags_planted = prepared.flags.len() as i32;
    let newly_prepared = prepared.created;

    // The scheduler repairs managed services before creating a new round, so
    // ordinary runtime latency is outside its scoring window. A crash-recovery
    // owner must repeat readiness work because it cannot trust the old runtime
    // identity. Reload the snapshot either way and never exec a stale container.
    if !newly_prepared {
        let repair_failures =
            match crate::controllers::edit::ensure_ad_containers(state, game, None, false, false)
                .await
            {
                Ok((_, failures)) => failures as usize,
                Err(error) => {
                    tracing::warn!(
                        game = game.id,
                        round = next_number,
                        %error,
                        "cron: managed A&D service recovery failed before flag propagation"
                    );
                    1
                }
            };
        if repair_failures > 0 {
            tracing::warn!(
                game = game.id,
                round = next_number,
                failed = repair_failures,
                "cron: managed A&D service recovery was incomplete before flag propagation"
            );
        }
    }
    let publication =
        crate::services::ad_engine::prepared_round_snapshot(&state.db, game.id, round_id).await?;
    let publication_settled: bool = sqlx::query_scalar(
        r#"SELECT flags_published_at IS NOT NULL
             FROM "AdRounds" WHERE id = $1 AND game_id = $2"#,
    )
    .bind(round_id)
    .bind(game.id)
    .fetch_one(state.pg())
    .await
    .map_err(|error| crate::utils::error::AppError::internal(error.to_string()))?;
    // Crown-cycle hills cross their durable reset state machine before the
    // checker starts. A reset/readiness failure leaves the cycle outside
    // `Active`, so the checker records no participant-attributed sample and the
    // round remains a field-wide void for that hill.
    if let Err(error) = crate::services::ad_engine::koth_cycle::drive_cycle_transitions(
        state,
        game.id,
        round_id,
        next_number,
    )
    .await
    {
        tracing::warn!(
            game = game.id,
            round = next_number,
            %error,
            "cron: KotH crown-cycle transition remains pending"
        );
    }

    // A newly prepared round begins publication after bounded KotH transition
    // work. Its persisted boundary was captured after managed readiness; the
    // pipeline deadline still caps any transition delay. Recovery retains the
    // persisted anchor so repeated crashes cannot extend publication indefinitely.
    let publication_anchor = if newly_prepared {
        chrono::Utc::now()
    } else {
        publication.started_at
    };
    let grace_seconds = game
        .ad_min_grace_period_seconds
        .unwrap_or(crate::services::ad_engine::DEFAULT_CHECKER_GRACE_SECONDS);

    // Stream each durable successful delivery receipt directly to the checker.
    // It schedules from that service's immutable completion time while other
    // targets are still retrying, so one slow participant is not a global
    // publication barrier. The publisher still closes every missing receipt at
    // its seven-second absolute deadline.
    let (receipt_sender, receipt_receiver) = tokio::sync::mpsc::unbounded_channel();
    let publisher = publish_round_flags(
        state,
        game.id,
        round_id,
        next_number,
        publication_anchor,
        grace_seconds,
        publication.flags,
        publication_settled,
        pipeline_deadline,
        receipt_sender,
    );
    let checker = crate::services::ad_engine::run_checker(
        &state.db,
        state.containers.as_ref(),
        game.id,
        round_id,
        lease,
        pipeline_deadline,
        receipt_receiver,
    );
    let (delivery, checker) = tokio::join!(publisher, checker);
    let delivery = delivery?;
    if delivery.failure_count > 0 {
        tracing::warn!(
            game = game.id,
            round = next_number,
            failed = delivery.failure_count,
            "cron: some A&D services did not accept the new flag"
        );
    }
    if let Err(error) = checker {
        tracing::warn!(
            game = game.id,
            round = next_number,
            "cron: SLA checker failed (round advanced anyway): {error}"
        );
        return Err(error);
    }

    // Repair only after the KotH checker persisted its sample. Held targets in an
    // active scoring game require a matching dead-container receipt, so a partial
    // checker failure cannot silently clear responsibility.
    if let Err(error) = crate::controllers::game::koth::ensure_koth_hills(state, game.id).await {
        tracing::warn!(
            game = game.id,
            round = next_number,
            %error,
            "cron: post-check KotH repair failed"
        );
    }

    state
        .cache
        .remove(&format!("_AdScoreBoard_{}", game.id))
        .await;
    state
        .cache
        .remove(&format!("_AdScoreBoardFrozen_{}", game.id))
        .await;
    state
        .cache
        .remove(&format!("_KothScoreBoard_{}", game.id))
        .await;
    state
        .cache
        .remove(&format!("_KothScoreBoardFrozen_{}", game.id))
        .await;
    state
        .cache
        .remove(&format!("_KothTimeline_{}", game.id))
        .await;
    state
        .cache
        .remove(&format!("_KothTimelineFrozen_{}", game.id))
        .await;
    if let Ok(challenge_ids) =
        sqlx::query_scalar::<_, i32>(r#"SELECT challenge_id FROM "KothTargets" WHERE game_id = $1"#)
            .bind(game.id)
            .fetch_all(state.pg())
            .await
    {
        for challenge_id in challenge_ids {
            state
                .cache
                .remove(&format!("_KothHillState_{}_{}", game.id, challenge_id))
                .await;
        }
    }

    state.publish_event(
        "ReceivedAttack",
        Some(game.id),
        serde_json::json!({
            "type": "adRoundAdvance",
            "gameId": game.id,
            "round": next_number,
            "flagsPlanted": flags_planted,
        })
        .to_string(),
    );
    tracing::debug!(
        game = game.id,
        round = next_number,
        flags = flags_planted,
        "cron: A&D round advanced"
    );
    Ok(())
}

#[cfg(test)]
#[path = "round_finish/tests.rs"]
mod tests;
