use serde_json::{json, Value};
use uuid::Uuid;

use super::database_error;
use crate::app_state::SharedState;
use crate::services::challenge_mutations::load_challenge_locked;
use crate::utils::enums::{ChallengeBuildStatus, ChallengeType};
use crate::utils::error::{AppError, AppResult};

const OPERATIONAL_FIELDS: &[&str] = &[
    "container_image",
    "build_image_digest",
    "ad_checker_image",
    "workload_spec",
    "runtime_profile",
    "memory_limit",
    "cpu_count",
    "storage_limit",
    "network_mode",
    "expose_port",
    "ad_allow_egress",
    "ad_self_hosted",
    "is_enabled",
    "enable_traffic_capture",
    "ad_allow_self_reset",
    "ad_ssh_requires_flag",
    "observer_revision",
];

fn changed(before: &Value, after: &Value) -> bool {
    OPERATIONAL_FIELDS
        .iter()
        .any(|field| before.get(field) != after.get(field))
}

fn id(row: &Value) -> AppResult<i32> {
    row.get("id")
        .and_then(Value::as_i64)
        .and_then(|id| i32::try_from(id).ok())
        .ok_or_else(|| AppError::internal("invalid maintenance challenge identity"))
}

pub(crate) async fn execute_resume(
    st: &SharedState,
    claimed: &crate::services::control_jobs::ClaimedControlJob,
) -> AppResult<Value> {
    let window_id: Uuid = serde_json::from_value(
        claimed
            .input
            .get("windowId")
            .cloned()
            .unwrap_or(Value::Null),
    )
    .map_err(|_| AppError::bad_request("windowId is required"))?;
    // Hold across teardown so a reclaimed lease cannot retire replacements
    // provisioned after another executor has completed this same window.
    let lock = crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(
        st.pg(),
        &format!("competition-maintenance:{window_id}"),
    )
    .await?;
    let result = resume(st, claimed.model.game_id, window_id).await;
    lock.release().await?;
    if let Err(error) = &result {
        sqlx::query(
            r#"UPDATE "CompetitionMaintenanceWindows" SET error = $2
             WHERE id = $1 AND state IN ('Editing','Applying')"#,
        )
        .bind(window_id)
        .bind(error.to_string().chars().take(900).collect::<String>())
        .execute(st.pg())
        .await
        .map_err(database_error)?;
    }
    result
}

async fn resume(st: &SharedState, game_id: i32, window_id: Uuid) -> AppResult<Value> {
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    let connection = &mut **control.transaction_mut();
    sqlx::query("SELECT set_config('rsctf.maintenance_writer', 'on', true)")
        .execute(&mut *connection)
        .await
        .map_err(database_error)?;
    let (state, next, before, stored_after): (String, Option<i32>, Option<Value>, Option<Value>) =
        sqlx::query_as(
            r#"SELECT state, effective_from_round, before_snapshot, after_snapshot
             FROM "CompetitionMaintenanceWindows" WHERE id = $1 AND game_id = $2 FOR UPDATE"#,
        )
        .bind(window_id)
        .bind(game_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(database_error)?
        .ok_or_else(|| AppError::not_found("Maintenance window not found"))?;
    if state == "Completed" {
        return Ok(json!({"resumed": true, "effectiveFromRound": next}));
    }
    if !matches!(state.as_str(), "Editing" | "Applying") {
        return Err(AppError::conflict("Maintenance is not ready to apply"));
    }
    let next = next.ok_or_else(|| AppError::internal("Maintenance has no round fence"))?;
    let paused: bool = sqlx::query_scalar(
        r#"SELECT ad_scoring_paused AND
         (SELECT COALESCE(MAX(number), 0) + 1 FROM "AdRounds" WHERE game_id = $1) = $2
         FROM "Games" WHERE id = $1 FOR UPDATE"#,
    )
    .bind(game_id)
    .bind(next)
    .fetch_one(&mut *connection)
    .await
    .map_err(database_error)?;
    if !paused {
        return Err(AppError::conflict("Maintenance round fence changed"));
    }
    let before = before
        .and_then(|value| value.as_array().cloned())
        .ok_or_else(|| AppError::internal("Maintenance baseline is missing"))?;
    let after = match stored_after {
        Some(value) => value
            .as_array()
            .cloned()
            .ok_or_else(|| AppError::internal("Invalid maintenance snapshot"))?,
        None => {
            let pending: bool = sqlx::query_scalar(
                r#"SELECT EXISTS (
                SELECT 1 FROM "ChallengeDefinitionTransitions" transition
                JOIN "GameChallenges" challenge ON challenge.id=transition.challenge_id
                WHERE challenge.game_id=$1)"#,
            )
            .bind(game_id)
            .fetch_one(&mut *connection)
            .await
            .map_err(database_error)?;
            if pending {
                return Err(AppError::conflict(
                    "Finish the pending challenge replacement before applying maintenance",
                ));
            }
            let touched: Vec<i32> = sqlx::query_scalar(
                r#"SELECT touched_challenge_ids
                FROM "CompetitionMaintenanceWindows" WHERE id=$1"#,
            )
            .bind(window_id)
            .fetch_one(&mut *connection)
            .await
            .map_err(database_error)?;
            let rows: Vec<Value> = sqlx::query_scalar(r#"SELECT rsctf_competition_definition(challenge) || jsonb_build_object(
                 'observer_revision', (SELECT revision FROM "KothApiObserverRevisions"
                     WHERE challenge_id = challenge.id))
                 FROM "GameChallenges" challenge WHERE game_id = $1 AND "Type" IN (4,5) ORDER BY id FOR UPDATE"#)
                .bind(game_id).fetch_all(&mut *connection).await.map_err(database_error)?;
            let mut affected = Vec::new();
            for row in rows {
                let challenge_id = id(&row)?;
                let Some(old) = before.iter().find(|old| old.get("id") == row.get("id")) else {
                    continue;
                };
                if !changed(old, &row) && !touched.contains(&challenge_id) {
                    continue;
                }
                let challenge = load_challenge_locked(connection, game_id, challenge_id).await?;
                if !challenge.challenge_type.uses_ad_engine() {
                    continue;
                }
                if challenge.is_enabled && !challenge.ad_self_hosted {
                    if challenge.build_status != ChallengeBuildStatus::Success
                        && challenge.workload_spec.is_none()
                    {
                        return Err(AppError::conflict(format!(
                            "Build challenge {challenge_id} successfully before resuming"
                        )));
                    }
                    crate::services::challenge_workloads::resolve_runtime(st, &challenge)?;
                }
                if let Some(path) = challenge
                    .ad_checker_image
                    .as_deref()
                    .filter(|path| !path.trim().is_empty())
                {
                    if !tokio::fs::metadata(std::path::Path::new(path).join("run.py"))
                        .await
                        .is_ok_and(|metadata| metadata.is_file())
                    {
                        return Err(AppError::conflict(format!(
                            "Prepare the checker for challenge {challenge_id} before resuming"
                        )));
                    }
                }
                affected.push(row);
            }
            affected
        }
    };
    if state == "Editing" {
        sqlx::query(
            r#"UPDATE "CompetitionMaintenanceWindows" SET state = 'Applying',
             after_snapshot = $2, error = NULL WHERE id = $1 AND state = 'Editing'"#,
        )
        .bind(window_id)
        .bind(json!(after))
        .execute(&mut *connection)
        .await
        .map_err(database_error)?;
        let ids = after.iter().map(id).collect::<AppResult<Vec<_>>>()?;
        sqlx::query(
            r#"UPDATE "GameChallenges" SET is_enabled = FALSE WHERE game_id = $1 AND id = ANY($2)"#,
        )
        .bind(game_id)
        .bind(&ids)
        .execute(&mut *connection)
        .await
        .map_err(database_error)?;
    }
    crate::services::ad::koth_capability_cache::release_game_control(
        control,
        st.cache.as_ref(),
        game_id,
        !after.is_empty(),
    )
    .await?;

    for row in &after {
        let challenge_id = id(row)?;
        crate::services::challenge_lifecycle::destroy_challenge_containers_by_id(
            st,
            game_id,
            challenge_id,
            true,
            true,
            true,
        )
        .await?;
        retire_hill_runtime(st, game_id, challenge_id, next).await?;
    }

    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    let connection = &mut **control.transaction_mut();
    // A reclaimed job can finish the same teardown while an older lease holder
    // is still returning. Only the first finisher may publish a new revision.
    let state: String = sqlx::query_scalar(
        r#"SELECT state FROM "CompetitionMaintenanceWindows"
             WHERE id = $1 AND game_id = $2 FOR UPDATE"#,
    )
    .bind(window_id)
    .bind(game_id)
    .fetch_one(&mut *connection)
    .await
    .map_err(database_error)?;
    if state == "Completed" {
        return Ok(json!({"resumed": true, "effectiveFromRound": next}));
    }
    if state != "Applying" {
        return Err(AppError::conflict(
            "Maintenance state changed during application",
        ));
    }
    sqlx::query("SELECT set_config('rsctf.maintenance_writer', 'on', true)")
        .execute(&mut *connection)
        .await
        .map_err(database_error)?;
    for row in &after {
        let challenge_id = id(row)?;
        sqlx::query(
            r#"UPDATE "GameChallenges" SET is_enabled = $3,
             revision = revision + 1, ad_control_revision = ad_control_revision + 1
             WHERE game_id = $1 AND id = $2"#,
        )
        .bind(game_id)
        .bind(challenge_id)
        .bind(
            row.get("is_enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        )
        .execute(&mut *connection)
        .await
        .map_err(database_error)?;
    }
    let affected = after.iter().map(id).collect::<AppResult<Vec<_>>>()?;
    update_hill_history(connection, game_id, next, &affected).await?;
    sqlx::query(
        r#"UPDATE "CompetitionMaintenanceWindows" SET state = 'Completed',
         error = NULL, completed_at = clock_timestamp() WHERE id = $1 AND state = 'Applying'"#,
    )
    .bind(window_id)
    .execute(&mut *connection)
    .await
    .map_err(database_error)?;
    sqlx::query(
        r#"UPDATE "Games" SET ad_scoring_paused = FALSE,
         ad_scoring_paused_at = NULL, ad_control_revision = ad_control_revision + 1 WHERE id = $1"#,
    )
    .bind(game_id)
    .execute(&mut *connection)
    .await
    .map_err(database_error)?;
    crate::services::ad::koth_capability_cache::release_game_control(
        control,
        st.cache.as_ref(),
        game_id,
        !after.is_empty(),
    )
    .await?;
    crate::services::scoreboard_invalidation::invalidate_live_hill_snapshot(st, game_id).await;
    Ok(json!({"resumed": true, "effectiveFromRound": next, "replacedChallenges": after.len()}))
}

async fn retire_hill_runtime(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
    next: i32,
) -> AppResult<()> {
    let key = format!("shared-container:{challenge_id}");
    let lock =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &key).await?;
    // A crash can leave a replacement recorded on the cycle before its target
    // pointer is published. Retire those owned identities as well as the target.
    let backends: Vec<String> = sqlx::query_scalar(r#"SELECT DISTINCT backend FROM "KothCrownCycles" cycle
         CROSS JOIN LATERAL unnest(ARRAY[cycle.old_container_id, cycle.replacement_container_id]) backend
         WHERE cycle.game_id = $1 AND cycle.challenge_id = $2
           AND cycle.phase NOT IN ('Completed','Ended') AND backend IS NOT NULL"#)
        .bind(game_id).bind(challenge_id).fetch_all(st.pg()).await.map_err(database_error)?;
    for backend in backends {
        crate::services::challenge_lifecycle::destroy_after_capture_fence(st, &backend).await?;
    }
    sqlx::query(
        r#"UPDATE "KothCrownCycles" SET phase = 'Completed',
         planned_end_round = LEAST(planned_end_round, $3 - 1),
         actual_end_round = CASE WHEN actual_start_round IS NULL THEN NULL ELSE $3 - 1 END,
         completed_at = COALESCE(completed_at, clock_timestamp()),
         finalized_at = COALESCE(finalized_at, clock_timestamp()), updated_at = clock_timestamp()
         WHERE game_id = $1 AND challenge_id = $2 AND phase NOT IN ('Completed','Ended')"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(next)
    .execute(st.pg())
    .await
    .map_err(database_error)?;
    lock.release().await?;
    Ok(())
}

async fn update_hill_history(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    next: i32,
    affected: &[i32],
) -> AppResult<()> {
    let config: Option<(i32, Value)> = sqlx::query_as(
        r#"SELECT scoring_start_round, hills_snapshot
         FROM "KothOfficialConfigs" WHERE game_id = $1 FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(database_error)?;
    let Some((start, mut hills)) = config else {
        return Ok(());
    };
    for hill in hills
        .as_array_mut()
        .ok_or_else(|| AppError::internal("Invalid official hills"))?
    {
        let challenge_id = hill
            .get("challengeId")
            .and_then(Value::as_i64)
            .and_then(|id| i32::try_from(id).ok())
            .ok_or_else(|| AppError::internal("Invalid official hill"))?;
        if !affected.contains(&challenge_id) {
            continue;
        }
        let (image, enabled, api): (Option<String>, bool, bool) = sqlx::query_as(
            r#"SELECT build_image_digest, is_enabled, EXISTS (
                 SELECT 1 FROM "KothApiObservers" WHERE challenge_id = $2 AND game_id = $1)
                 FROM "GameChallenges" WHERE game_id = $1 AND id = $2 AND "Type" = $3"#,
        )
        .bind(game_id)
        .bind(challenge_id)
        .bind(ChallengeType::KingOfTheHill as i16)
        .fetch_one(&mut *connection)
        .await
        .map_err(database_error)?;
        let source = if api {
            "Api"
        } else {
            hill.get("claimSource")
                .and_then(Value::as_str)
                .unwrap_or("Marker")
        };
        let previous = json!({"effectiveFromRound": start, "image": hill.get("image"),
            "claimSource": hill.get("claimSource").and_then(Value::as_str).unwrap_or("Marker"),
            "enabled": hill.get("enabled").and_then(Value::as_bool).unwrap_or(true)});
        let revision = json!({"effectiveFromRound": next, "image": image, "claimSource": source, "enabled": enabled});
        let mut history = hill
            .get("operationalHistory")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| vec![previous]);
        // Multiple windows can finish before the next round is opened. Only
        // the last definition at that boundary can govern that round; every
        // intermediate edit remains retained in its maintenance audit window.
        history.retain(|revision| {
            revision
                .get("effectiveFromRound")
                .and_then(Value::as_i64)
                .is_some_and(|round| round < i64::from(next))
        });
        history.push(revision.clone());
        hill["operationalHistory"] = json!(history);
        hill["image"] = revision["image"].clone();
        hill["claimSource"] = revision["claimSource"].clone();
        hill["enabled"] = revision["enabled"].clone();
    }
    sqlx::query(r#"UPDATE "KothOfficialConfigs" SET hills_snapshot = $2 WHERE game_id = $1"#)
        .bind(game_id)
        .bind(hills)
        .execute(&mut *connection)
        .await
        .map_err(database_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn presentation_changes_do_not_replace_runtimes() {
        assert!(!changed(&json!({"title":"Old"}), &json!({"title":"New"})));
        assert!(changed(
            &json!({"memory_limit":64}),
            &json!({"memory_limit":128})
        ));
        assert!(changed(
            &json!({"build_image_digest":"old"}),
            &json!({"build_image_digest":"new"})
        ));
    }
}
