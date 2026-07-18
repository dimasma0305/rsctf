use crate::app_state::SharedState;
use crate::models::data::game;
use crate::utils::error::AppResult;
use futures::StreamExt;

/// Flag delivery is I/O-bound, but an unconstrained fan-out can exhaust the
/// connection pool and yamux stream budget during a large round transition.
fn flag_push_concurrency() -> usize {
    std::env::var("RSCTF_AD_FLAG_PUSH_CONCURRENCY")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (1..=256).contains(value))
        .unwrap_or(64)
}

fn flag_push_attempts() -> usize {
    std::env::var("RSCTF_AD_FLAG_PUSH_ATTEMPTS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (1..=5).contains(value))
        .unwrap_or(3)
}

fn flag_push_timeout() -> std::time::Duration {
    let seconds = std::env::var("RSCTF_AD_FLAG_PUSH_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (1..=10).contains(value))
        .unwrap_or(2);
    std::time::Duration::from_secs(seconds)
}

async fn deliver_external_flag(
    state: &SharedState,
    planted: &crate::services::ad_engine::AdvancedRoundFlag,
) -> (bool, usize) {
    let attempts = flag_push_attempts();
    for attempt in 1..=attempts {
        let delivered = tokio::time::timeout(
            flag_push_timeout(),
            crate::services::byoc_tunnel::push_flag(
                state,
                planted.participation_id,
                planted.challenge_id,
                &planted.flag,
            ),
        )
        .await
        .unwrap_or(false);
        if delivered {
            return (true, attempt);
        }
        if attempt < attempts {
            tokio::time::sleep(std::time::Duration::from_millis(50 * attempt as u64)).await;
        }
    }
    (false, attempts)
}

async fn deliver_managed_flag(
    state: &SharedState,
    planted: &crate::services::ad_engine::AdvancedRoundFlag,
    container_id: &str,
) -> (bool, usize) {
    let attempts = flag_push_attempts();
    for attempt in 1..=attempts {
        let command = managed_flag_command(&planted.flag);
        let delivered = matches!(
            tokio::time::timeout(flag_push_timeout(), state.containers.exec(container_id, command))
                .await,
            Ok(Ok(output)) if output.trim_end_matches(['\r', '\n']) == planted.flag
        );
        if delivered {
            return (true, attempt);
        }
        if attempt < attempts {
            tokio::time::sleep(std::time::Duration::from_millis(50 * attempt as u64)).await;
        }
    }
    (false, attempts)
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
    planted: crate::services::ad_engine::AdvancedRoundFlag,
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
        (true, Some(container_id)) => deliver_managed_flag(state, &planted, container_id).await,
        (true, None) => {
            return crate::services::ad_engine::FlagDeliveryOutcome::failed(
                planted.team_service_id,
                kind,
                None,
                0,
                "managed service has no active container identity",
            );
        }
        (false, _) => deliver_external_flag(state, &planted).await,
    };
    if attempted.0 {
        crate::services::ad_engine::FlagDeliveryOutcome::succeeded(
            planted.team_service_id,
            kind,
            planted.managed.then(|| container_id.unwrap().to_string()),
            attempted.1,
        )
    } else {
        crate::services::ad_engine::FlagDeliveryOutcome::failed(
            planted.team_service_id,
            kind,
            planted.managed.then(|| container_id.unwrap().to_string()),
            attempted.1,
            if planted.managed {
                "managed container rejected the round flag"
            } else {
                "BYOC tunnel did not acknowledge the round flag"
            },
        )
    }
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
    let finished =
        tokio::time::timeout(budget, finish_prepared_round(state, game, prepared, &lease)).await;
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
) -> AppResult<()> {
    let round_id = prepared.id;
    let next_number = prepared.number;
    let flags_planted = prepared.flags.len() as i32;

    // Repair A&D containers before planting. A replacement receives a new exact
    // runtime identity, so reload the committed round snapshot afterward and
    // never exec a stale container ID. KotH is deliberately excluded here and
    // remains untouched until its dead-container checker evidence is committed.
    let repair_failures =
        match crate::controllers::edit::ensure_ad_containers(state, game, None, false, false).await
        {
            Ok((_, failures)) => failures as usize,
            Err(error) => {
                tracing::warn!(
                    game = game.id,
                    round = next_number,
                    %error,
                    "cron: managed A&D service repair failed before flag propagation"
                );
                1
            }
        };
    if repair_failures > 0 {
        tracing::warn!(
            game = game.id,
            round = next_number,
            failed = repair_failures,
            "cron: managed A&D service repair was incomplete before flag propagation"
        );
    }
    let publication =
        crate::services::ad_engine::prepared_round_snapshot(&state.db, game.id, round_id).await?;

    let mut pushes = futures::stream::iter(publication.flags)
        .map(|planted| deliver_round_flag(state, planted))
        .buffer_unordered(flag_push_concurrency());
    let mut outcomes = Vec::with_capacity(flags_planted as usize);
    while let Some(outcome) = pushes.next().await {
        outcomes.push(outcome);
    }
    let delivery = crate::services::ad_engine::record_flag_delivery_outcomes(
        &state.db, game.id, round_id, &outcomes,
    )
    .await?;
    if delivery.failure_count > 0 {
        tracing::warn!(
            game = game.id,
            round = next_number,
            failed = delivery.failure_count,
            "cron: some A&D services did not accept the new flag"
        );
    }

    // Crown-cycle hills cross their durable reset state machine before the
    // round checker starts. A reset/readiness failure leaves the cycle outside
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

    if let Err(error) = crate::services::ad_engine::run_checker(
        &state.db,
        state.containers.as_ref(),
        game.id,
        round_id,
        lease,
    )
    .await
    {
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
mod tests {
    use super::{
        flag_push_attempts, flag_push_concurrency, flag_push_timeout, managed_flag_command,
    };

    #[test]
    fn flag_push_concurrency_is_bounded() {
        let concurrency = flag_push_concurrency();
        assert!((1..=256).contains(&concurrency));
        assert!((1..=5).contains(&flag_push_attempts()));
        assert!((1..=10).contains(&flag_push_timeout().as_secs()));
    }

    #[test]
    fn managed_flag_is_an_argument_not_shell_source() {
        let flag = "flag{$(touch /tmp/owned);'\\\"}";
        let command = managed_flag_command(flag);
        assert_eq!(command[4], flag);
        assert!(!command[2].contains(flag));
        assert!(command[2].contains("$1"));
    }
}
