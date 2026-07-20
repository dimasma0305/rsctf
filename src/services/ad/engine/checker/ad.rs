//! Streaming per-service A&D checker scheduling.

use super::super::persistence::AdProbeResult;
use super::*;

pub(super) const PENDING_AD_SERVICES_SQL: &str = r#"SELECT result.team_service_id, delivery.completed_at
         FROM "AdCheckResults" result
         JOIN "AdFlags" flag
           ON flag.round_id = result.round_id
          AND flag.team_service_id = result.team_service_id
         JOIN "AdFlagDeliveryResults" delivery
           ON delivery.round_id = flag.round_id
          AND delivery.team_service_id = flag.team_service_id
         JOIN "AdTeamServices" service
           ON service.id = result.team_service_id
          AND service.game_id = $2
        WHERE result.round_id = $1
          AND result.sla_credit IS NULL
          AND delivery.delivered = TRUE
          AND (delivery.delivery_kind <> 'Managed'
               OR delivery.container_id IS NOT DISTINCT FROM service.container_id)"#;

#[derive(Clone, Copy)]
pub(super) struct AdCheckerTiming {
    pub(super) round_number: i32,
    pub(super) tick_seconds: i32,
    pub(super) grace_seconds: i32,
    pub(super) window_fraction: f64,
    pub(super) current_probe_cap: Option<std::time::Duration>,
    pub(super) nominal_probe_budget: Option<std::time::Duration>,
    pub(super) effective_deadline: tokio::time::Instant,
}

#[derive(Debug)]
struct AdProbeInput {
    service_id: i32,
    host: String,
    port: i32,
    participation_id: i32,
    challenge_id: i32,
    container_id: Option<String>,
    checker_dir: Option<String>,
    flag: Option<String>,
    schedule: Option<CheckerSchedule>,
    starts_at: tokio::time::Instant,
    no_window_status: AdCheckStatus,
    timeout_is_platform_limited: bool,
}

async fn run_scheduled_probe(input: AdProbeInput, timing: AdCheckerTiming) -> AdProbeResult {
    let flag_aware =
        input.checker_dir.is_some() && input.flag.as_deref().is_some_and(|value| !value.is_empty());
    let can_start = input.schedule.is_some_and(|schedule| {
        checker_probe_can_start(
            timing.effective_deadline,
            schedule.probe_budget,
            CHECKER_PERSISTENCE_MARGIN,
            tokio::time::Instant::now(),
        )
    });
    let (status, message) = match (input.schedule, can_start) {
        (Some(schedule), true) => {
            run_check(
                input.checker_dir.as_deref(),
                &input.host,
                input.port,
                timing.round_number,
                input.participation_id,
                input.challenge_id,
                input.flag.as_deref(),
                schedule.probe_budget,
                input.timeout_is_platform_limited,
            )
            .await
        }
        (None, _) => (
            input.no_window_status,
            Some(
                if input.no_window_status == AdCheckStatus::Offline {
                    "flag delivery completed too late for a full checker probe"
                } else {
                    "checker has no safe grace/jitter/execution window"
                }
                .into(),
            ),
        ),
        (Some(_), false) => (
            AdCheckStatus::InternalError,
            Some("checker capacity exhausted before safe probe runway".into()),
        ),
    };
    AdProbeResult {
        service_id: input.service_id,
        participation_id: input.participation_id,
        challenge_id: input.challenge_id,
        host: input.host,
        port: input.port,
        container_id: input.container_id,
        status,
        message,
        flag_verified: flag_aware && status == AdCheckStatus::Ok,
        observed_at: Utc::now(),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn check_services(
    db: &DatabaseConnection,
    services: Vec<ad_team_service::Model>,
    game_id: i32,
    round_id: i32,
    lease: &RoundFinishLease,
    checker_dirs: std::collections::HashMap<i32, Option<String>>,
    timing: AdCheckerTiming,
    mut delivery_receipts: tokio::sync::mpsc::UnboundedReceiver<FlagDeliveryReceipt>,
) -> AppResult<()> {
    if services.is_empty() {
        return Ok(());
    }
    let service_ids: Vec<i32> = services.iter().map(|service| service.id).collect();
    let flags: std::collections::HashMap<i32, String> = ad_flag::Entity::find()
        .filter(ad_flag::Column::RoundId.eq(round_id))
        .filter(ad_flag::Column::TeamServiceId.is_in(service_ids))
        .all(db)
        .await?
        .into_iter()
        .map(|flag| (flag.team_service_id, flag.flag))
        .collect();
    let pending_receipts: Vec<FlagDeliveryReceipt> =
        sqlx::query_as::<_, (i32, chrono::DateTime<Utc>)>(PENDING_AD_SERVICES_SQL)
            .bind(round_id)
            .bind(game_id)
            .fetch_all(db.get_postgres_connection_pool())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
            .into_iter()
            .map(|(team_service_id, completed_at)| FlagDeliveryReceipt {
                team_service_id,
                completed_at,
            })
            .collect();
    let services: std::collections::HashMap<_, _> = services
        .into_iter()
        .map(|service| (service.id, service))
        .collect();

    // The producer and writer share the pipeline lifetime. Cancellation drops
    // both, leaving NULL placeholders for the next exact lease owner.
    let (result_tx, result_rx) = tokio::sync::mpsc::unbounded_channel();
    let producer = async move {
        let make_input = |receipt: FlagDeliveryReceipt| {
            let service = services.get(&receipt.team_service_id)?;
            let wall_now = Utc::now();
            let monotonic_now = tokio::time::Instant::now();
            let available_after_delivery = checker_available_after_delivery(
                receipt.completed_at,
                wall_now,
                monotonic_now,
                timing.effective_deadline,
            );
            let schedule = timing.current_probe_cap.and_then(|probe_cap| {
                checker_schedule(
                    timing.tick_seconds,
                    timing.grace_seconds,
                    timing.window_fraction,
                    available_after_delivery,
                    probe_cap,
                )
            });
            let starts_at = schedule.map_or(monotonic_now, |schedule| {
                checker_start_instant(
                    receipt.completed_at,
                    random_checker_delay(schedule),
                    wall_now,
                    monotonic_now,
                )
            });
            let timeout_is_platform_limited =
                match (timing.current_probe_cap, timing.nominal_probe_budget) {
                    (Some(planned), Some(nominal)) => {
                        probe_budget_is_platform_limited(planned, nominal)
                    }
                    _ => true,
                };
            Some(AdProbeInput {
                service_id: service.id,
                host: service.host.clone(),
                port: service.port,
                participation_id: service.participation_id,
                challenge_id: service.challenge_id,
                container_id: service.container_id.clone(),
                checker_dir: checker_dirs.get(&service.challenge_id).cloned().flatten(),
                flag: flags.get(&service.id).cloned(),
                schedule,
                starts_at,
                no_window_status: if timing.current_probe_cap.is_some() {
                    AdCheckStatus::Offline
                } else {
                    AdCheckStatus::InternalError
                },
                timeout_is_platform_limited,
            })
        };
        let mut seen = std::collections::HashSet::new();
        let mut scheduled = std::collections::BTreeMap::new();
        let mut sequence = 0_u64;
        for receipt in pending_receipts {
            if seen.insert(receipt.team_service_id) {
                if let Some(input) = make_input(receipt) {
                    scheduled.insert((input.starts_at, sequence), input);
                    sequence = sequence.wrapping_add(1);
                }
            }
        }
        let concurrency = checker_concurrency();
        let mut probes = futures::stream::FuturesUnordered::new();
        let mut receipts_closed = false;
        loop {
            while probes.len() < concurrency {
                let Some((&(starts_at, _), _)) = scheduled.first_key_value() else {
                    break;
                };
                if starts_at > tokio::time::Instant::now() {
                    break;
                }
                let (_, input) = scheduled.pop_first().expect("first scheduled probe exists");
                probes.push(run_scheduled_probe(input, timing));
            }
            if receipts_closed && scheduled.is_empty() && probes.is_empty() {
                break;
            }
            let timer_at = (probes.len() < concurrency)
                .then(|| scheduled.first_key_value().map(|(key, _)| key.0))
                .flatten();
            tokio::select! {
                receipt = delivery_receipts.recv(), if !receipts_closed => {
                    match receipt {
                        Some(receipt) if seen.insert(receipt.team_service_id) => {
                            if let Some(input) = make_input(receipt) {
                                scheduled.insert((input.starts_at, sequence), input);
                                sequence = sequence.wrapping_add(1);
                            }
                        }
                        Some(_) => {}
                        None => receipts_closed = true,
                    }
                }
                result = probes.next(), if !probes.is_empty() => {
                    if let Some(result) = result {
                        result_tx.send(result).map_err(|_| {
                            AppError::internal(
                                "A&D checker result writer stopped before probe completion",
                            )
                        })?;
                    }
                }
                _ = tokio::time::sleep_until(
                    timer_at.unwrap_or_else(|| tokio::time::Instant::now()
                        + std::time::Duration::from_secs(86_400)),
                ), if timer_at.is_some() => {}
            }
        }
        Ok::<(), AppError>(())
    };
    let writer = super::super::persistence::record_check_result_batches(
        db, game_id, round_id, lease, result_rx,
    );
    tokio::try_join!(producer, writer)?;
    Ok(())
}

#[cfg(test)]
pub(super) async fn classify_no_window_for_test(status: AdCheckStatus) -> AdProbeResult {
    run_scheduled_probe(
        AdProbeInput {
            service_id: 1,
            host: "unreachable.invalid".into(),
            port: 1,
            participation_id: 2,
            challenge_id: 3,
            container_id: None,
            checker_dir: None,
            flag: Some("flag{late}".into()),
            schedule: None,
            starts_at: tokio::time::Instant::now(),
            no_window_status: status,
            timeout_is_platform_limited: false,
        },
        AdCheckerTiming {
            round_number: 4,
            tick_seconds: 30,
            grace_seconds: 18,
            window_fraction: 0.5,
            current_probe_cap: Some(std::time::Duration::from_secs(1)),
            nominal_probe_budget: Some(std::time::Duration::from_secs(1)),
            effective_deadline: tokio::time::Instant::now(),
        },
    )
    .await
}
