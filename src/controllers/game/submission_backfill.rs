//! Reconnect-safe monitor submission recovery.

use super::*;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmissionBackfillQuery {
    #[serde(default)]
    pub after: Option<i64>,
    #[serde(default = "submission_backfill_default_limit")]
    pub limit: i64,
}

fn submission_backfill_default_limit() -> i64 {
    crate::services::submission_feed::MAX_BACKFILL_SUBMISSIONS
}

/// `GET /api/game/{id}/submissions/backfill` — monitor-only reconnect recovery.
/// Omitting `after` returns a cursor checkpoint without any history rows.
pub async fn submission_backfill(
    State(st): State<SharedState>,
    MonitorUser(_user): MonitorUser,
    Path(id): Path<i32>,
    Query(q): Query<SubmissionBackfillQuery>,
) -> AppResult<RequestResponse<crate::services::submission_feed::SubmissionBackfill>> {
    let start: Option<DateTime<Utc>> = sqlx::query_scalar(
        r#"SELECT start_time_utc FROM "Games" WHERE id = $1 AND deletion_pending = FALSE"#,
    )
    .bind(id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let start = start.ok_or_else(|| AppError::not_found("Game not found"))?;
    if Utc::now() < start {
        return Err(AppError::game_not_started());
    }

    let data = match q.after {
        Some(after) if after < 0 => {
            return Err(AppError::bad_request(
                "Submission cursor must not be negative",
            ));
        }
        Some(after) => {
            crate::services::submission_feed::backfill_after(st.pg(), id, after, q.limit)
                .await
                .map_err(|error| AppError::internal(error.to_string()))?
        }
        None => crate::services::submission_feed::SubmissionBackfill {
            submissions: Vec::new(),
            next_cursor: crate::services::submission_feed::latest_cursor(st.pg(), id)
                .await
                .map_err(|error| AppError::internal(error.to_string()))?,
            has_more: false,
        },
    };
    Ok(RequestResponse::ok(data))
}
