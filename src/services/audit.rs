//! services/audit.rs — semantic audit logging into the `Logs` table.
//!
//! Mirrors RSCTF's `logger.Log(message, userName, ip, TaskStatus)` calls, which
//! its Serilog `DatabaseSink` persists at `Information` level. Unlike a
//! request-logging middleware, ONLY meaningful actions are recorded — user login/
//! register, team + game actions, admin changes — which is exactly what RSCTF's
//! `/admin/logs` page shows. Each write is best-effort: a logging failure never
//! propagates into the request.

use chrono::Utc;
use sea_orm::{ActiveModelTrait, DatabaseConnection, Set};

use crate::models::data::log_entry;

/// Insert one audit row (best-effort). Shared by every public helper so there is
/// a single `Logs` insert site; `fingerprint` populates the `browser_fingerprint`
/// column (RSCTF `LogModel.BrowserFingerprint`).
#[allow(clippy::too_many_arguments)]
async fn write(
    db: &DatabaseConnection,
    level: &str,
    logger: &str,
    user_name: Option<String>,
    remote_ip: Option<String>,
    status: &str,
    fingerprint: Option<String>,
    message: impl Into<String>,
) {
    let entry = log_entry::ActiveModel {
        time_utc: Set(Utc::now()),
        level: Set(level.to_string()),
        logger: Set(logger.to_string()),
        remote_ip: Set(remote_ip),
        user_name: Set(user_name),
        message: Set(message.into()),
        status: Set(Some(status.to_string())),
        browser_fingerprint: Set(fingerprint),
        ..Default::default()
    };
    let _ = entry.insert(db).await;
}

/// Record one audit event. `status` is a `TaskStatus` name (`"Success"`,
/// `"Failed"`, `"Denied"`, …); `logger` is the source context RSCTF uses as the
/// column value (e.g. `"AccountController"`).
pub async fn log(
    db: &DatabaseConnection,
    level: &str,
    logger: &str,
    user_name: Option<String>,
    remote_ip: Option<String>,
    status: &str,
    message: impl Into<String>,
) {
    write(
        db, level, logger, user_name, remote_ip, status, None, message,
    )
    .await;
}

/// The common case: a successful `Information`-level event.
pub async fn info(
    db: &DatabaseConnection,
    logger: &str,
    user_name: Option<String>,
    remote_ip: Option<String>,
    message: impl Into<String>,
) {
    write(
        db,
        "Information",
        logger,
        user_name,
        remote_ip,
        "Success",
        None,
        message,
    )
    .await;
}

/// Like [`info`], but also records the submitting browser fingerprint into the
/// row's `browser_fingerprint` column — RSCTF attaches the fingerprint to the
/// `Account_UserLogined` success event (`logger.Log(..., fingerprint: …)`), which
/// the admin Logs table renders in its `fingerprint` column. Best-effort.
pub async fn info_with_fingerprint(
    db: &DatabaseConnection,
    logger: &str,
    user_name: Option<String>,
    remote_ip: Option<String>,
    fingerprint: Option<String>,
    message: impl Into<String>,
) {
    write(
        db,
        "Information",
        logger,
        user_name,
        remote_ip,
        "Success",
        fingerprint,
        message,
    )
    .await;
}
