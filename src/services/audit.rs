//! services/audit.rs — semantic audit logging into the `Logs` table.
//!
//! Mirrors RSCTF's `logger.Log(message, userName, ip, TaskStatus)` calls, which
//! its Serilog `DatabaseSink` persists at `Information` level. Unlike a
//! request-logging middleware, ONLY meaningful actions are recorded — user login/
//! register, team + game actions, admin changes — which is exactly what RSCTF's
//! `/admin/logs` page shows. Each write is best-effort: a logging failure never
//! propagates into the request.

use chrono::{DateTime, Utc};
use sea_orm::{ActiveModelTrait, Set};
use serde::Serialize;

use crate::app_state::SharedState;
use crate::models::data::log_entry;

/// Byte-identical model shared by the admin log list and the `ReceivedLog`
/// SignalR event. Keeping one serializer prevents the polled and pushed views
/// from drifting (especially timestamp units and camelCase field names).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogMessageModel {
    #[serde(with = "crate::utils::datetime::millis")]
    pub time: DateTime<Utc>,
    pub level: Option<String>,
    pub msg: Option<String>,
    pub ip: Option<String>,
    pub name: Option<String>,
    pub status: Option<String>,
    pub fingerprint: Option<String>,
}

impl From<log_entry::Model> for LogMessageModel {
    fn from(entry: log_entry::Model) -> Self {
        Self {
            time: entry.time_utc,
            level: Some(entry.level),
            msg: Some(entry.message),
            ip: entry.remote_ip,
            name: entry.user_name,
            status: entry.status,
            fingerprint: entry.browser_fingerprint,
        }
    }
}

/// Insert one audit row (best-effort). Shared by every public helper so there is
/// a single `Logs` insert site; `fingerprint` populates the `browser_fingerprint`
/// column (RSCTF `LogModel.BrowserFingerprint`).
#[allow(clippy::too_many_arguments)]
async fn write(
    st: &SharedState,
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
    let Ok(inserted) = entry.insert(&st.db).await else {
        return;
    };
    if let Ok(payload) = serde_json::to_string(&LogMessageModel::from(inserted)) {
        // EventBus fans this out across replicas. Publishing remains
        // best-effort just like the audit insert itself.
        st.publish_event("ReceivedLog", None, payload);
    }
}

/// Record one audit event. `status` is a `TaskStatus` name (`"Success"`,
/// `"Failed"`, `"Denied"`, …); `logger` is the source context RSCTF uses as the
/// column value (e.g. `"AccountController"`).
pub async fn log(
    st: &SharedState,
    level: &str,
    logger: &str,
    user_name: Option<String>,
    remote_ip: Option<String>,
    status: &str,
    message: impl Into<String>,
) {
    write(
        st, level, logger, user_name, remote_ip, status, None, message,
    )
    .await;
}

/// The common case: a successful `Information`-level event.
pub async fn info(
    st: &SharedState,
    logger: &str,
    user_name: Option<String>,
    remote_ip: Option<String>,
    message: impl Into<String>,
) {
    write(
        st,
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
    st: &SharedState,
    logger: &str,
    user_name: Option<String>,
    remote_ip: Option<String>,
    fingerprint: Option<String>,
    message: impl Into<String>,
) {
    write(
        st,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pushed_log_uses_the_same_millisecond_wire_contract_as_admin_reads() {
        let at = DateTime::parse_from_rfc3339("2026-07-20T06:00:00.123Z")
            .unwrap()
            .with_timezone(&Utc);
        let model = LogMessageModel::from(log_entry::Model {
            id: 7,
            time_utc: at,
            level: "Information".to_string(),
            logger: "AdminController".to_string(),
            remote_ip: Some("192.0.2.7".to_string()),
            user_name: Some("admin".to_string()),
            message: "created fixture".to_string(),
            status: Some("Success".to_string()),
            browser_fingerprint: Some("fixture-fingerprint".to_string()),
        });

        let value = serde_json::to_value(model).unwrap();
        assert_eq!(value["time"], at.timestamp_millis());
        assert_eq!(value["level"], "Information");
        assert_eq!(value["msg"], "created fixture");
        assert_eq!(value["ip"], "192.0.2.7");
        assert_eq!(value["name"], "admin");
        assert_eq!(value["status"], "Success");
        assert_eq!(value["fingerprint"], "fixture-fingerprint");
    }
}
