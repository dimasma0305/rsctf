//! Compact live participant projection for the challenge route.

use super::*;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameParticipantDeltaModel {
    pub rank: Option<ScoreboardItem>,
}

/// `GET /api/game/{id}/details/participant` — the caller's compact live rank.
///
/// The compatibility `/details` endpoint remains the one-shot catalog and
/// credential bootstrap. This endpoint intentionally omits that catalog and
/// team token, and reads one row from a per-scoreboard-generation index.
pub async fn game_participant_delta(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let ctx = context_info(&st, &user, id, false).await?;
    let mut rank = build_participant_scoreboard_item(
        &st,
        &ctx.game,
        user.is_monitor(),
        ctx.participation.team_id,
    )
    .await?;

    let mut visible_challenge_ids = Vec::new();
    if let Some(rank) = &mut rank {
        let solved_ids: Vec<i32> = rank
            .solved_challenges
            .iter()
            .map(|challenge| challenge.id)
            .collect();
        let permissions = effective_permissions_batch(&st, &ctx.participation, &solved_ids).await?;
        let visible: HashSet<i32> = solved_ids
            .into_iter()
            .filter(|challenge_id| {
                permissions
                    .get(challenge_id)
                    .is_none_or(|permission| permission.contains(GamePermission::VIEW_CHALLENGE))
            })
            .collect();
        retain_visible_solves(rank, &visible);
        visible_challenge_ids.extend(visible);
        visible_challenge_ids.sort_unstable();
    }

    let model = GameParticipantDeltaModel { rank };
    let raw = bytes::Bytes::from(
        serde_json::to_vec(&model).map_err(|error| AppError::internal(error.to_string()))?,
    );
    let bundle = scoreboard_encoding::build_versioned_bundle(
        raw,
        format!(
            "participant:{}:{}:{}",
            id,
            ctx.participation.id,
            user.is_monitor()
        ),
    )
    .await?
    .bytes;

    final_policy::finish_participant_response(
        st.pg(),
        &user,
        id,
        ctx.participation.team_id,
        ctx.participation.id,
        visible_challenge_ids,
        bundle,
        &headers,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_projection_uses_the_camel_case_rank_contract() {
        let wire = serde_json::to_value(GameParticipantDeltaModel { rank: None }).unwrap();
        assert!(wire.get("rank").is_some());
        assert_eq!(wire.as_object().map(serde_json::Map::len), Some(1));
    }
}
