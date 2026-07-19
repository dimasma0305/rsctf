use std::collections::HashMap;

use serde_json::json;

use crate::app_state::SharedState;
use crate::utils::enums::ParticipationStatus;
use crate::utils::error::{AppError, AppResult};

use super::{load_hill_spec, record_receipt, CrownPhase, CycleRow, OfficialConfig};

pub(super) struct CapabilityWindow<'a> {
    pub(super) target_id: i32,
    pub(super) game_id: i32,
    pub(super) challenge_id: i32,
    pub(super) cycle_id: i64,
    pub(super) reset_attempt: i32,
    pub(super) round_number: i32,
    pub(super) ad_round_id: i32,
    pub(super) roster: &'a [i32],
    pub(super) tokens: &'a [String],
}

pub(super) async fn rotate_capability_window(
    connection: &mut sqlx::PgConnection,
    window: CapabilityWindow<'_>,
) -> AppResult<Option<Vec<i32>>> {
    if window.roster.len() != window.tokens.len() {
        return Err(AppError::internal(
            "KotH capability roster/token cardinality mismatch",
        ));
    }

    // A delayed recovery task must not revoke or replace the window published
    // by a newer attempt. Lock the exact cycle/target identity before the first
    // side effect; every lifecycle writer updates these same rows.
    let fenced: Option<i64> = sqlx::query_scalar(
        r#"SELECT cycle.id
             FROM "KothCrownCycles" cycle
             JOIN "KothTargets" target
               ON target.game_id = cycle.game_id
              AND target.challenge_id = cycle.challenge_id
            WHERE cycle.id = $1 AND cycle.game_id = $2
              AND cycle.challenge_id = $3 AND cycle.reset_attempt = $4
              AND cycle.phase = 'CapabilityPending'
              AND target.id = $5
              AND target.container_id = cycle.replacement_container_id
            FOR UPDATE OF cycle, target"#,
    )
    .bind(window.cycle_id)
    .bind(window.game_id)
    .bind(window.challenge_id)
    .bind(window.reset_attempt)
    .bind(window.target_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if fenced.is_none() {
        return Ok(None);
    }

    sqlx::query(
        r#"UPDATE "KothTokens" token
              SET revoked_at = COALESCE(revoked_at, clock_timestamp())
            WHERE challenge_id = $1 AND revoked_at IS NULL
              AND (cycle_id <> $2 OR reset_attempt <> $3)"#,
    )
    .bind(window.challenge_id)
    .bind(window.cycle_id)
    .bind(window.reset_attempt)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if window.roster.is_empty() {
        return Ok(Some(Vec::new()));
    }

    let participation_teams: Vec<(i32, i32)> = sqlx::query_as(
        r#"SELECT participation.id, participation.team_id
             FROM "Participations" participation
            WHERE participation.id = ANY($1)
              AND participation.game_id = $2
              AND participation.status = $3"#,
    )
    .bind(window.roster)
    .bind(window.game_id)
    .bind(ParticipationStatus::Accepted as i16)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let team_ids: Vec<i32> = participation_teams
        .iter()
        .map(|(_, team_id)| *team_id)
        .collect();
    let eligible_teams = crate::services::ad::roster::eligible_shared_credential_teams_on(
        &mut *connection,
        &team_ids,
    )
    .await?;
    let team_by_participation: HashMap<i32, i32> = participation_teams.into_iter().collect();
    let mut eligible_roster = Vec::new();
    let mut eligible_tokens = Vec::new();
    for (participation_id, token) in window.roster.iter().zip(window.tokens) {
        if team_by_participation
            .get(participation_id)
            .is_some_and(|team_id| eligible_teams.contains(team_id))
        {
            eligible_roster.push(*participation_id);
            eligible_tokens.push(token.clone());
        }
    }
    // An idempotent retry can encounter rows written by an older binary before
    // the eligibility fence existed. Revoke any such row in this same window.
    sqlx::query(
        r#"UPDATE "KothTokens"
              SET revoked_at = COALESCE(revoked_at, clock_timestamp())
            WHERE cycle_id = $1 AND challenge_id = $2
              AND reset_attempt = $3 AND target_id = $4
              AND NOT (participation_id = ANY($5))
              AND revoked_at IS NULL"#,
    )
    .bind(window.cycle_id)
    .bind(window.challenge_id)
    .bind(window.reset_attempt)
    .bind(window.target_id)
    .bind(&eligible_roster)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !eligible_roster.is_empty() {
        sqlx::query(
            r#"INSERT INTO "KothTokens"
             (target_id, participation_id, token, submitted_at,
              round_number, ad_round_id, revoked_at, cycle_id, challenge_id,
              reset_attempt)
           SELECT $1, minted.participation_id, minted.token,
                  clock_timestamp(), $4, $5, NULL, $6, $7, $8
             FROM UNNEST($2::integer[], $3::text[])
                  AS minted(participation_id, token)
             JOIN "Participations" participation
               ON participation.id = minted.participation_id
              AND participation.game_id = $9
              AND participation.status = $10
           ON CONFLICT (cycle_id, challenge_id, reset_attempt, participation_id)
             DO NOTHING"#,
        )
        .bind(window.target_id)
        .bind(&eligible_roster)
        .bind(&eligible_tokens)
        .bind(window.round_number)
        .bind(window.ad_round_id)
        .bind(window.cycle_id)
        .bind(window.challenge_id)
        .bind(window.reset_attempt)
        .bind(window.game_id)
        .bind(ParticipationStatus::Accepted as i16)
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    let active: Vec<i32> = sqlx::query_scalar(
        r#"SELECT participation_id
             FROM "KothTokens"
            WHERE cycle_id = $1 AND challenge_id = $2
              AND reset_attempt = $3 AND target_id = $4
              AND participation_id = ANY($5) AND revoked_at IS NULL
            ORDER BY participation_id"#,
    )
    .bind(window.cycle_id)
    .bind(window.challenge_id)
    .bind(window.reset_attempt)
    .bind(window.target_id)
    .bind(&eligible_roster)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(Some(active))
}

pub(super) async fn mint_capabilities(
    st: &SharedState,
    config: &OfficialConfig,
    cycle: &CycleRow,
    ad_round_id: i32,
    round_number: i32,
) -> AppResult<()> {
    let spec = load_hill_spec(st, cycle).await?;
    let tokens: Vec<String> = config
        .roster
        .iter()
        .map(|_| format!("koth_{}", crate::utils::codec::random_token(18)))
        .collect();
    let mut control =
        super::super::super::koth_auth::acquire_game_lock(&st.db, cycle.game_id).await?;
    let Some(issued_participations) = rotate_capability_window(
        control.transaction_mut(),
        CapabilityWindow {
            target_id: spec.target_id,
            game_id: cycle.game_id,
            challenge_id: cycle.challenge_id,
            cycle_id: cycle.id,
            reset_attempt: cycle.reset_attempt,
            round_number,
            ad_round_id,
            roster: &config.roster,
            tokens: &tokens,
        },
    )
    .await?
    else {
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(());
    };
    sqlx::query(r#"DELETE FROM "KothClaimStates" WHERE target_id = $1"#)
        .bind(spec.target_id)
        .execute(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let advanced = sqlx::query(
        r#"UPDATE "KothCrownCycles"
              SET provisional_participation_id = NULL,
                  confirmed_participation_id = NULL,
                  confirmation_progress = 0,
                  phase = 'ReadinessPending', updated_at = clock_timestamp()
            WHERE id = $1 AND phase = 'CapabilityPending'"#,
    )
    .bind(cycle.id)
    .execute(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    if advanced != 1 {
        return Err(AppError::internal(
            "KotH capability phase changed after its window was fenced",
        ));
    }
    record_receipt(
        control.transaction_mut(),
        cycle,
        CrownPhase::CapabilityPending,
        json!({
            "issuedCapabilities": issued_participations.len(),
            "round": round_number,
            "resetAttempt": cycle.reset_attempt,
        }),
        None,
    )
    .await?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    st.cache
        .remove(&format!("latestround:{}", cycle.game_id))
        .await;
    for participation_id in &config.roster {
        st.cache
            .remove(&format!(
                "kothtoken:{}:{}:{}:{}",
                cycle.game_id, cycle.challenge_id, participation_id, round_number
            ))
            .await;
        st.cache
            .remove(&format!(
                "kothtokensall:{}:{}:{}",
                cycle.game_id, participation_id, round_number
            ))
            .await;
    }
    Ok(())
}
