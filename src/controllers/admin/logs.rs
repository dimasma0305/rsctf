//! Audit-log listing.

use super::*;
use sea_orm::sea_query::{Expr, Func};
use sea_orm::ColumnTrait;

pub use crate::services::audit::LogMessageModel;

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
/// optional `level` filter and substring `search` across name / message / ip,
/// faithful to RSCTF `ILogRepository.GetLogs`. Returns the raw `LogMessageModel[]`.
pub async fn logs(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(q): Query<LogsQuery>,
) -> AppResult<RequestResponse<Vec<LogMessageModel>>> {
    let count = q.count.clamp(0, 1000);

    let mut base = log_entry::Entity::find();

    // `"All"` (the default sentinel) means "no level filter".
    if !q.level.is_empty() && q.level != "All" {
        base = base.filter(log_entry::Column::Level.eq(q.level.clone()));
    }

    if let Some(search) = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        // Case-insensitive substring match (RSCTF searches with ILIKE); mirror the
        // `Func::lower(col) LIKE %term%` pattern admin/users.rs already uses.
        let pat = format!("%{}%", search.to_lowercase());
        base = base.filter(
            Condition::any()
                .add(
                    Expr::expr(Func::lower(log_entry::Column::UserName.into_expr()))
                        .like(pat.as_str()),
                )
                .add(
                    Expr::expr(Func::lower(log_entry::Column::Message.into_expr()))
                        .like(pat.as_str()),
                )
                .add(
                    Expr::expr(Func::lower(log_entry::Column::RemoteIp.into_expr()))
                        .like(pat.as_str()),
                )
                .add(
                    Expr::expr(Func::lower(
                        log_entry::Column::BrowserFingerprint.into_expr(),
                    ))
                    .like(pat.as_str()),
                ),
        );
    }

    let rows = base
        .order_by_desc(log_entry::Column::TimeUtc)
        .offset(q.skip)
        .limit(count)
        .all(&st.db)
        .await?;

    let data = rows.into_iter().map(LogMessageModel::from).collect();
    Ok(RequestResponse::ok(data))
}
