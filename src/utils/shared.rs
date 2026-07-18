//! Response envelopes and pagination, faithful to RSCTF `Utils/Shared.cs`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

/// Successful response carrying `data`. RSCTF returns the **raw** model on
/// success (only errors use the `{title,status}` envelope), so this serializes
/// as just `data` — e.g. `GET /api/account/profile` -> `ProfileUserInfoModel`,
/// not `{title,data,status}`. Kept named `RequestResponse` so every existing
/// `RequestResponse::ok(model)` call site becomes raw with no churn.
#[derive(Debug)]
pub struct RequestResponse<T: Serialize> {
    pub data: T,
    pub status: u16,
}

impl<T: Serialize> RequestResponse<T> {
    pub fn ok(data: T) -> Self {
        Self { data, status: 200 }
    }
    pub fn with_status(data: T, status: u16) -> Self {
        Self { data, status }
    }
}

impl<T: Serialize> IntoResponse for RequestResponse<T> {
    fn into_response(self) -> Response {
        let code = StatusCode::from_u16(self.status).unwrap_or(StatusCode::OK);
        (code, Json(self.data)).into_response()
    }
}

/// The few endpoints RSCTF genuinely wraps in `RequestResponse<T>` on success
/// (`{title,data,status}`) — e.g. `register` -> `RequestResponseOfRegisterStatus`,
/// `changeemail` -> `RequestResponseOfBoolean`, `fingerprintchallenge`.
#[derive(Debug, Serialize)]
pub struct Wrapped<T: Serialize> {
    pub title: String,
    pub data: T,
    pub status: u16,
}

impl<T: Serialize> Wrapped<T> {
    pub fn ok(data: T) -> Self {
        Self {
            title: String::new(),
            data,
            status: 200,
        }
    }
}

impl<T: Serialize> IntoResponse for Wrapped<T> {
    fn into_response(self) -> Response {
        let code = StatusCode::from_u16(self.status).unwrap_or(StatusCode::OK);
        (code, Json(self)).into_response()
    }
}

/// RSCTF `RequestResponse` (no data) — `{ title, status }`.
#[derive(Debug, Serialize)]
pub struct MessageResponse {
    pub title: String,
    pub status: u16,
}

impl MessageResponse {
    pub fn new(title: impl Into<String>, status: u16) -> Self {
        Self {
            title: title.into(),
            status,
        }
    }
    pub fn ok(title: impl Into<String>) -> Self {
        Self::new(title, 200)
    }
}

impl IntoResponse for MessageResponse {
    fn into_response(self) -> Response {
        let code = StatusCode::from_u16(self.status).unwrap_or(StatusCode::OK);
        (code, Json(self)).into_response()
    }
}

/// RSCTF `ArrayResponse<T>` — a page of results plus the total row count.
#[derive(Debug, Serialize)]
pub struct ArrayResponse<T: Serialize> {
    pub data: Vec<T>,
    pub total: i64,
    pub length: usize,
}

impl<T: Serialize> ArrayResponse<T> {
    pub fn new(data: Vec<T>, total: i64) -> Self {
        Self {
            length: data.len(),
            total,
            data,
        }
    }
}

impl<T: Serialize> IntoResponse for ArrayResponse<T> {
    fn into_response(self) -> Response {
        Json(self).into_response()
    }
}

/// Standard `?count=&skip=` pagination query, matching RSCTF conventions.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageParams {
    #[serde(default = "default_count")]
    pub count: u64,
    #[serde(default)]
    pub skip: u64,
}

fn default_count() -> u64 {
    50
}

impl Default for PageParams {
    fn default() -> Self {
        Self {
            count: default_count(),
            skip: 0,
        }
    }
}

impl PageParams {
    /// Clamp `count` to a sane maximum to bound query cost.
    pub fn limit(&self) -> u64 {
        self.count.clamp(1, 500)
    }
}
