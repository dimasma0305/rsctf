//! Audit-log listing.

use super::*;

pub use crate::services::audit::LogMessageModel;

const ADMIN_LOGS_SQL: &str = r#"
SELECT id,
       time_utc AS time,
       level,
       message AS msg,
       remote_ip AS ip,
       user_name AS name,
       status,
       browser_fingerprint AS fingerprint
  FROM "Logs"
 WHERE ($1 = 'All' OR $1 = '' OR level = $1)
   AND ($2::TEXT IS NULL
        OR LOWER(COALESCE(user_name, '')) LIKE $2
        OR LOWER(message) LIKE $2
        OR LOWER(COALESCE(remote_ip, '')) LIKE $2
        OR LOWER(COALESCE(browser_fingerprint, '')) LIKE $2)
 ORDER BY time_utc DESC, id DESC
 LIMIT $3 OFFSET $4
"#;

/// Log listing query (`?level=&count=&skip=&search=`). Mirrors RSCTF's
/// `Logs` action: `level` defaults to the `"All"` sentinel (no filter).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogsQuery {
    #[serde(default = "default_level")]
    pub level: String,
    #[serde(default = "default_log_count")]
    pub count: u64,
    #[serde(default)]
    pub skip: u64,
    #[serde(default)]
    pub search: Option<String>,
}

fn default_level() -> String {
    "All".to_string()
}

fn default_log_count() -> u64 {
    50
}

/// `GET /api/admin/logs` — page of audit-log rows, newest first, with an
/// optional `level` filter and substring `search` across name / message / ip /
/// fingerprint,
/// faithful to RSCTF `ILogRepository.GetLogs`. Returns the raw `LogMessageModel[]`.
pub async fn logs(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(q): Query<LogsQuery>,
) -> AppResult<RequestResponse<Vec<LogMessageModel>>> {
    let count = q.count.clamp(0, 1000) as i64;
    let skip = q.skip.min(i64::MAX as u64) as i64;
    let search = q
        .search
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("%{}%", value.to_lowercase()));

    let data = sqlx::query_as::<_, LogMessageModel>(ADMIN_LOGS_SQL)
        .bind(&q.level)
        .bind(search.as_deref())
        .bind(count)
        .bind(skip)
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    Ok(RequestResponse::ok(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_log_page_has_a_stable_timestamp_and_id_order() {
        let normalized = ADMIN_LOGS_SQL
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(normalized.starts_with("SELECT id,"));
        assert!(normalized.contains("ORDER BY time_utc DESC, id DESC"));
        assert!(normalized.contains("LIMIT $3 OFFSET $4"));
    }
}
