//! Shared container lifecycle primitives for player requests and domain workers.
//! Authorization and publication rechecks stay inside these operations.
use crate::app_state::SharedState;
use crate::models::data::{container, game_challenge};
use crate::services::container::ContainerSpec;
use crate::services::live_roster::LiveParticipationIdentity;
use crate::utils::enums::{ChallengeReviewStatus, ChallengeType, ContainerStatus, NetworkMode};
use crate::utils::error::{AppError, AppResult};
use chrono::{DateTime, Utc};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub(crate) mod eligibility;
pub(crate) mod image_repair;
pub(crate) mod operations;
pub(crate) mod publication;
pub(crate) mod reaping;
pub(crate) mod shared;
pub(crate) mod workload_fence;
use eligibility::{
    load_eligible_shared_challenge, player_container_request_is_eligible, ContainerRequestMode,
};
use publication::{revoke_published_shared_container, revoke_published_team_container};
use workload_fence::{ensure_publication_definition_current, load_shared_definition_snapshot};

/// RSCTF `ContainerInfoModel`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerInfoModel {
    pub id: String,
    pub status: ContainerStatus,
    #[serde(with = "crate::utils::datetime::millis")]
    pub started_at: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub expect_stop_at: DateTime<Utc>,
    pub entry: String,
}

impl From<&container::Model> for ContainerInfoModel {
    fn from(c: &container::Model) -> Self {
        Self {
            id: c.id.to_string(),
            status: c.status,
            started_at: c.started_at,
            expect_stop_at: c.expect_stop_at,
            entry: c.entry(),
        }
    }
}

pub(crate) fn uses_shared_container(c: &game_challenge::Model) -> bool {
    c.challenge_type == ChallengeType::StaticContainer
        && c.enable_shared_container
        && crate::services::challenge_workloads::has_runtime(c)
}

pub(crate) async fn load_playable_challenge(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<game_challenge::Model> {
    game_challenge::Entity::find()
        .filter(game_challenge::Column::Id.eq(challenge_id))
        .filter(game_challenge::Column::GameId.eq(game_id))
        .filter(game_challenge::Column::IsEnabled.eq(true))
        .filter(game_challenge::Column::ReviewStatus.eq(ChallengeReviewStatus::Active))
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Challenge not found"))
}
