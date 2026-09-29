//! Admin routes for the platform-wide detection registries: accepted AI chat
//! providers and agent-artifact signatures, plus the per-event rescan.

use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post, put};
use axum::Router;

use super::{agent_signatures, ai_chat_providers};
use crate::app_state::SharedState;
use crate::middlewares::rate_limiter::{limited, Policy};

pub(super) fn router() -> Router<SharedState> {
    Router::new()
        .route(
            "/api/admin/ai-chat-providers",
            limited(Policy::Query, get(ai_chat_providers::list_providers)),
        )
        .route(
            "/api/admin/ai-chat-providers/{key}",
            limited(
                Policy::Query,
                put(ai_chat_providers::save_provider).delete(ai_chat_providers::delete_provider),
            )
            .layer(DefaultBodyLimit::max(4096)),
        )
        .route(
            "/api/admin/agent-signatures",
            limited(Policy::Query, get(agent_signatures::list_signatures)),
        )
        .route(
            "/api/admin/agent-signatures/{key}",
            limited(
                Policy::Query,
                put(agent_signatures::save_signature).delete(agent_signatures::delete_signature),
            )
            .layer(DefaultBodyLimit::max(4096)),
        )
        .route(
            "/api/admin/games/{id}/agent-artifacts/rescan",
            limited(
                Policy::Concurrency,
                post(agent_signatures::rescan_game_artifacts),
            ),
        )
}
