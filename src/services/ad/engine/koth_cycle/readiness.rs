//! Choose the official hill set once, while the game configuration fence is held.
use sqlx::{Postgres, Transaction};

use crate::utils::enums::{ChallengeBuildStatus, ChallengeReviewStatus, ChallengeType};
use crate::utils::error::{AppError, AppResult};

#[derive(Default)]
pub(crate) struct ReadyHills {
    pub ids: Vec<i32>,
    pub has_marker: bool,
    pub excluded: Vec<serde_json::Value>,
}

pub(crate) async fn ready_hills(
    tx: &mut Transaction<'static, Postgres>,
    game_id: i32,
    checker_ids: &[i32],
) -> AppResult<ReadyHills> {
    let rows = sqlx::query_as::<_, (i32, bool, bool, bool)>(
        r#"SELECT challenge.id,
                  challenge.build_status = $4
                    AND NULLIF(BTRIM(challenge.build_image_digest), '') IS NOT NULL,
                  EXISTS (SELECT 1 FROM "KothTargets" target
                           WHERE target.game_id = challenge.game_id
                             AND target.challenge_id = challenge.id
                             AND NULLIF(BTRIM(target.host), '') IS NOT NULL
                             AND target.port BETWEEN 1 AND 65535
                             AND NULLIF(BTRIM(target.container_id), '') IS NOT NULL
                           FOR SHARE OF target),
                  EXISTS (SELECT 1 FROM "KothApiObservers" observer
                           WHERE observer.game_id = challenge.game_id
                             AND observer.challenge_id = challenge.id)
             FROM "GameChallenges" challenge
            WHERE challenge.game_id = $1 AND challenge.is_enabled = TRUE
              AND challenge.review_status = $2 AND challenge."Type" = $3
            ORDER BY challenge.id FOR SHARE OF challenge"#,
    )
    .bind(game_id)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeType::KingOfTheHill as i16)
    .bind(ChallengeBuildStatus::Success as i16)
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let mut ready = ReadyHills::default();
    for (id, image, target, api) in rows {
        let mut reasons = Vec::new();
        if !image {
            reasons.push("ImageNotReady");
        }
        if !target {
            reasons.push("TargetNotReady");
        }
        if !checker_ids.contains(&id) {
            reasons.push("CheckerNotReady");
        }
        if reasons.is_empty() {
            ready.ids.push(id);
            ready.has_marker |= !api;
        } else {
            ready
                .excluded
                .push(serde_json::json!({"challengeId": id, "reasons": reasons}));
        }
    }
    Ok(ready)
}
