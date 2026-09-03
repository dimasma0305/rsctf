//! Bounded, non-secret history for administrator CSV user imports.

use super::*;

const IMPORT_HISTORY_DAYS: i64 = 180;
const MAX_HISTORY_PAGE: u64 = 50;

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ImportHistorySummary {
    pub operation_id: Uuid,
    pub source_name: Option<String>,
    pub requested_by: String,
    pub status: String,
    pub total: i64,
    pub created: i64,
    pub updated: i64,
    pub skipped: i64,
    #[serde(with = "crate::utils::datetime::millis")]
    pub created_at_utc: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub completed_at_utc: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub credential_expires_at_utc: Option<DateTime<Utc>>,
    pub credentials_available: bool,
    pub details_available: bool,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ImportHistoryRow {
    pub row_index: i32,
    pub user_id: Option<Uuid>,
    pub user_exists: bool,
    pub email: String,
    pub real_name: String,
    pub user_name: String,
    pub team_name: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub email_status: String,
    pub email_error: Option<String>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub email_attempted_at_utc: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportHistoryDetail {
    #[serde(flatten)]
    pub summary: ImportHistorySummary,
    pub rows: Vec<ImportHistoryRow>,
}

fn history_summary_sql(filter_operation: bool) -> String {
    let operation_filter = if filter_operation {
        "AND job.operation_id = $1"
    } else {
        ""
    };
    format!(
        r#"SELECT job.operation_id,
                  job.source_name,
                  COALESCE(requester.user_name, requester.email, 'Unknown administrator')
                    AS requested_by,
                  CASE job.status
                    WHEN 0 THEN 'Running'
                    WHEN 1 THEN 'Completed'
                    ELSE 'Expired'
                  END AS status,
                  job.row_count::BIGINT AS total,
                  COUNT(history.row_index) FILTER (WHERE history.outcome = 'created')::BIGINT
                    AS created,
                  COUNT(history.row_index) FILTER (WHERE history.outcome = 'updated')::BIGINT
                    AS updated,
                  COUNT(history.row_index) FILTER (WHERE history.outcome = 'skipped')::BIGINT
                    AS skipped,
                  job.created_at_utc,
                  job.completed_at_utc,
                  job.result_expires_at_utc AS credential_expires_at_utc,
                  (job.status = 1
                   AND job.result_expires_at_utc > clock_timestamp())
                    AS credentials_available,
                  COUNT(history.row_index) > 0 AS details_available
             FROM "AdminCredentialJobs" job
             JOIN "AspNetUsers" requester ON requester.id = job.requested_by
        LEFT JOIN "AdminUserImportHistoryRows" history
               ON history.operation_id = job.operation_id
            WHERE job.created_at_utc >= clock_timestamp() - INTERVAL '{IMPORT_HISTORY_DAYS} days'
              {operation_filter}
         GROUP BY job.operation_id, job.source_name, requester.user_name, requester.email,
                  job.status, job.row_count, job.created_at_utc, job.completed_at_utc,
                  job.result_expires_at_utc"#,
    )
}

/// `GET /api/admin/users/imports` — newest bounded import summaries.
pub async fn import_history(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(query): Query<ListQuery>,
) -> AppResult<ArrayResponse<ImportHistorySummary>> {
    let count = query.count.clamp(1, MAX_HISTORY_PAGE);
    let total: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::BIGINT
             FROM "AdminCredentialJobs"
            WHERE created_at_utc >= clock_timestamp() - INTERVAL '180 days'"#,
    )
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let sql = format!(
        "{} ORDER BY job.created_at_utc DESC, job.operation_id DESC LIMIT $1 OFFSET $2",
        history_summary_sql(false)
    );
    let rows = sqlx::query_as::<_, ImportHistorySummary>(&sql)
        .bind(i64::try_from(count).expect("history page bound fits i64"))
        .bind(i64::try_from(query.skip).unwrap_or(i64::MAX))
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(ArrayResponse::new(rows, total))
}

/// `GET /api/admin/users/imports/{operationId}` — one import and its row outcomes.
pub async fn import_history_detail(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(operation_id): Path<Uuid>,
) -> AppResult<Json<ImportHistoryDetail>> {
    if operation_id.is_nil() {
        return Err(AppError::bad_request(
            "A valid import operation ID is required",
        ));
    }
    let summary = sqlx::query_as::<_, ImportHistorySummary>(&history_summary_sql(true))
        .bind(operation_id)
        .fetch_optional(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::not_found("Import history not found"))?;
    let rows = sqlx::query_as::<_, ImportHistoryRow>(
        r#"SELECT history.row_index,
                  history.user_id,
                  account.id IS NOT NULL AS user_exists,
                  history.email,
                  history.real_name,
                  history.user_name,
                  history.team_name,
                  history.outcome AS status,
                  history.error,
                  CASE
                    WHEN history.last_mail_operation_id IS NOT NULL
                      AND mail.delivered_at_utc IS NOT NULL THEN 'Sent'
                    WHEN history.last_mail_operation_id IS NOT NULL
                      AND mail.dead_at_utc IS NOT NULL THEN 'Failed'
                    WHEN history.last_mail_operation_id IS NOT NULL THEN 'Queued'
                    WHEN history.direct_delivery_status = 1 THEN 'Sent'
                    WHEN history.direct_delivery_status = 2 THEN 'Failed'
                    ELSE 'NotSent'
                  END AS email_status,
                  CASE
                    WHEN history.last_mail_operation_id IS NOT NULL THEN mail.last_error
                    ELSE history.direct_delivery_error
                  END AS email_error,
                  COALESCE(mail.delivered_at_utc, mail.dead_at_utc,
                           history.delivery_attempted_at_utc, mail.created_at_utc)
                    AS email_attempted_at_utc
             FROM "AdminUserImportHistoryRows" history
        LEFT JOIN "AspNetUsers" account ON account.id = history.user_id
        LEFT JOIN "MailOutbox" mail ON mail.operation_id = history.last_mail_operation_id
            WHERE history.operation_id = $1
         ORDER BY history.row_index
            LIMIT 200"#,
    )
    .bind(operation_id)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(Json(ImportHistoryDetail { summary, rows }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_window_and_page_are_hard_bounded() {
        assert_eq!(IMPORT_HISTORY_DAYS, 180);
        assert_eq!(MAX_HISTORY_PAGE, 50);
        let sql = history_summary_sql(false);
        assert!(sql.contains("INTERVAL '180 days'"));
        assert!(sql.contains("COUNT(history.row_index)"));
        assert!(!sql.to_lowercase().contains("password"));
    }
}
