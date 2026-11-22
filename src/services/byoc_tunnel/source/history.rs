use super::*;
use axum::{
    http::header,
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};

#[derive(Deserialize)]
pub(crate) struct Page {
    before: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Event {
    revision: i64,
    status: String,
    #[serde(with = "crate::utils::datetime::millis")]
    first_seen: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    last_seen: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tracked_files: Option<usize>,
    changes: Vec<Change>,
}

#[derive(Serialize)]
struct Change {
    path: String,
    kind: &'static str,
}

fn changes(previous: Option<&Files>, files: &Files) -> Vec<Change> {
    let empty = Files::new();
    let before = previous.unwrap_or(&empty);
    before
        .keys()
        .chain(files.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter_map(|name| {
            let kind = match (before.get(name), files.get(name)) {
                (None, Some(None)) => "missing",
                (None, Some(Some(_))) if previous.is_none() => "present",
                (None, Some(Some(_))) => "registered",
                (Some(_), None) => "unregistered",
                (Some(None), Some(Some(_))) => "created",
                (Some(Some(_)), Some(None)) => "deleted",
                (Some(a), Some(b)) if a != b => "modified",
                _ => return None,
            };
            Some(Change {
                path: name.clone(),
                kind,
            })
        })
        .collect()
}

#[derive(sqlx::FromRow)]
struct Row {
    revision: i64,
    state: String,
    files: String,
    first_seen: DateTime<Utc>,
    last_seen: DateTime<Utc>,
}

pub(crate) async fn history(
    db: &mut sqlx::PgConnection,
    service: Option<i32>,
    page: Page,
) -> AppResult<Response> {
    read(db, service, page, HistoryTable::Byoc).await
}

pub(crate) async fn ssh_history(
    db: &mut sqlx::PgConnection,
    service: i32,
    page: Page,
) -> AppResult<Response> {
    read(db, Some(service), page, HistoryTable::HostedSsh).await
}

async fn read(
    db: &mut sqlx::PgConnection,
    service: Option<i32>,
    page: Page,
    table: HistoryTable,
) -> AppResult<Response> {
    let _permit = crate::utils::upload::reserve_buffered(8 * 1024 * 1024)?;
    let rows=sqlx::query_as::<_,Row>(&format!("SELECT revision,state,files,first_seen,last_seen FROM {} WHERE service_id=$1 AND revision<$2 ORDER BY revision DESC LIMIT 9",table.sql_name()))
        .bind(service).bind(page.before.unwrap_or(i64::MAX)).fetch_all(db).await.map_err(db_error)?;
    let more = rows.len() > 8;
    let mut previous = None;
    let mut events = Vec::new();
    for (i, row) in rows.into_iter().rev().enumerate() {
        let files: Files =
            serde_json::from_str(&row.files).map_err(|e| AppError::internal(e.to_string()))?;
        let observed = row.state == "observed";
        if i != 0 || !more {
            let diff = if observed {
                changes(previous.as_ref(), &files)
            } else {
                Vec::new()
            };
            events.push(Event {
                revision: row.revision,
                status: row.state,
                first_seen: row.first_seen,
                last_seen: row.last_seen,
                tracked_files: observed.then_some(files.len()),
                changes: diff,
            });
        }
        previous = observed.then_some(files);
    }
    events.reverse();
    let before = if more {
        events.last().map(|e| e.revision)
    } else {
        None
    };
    Ok((
        [(header::CACHE_CONTROL, "private, no-store")],
        Json(serde_json::json!({"events":events,"before":before})),
    )
        .into_response())
}
