//! Organizer HTTP boundary for durable bulk challenge mutations.

use super::*;
use crate::services::bulk_challenges::{BulkChallengeMutationRequest, BulkChallengeMutationResult};

pub async fn mutate_challenges_bulk(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
    Json(request): Json<BulkChallengeMutationRequest>,
) -> AppResult<RequestResponse<BulkChallengeMutationResult>> {
    crate::services::bulk_challenges::mutate_challenges_bulk(st, user, game_id, request)
        .await
        .map(RequestResponse::ok)
}

#[cfg(test)]
#[path = "bulk_authorization_tests.rs"]
mod tests;
