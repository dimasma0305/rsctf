//! Admin Flag-Egress feed — `GET /api/admin/Games/{id}/FlagEgress`.
//!
//! Lists the bounded, searchable flag-egress aggregates for a game and exposes
//! a commit/update-ordered reconnect cursor. The proxy publishes the exact same
//! camelCase DTO after a successful commit.

use super::*;

use crate::services::flag_egress_feed::{self, FlagEgressBackfill, FlagEgressEventModel};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlagEgressQuery {
    #[serde(default = "default_count")]
    pub count: u64,
    #[serde(default)]
    pub skip: u64,
    #[serde(default)]
    pub search: Option<String>,
}

fn default_count() -> u64 {
    100
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlagEgressBackfillQuery {
    pub after: Option<i64>,
    #[serde(default = "default_backfill_limit")]
    pub limit: i64,
}

fn default_backfill_limit() -> i64 {
    flag_egress_feed::MAX_FLAG_EGRESS_BACKFILL
}

/// `GET /api/admin/Games/{id}/FlagEgress?count=&skip=&search=` — newest first.
pub async fn get_flag_egress(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(game_id): Path<i32>,
    Query(q): Query<FlagEgressQuery>,
) -> AppResult<ArrayResponse<FlagEgressEventModel>> {
    let page = flag_egress_feed::page(st.pg(), game_id, q.search.as_deref(), q.skip, q.count)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(ArrayResponse::new(page.events, page.total))
}

/// `GET /api/admin/Games/{id}/FlagEgress/backfill` — bounded reconnect recovery.
/// Omitting `after` returns a cursor-only checkpoint after the listener is live.
pub async fn get_flag_egress_backfill(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(game_id): Path<i32>,
    Query(q): Query<FlagEgressBackfillQuery>,
) -> AppResult<Json<FlagEgressBackfill>> {
    let data = match q.after {
        Some(after) if after < 0 => {
            return Err(AppError::bad_request(
                "Flag Egress cursor must not be negative",
            ));
        }
        Some(after) => flag_egress_feed::backfill_after(st.pg(), game_id, after, q.limit)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?,
        None => FlagEgressBackfill {
            events: Vec::new(),
            next_cursor: flag_egress_feed::latest_cursor(st.pg(), game_id)
                .await
                .map_err(|error| AppError::internal(error.to_string()))?,
            has_more: false,
        },
    };
    Ok(Json(data))
}
