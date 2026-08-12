use chrono::Utc;

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

use super::{create_or_load_cycle, drive_one_cycle, OfficialConfig};

pub(super) async fn drive_hill_transition(
    st: &SharedState,
    config: &OfficialConfig,
    game_id: i32,
    challenge_id: i32,
    ad_round_id: i32,
    round_number: i32,
    epoch: i32,
) -> AppResult<()> {
    let key = format!("shared-container:{challenge_id}");
    let _local = crate::utils::single_flight::coalesce(&key).await;
    let lock =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &key).await?;
    let latest = sqlx::query_as::<_, (i64, i32, String, i32)>(
        r#"SELECT id, cycle_number, phase, planned_end_round
             FROM "KothCrownCycles"
            WHERE game_id = $1 AND challenge_id = $2
            ORDER BY cycle_number DESC LIMIT 1"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let persistent_end_round = config.persistent_end_round().max(round_number);
    let event_ended = Utc::now() >= config.end_time_utc;
    let cycle_id = match latest {
        Some((cycle_id, _, ref phase, planned_end_round))
            if !matches!(phase.as_str(), "Completed" | "Ended") =>
        {
            if planned_end_round < persistent_end_round {
                sqlx::query(
                    r#"UPDATE "KothCrownCycles"
                          SET planned_end_round = $2,
                              updated_at = clock_timestamp()
                        WHERE id = $1 AND planned_end_round < $2"#,
                )
                .bind(cycle_id)
                .bind(persistent_end_round)
                .execute(st.pg())
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            }
            // The round checker changes Active to DestroyPending after a
            // stopped runtime or a confirmed health-failure streak. Until
            // then a persistent arena has no per-round lifecycle transition.
            if phase == "Active" && !event_ended {
                lock.release().await?;
                return Ok(());
            }
            cycle_id
        }
        Some((cycle_id, _, _, _)) if event_ended => cycle_id,
        None if event_ended => {
            lock.release().await?;
            return Ok(());
        }
        terminal => {
            let cycle_number = terminal.map_or(1, |(_, number, _, _)| number.saturating_add(1));
            create_or_load_cycle(
                st,
                game_id,
                challenge_id,
                cycle_number,
                epoch,
                round_number,
                persistent_end_round,
            )
            .await?
        }
    };
    // This is a no-op for Active and recovery phases. For a newly declared
    // persistent generation it captures the exact pre-scoring target identity
    // before the normal bootstrap sequence replaces and validates it.
    super::super::rollover::refresh_old_container(st, cycle_id).await?;
    let result = drive_one_cycle(st, config, cycle_id, ad_round_id, round_number).await;
    if let Err(error) = &result {
        sqlx::query(
            r#"UPDATE "KothCrownCycles"
                  SET last_error = $2, updated_at = clock_timestamp()
                WHERE id = $1 AND phase NOT IN ('Active','Completed','Ended')"#,
        )
        .bind(cycle_id)
        .bind(error.to_string())
        .execute(st.pg())
        .await
        .map_err(|db_error| AppError::internal(db_error.to_string()))?;
    }
    lock.release().await?;
    result
}
