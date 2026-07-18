//! Player automation view for all live hills.

use super::*;

/// One hill in the `Koth/Hills` list (field-for-field with the player toolkit's
/// automation example).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KothHillListItem {
    pub challenge_id: i32,
    pub title: String,
    /// Round of the hill's latest recorded control result (0 if none yet).
    pub round: i32,
    pub holder_participation_id: Option<i32>,
    pub holder_team_name: Option<String>,
    pub provisional_claimant_participation_id: Option<i32>,
    pub provisional_claimant_team_name: Option<String>,
    pub provisional_confirmation_ticks: i32,
    pub claim_confirmation_ticks: i32,
    pub is_you: bool,
    pub status: Option<String>,
    pub ip: Option<String>,
    pub port: Option<i32>,
    pub cycle_number: i32,
    pub cycle_tick: i32,
    pub cycle_ticks: i32,
    pub reset_phase: String,
    pub is_scorable: bool,
    pub eligible_now: bool,
    pub is_you_cooldown: bool,
    pub cooldown_participants: Vec<KothCooldownParticipant>,
    pub next_reset_ticks: Option<i32>,
}

/// `GET /api/Game/{id}/Ad/Koth/Hills` — every enabled hill in one call: exact
/// target, confirmed/provisional control, lifecycle, and cooldown state.
pub async fn koth_hills(
    State(st): State<SharedState>,
    maybe_user: MaybeUser,
    Path(id): Path<i32>,
    headers: HeaderMap,
    verified: Option<axum::Extension<crate::services::ad::api_token::VerifiedTeamToken>>,
    rejected: Option<axum::Extension<crate::services::ad::api_token::RejectedTeamToken>>,
) -> AppResult<RequestResponse<Vec<KothHillListItem>>> {
    let part = crate::controllers::game::ad::resolve_ad_attacker(
        &st,
        &headers,
        verified.as_ref().map(|extension| &extension.0),
        rejected.as_ref().map(|extension| &extension.0),
        maybe_user,
        id,
    )
    .await?;
    let board = compute_koth_hill_state(&st, id).await?;
    let mut lifecycle = load_lifecycle_map(&st, id, board.latest_round, None).await?;

    let mut list: Vec<KothHillListItem> = board
        .hills
        .iter()
        .filter(|hill| hill.is_enabled)
        .map(|hill| {
            let view = lifecycle.remove(&hill.challenge_id).unwrap_or_default();
            let is_you_cooldown = view
                .cooldown_participants
                .iter()
                .any(|cooldown| cooldown.participation_id == part.id);
            let holder = board.holder_by_challenge.get(&hill.challenge_id).copied();
            let (status, round) = match board.latest_control_by_challenge.get(&hill.challenge_id) {
                Some((status, round)) => (Some(status.clone()), *round),
                None => (None, 0),
            };
            KothHillListItem {
                challenge_id: hill.challenge_id,
                title: hill.title.clone(),
                round,
                holder_participation_id: holder,
                holder_team_name: board
                    .holder_team_name_by_challenge
                    .get(&hill.challenge_id)
                    .cloned(),
                provisional_claimant_participation_id: view.provisional_participation_id,
                provisional_claimant_team_name: view.provisional_team_name,
                provisional_confirmation_ticks: view.confirmation_progress,
                claim_confirmation_ticks: view.claim_confirmation_ticks,
                is_you: holder == Some(part.id),
                status,
                ip: hill.container_ip.clone(),
                port: hill.container_port,
                cycle_number: view.cycle_number,
                cycle_tick: view.cycle_tick,
                cycle_ticks: view.cycle_ticks,
                reset_phase: view.reset_phase,
                is_scorable: view.is_scorable,
                eligible_now: view.is_scorable && !is_you_cooldown,
                is_you_cooldown,
                cooldown_participants: view.cooldown_participants,
                next_reset_ticks: view.next_reset_ticks,
            }
        })
        .collect();
    list.sort_by_key(|hill| hill.challenge_id);

    Ok(RequestResponse::ok(list))
}
