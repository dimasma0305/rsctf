//! Unified error type. Every handler returns `Result<_, AppError>`; the
//! `IntoResponse` impl renders the RSCTF `RequestResponse { title, status }`
//! envelope so error bodies match the original API shape.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

/// True if a `sqlx` error is a Postgres unique-constraint violation (SQLSTATE 23505).
/// Lets a concurrent-insert race (e.g. a double-clicked round advance hitting
/// `ux_adrounds_game_number`) be treated as a no-op / 409 rather than a 500.
pub fn is_unique_violation(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(d) if d.code().as_deref() == Some("23505"))
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    BadRequest(String),

    #[error("unauthorized")]
    Unauthorized,

    #[error("forbidden")]
    Forbidden,

    #[error("{0}")]
    NotFound(String),

    #[error("{0}")]
    Conflict(String),

    #[error("too many requests")]
    TooManyRequests,

    #[error("{0}")]
    ServiceUnavailable(String),

    /// Carries a RSCTF numeric `ErrorCode` in the response body distinct from the
    /// HTTP status (e.g. 10001/10002 for game-not-started/ended, which the React
    /// client keys on to redirect). `http` is the transport status; `code` is the
    /// numeric ErrorCode rendered into `ErrorBody.status`.
    #[error("{title}")]
    Coded {
        http: StatusCode,
        code: u16,
        title: String,
    },

    #[error("validation failed: {0}")]
    Validation(String),

    #[error(transparent)]
    Database(#[from] sea_orm::DbErr),

    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl AppError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        AppError::BadRequest(msg.into())
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        AppError::NotFound(msg.into())
    }
    pub fn conflict(msg: impl Into<String>) -> Self {
        AppError::Conflict(msg.into())
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        AppError::Internal(anyhow::anyhow!(msg.into()))
    }
    pub fn unavailable(msg: impl Into<String>) -> Self {
        AppError::ServiceUnavailable(msg.into())
    }

    /// RSCTF `ErrorCode.GameEnded` (10002): 400 with the numeric code in the body.
    /// The React `TeamRank` redirects when `error.status === 10002`.
    pub fn game_ended() -> Self {
        AppError::Coded {
            http: StatusCode::BAD_REQUEST,
            code: 10002,
            title: "Game has ended".into(),
        }
    }

    /// RSCTF `ErrorCode.GameNotStarted` (10001): 400 with the numeric code in the body.
    pub fn game_not_started() -> Self {
        AppError::Coded {
            http: StatusCode::BAD_REQUEST,
            code: 10001,
            title: "Game has not started".into(),
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            AppError::BadRequest(_) | AppError::Validation(_) => StatusCode::BAD_REQUEST,
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::Forbidden => StatusCode::FORBIDDEN,
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::Conflict(_) => StatusCode::CONFLICT,
            AppError::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
            AppError::ServiceUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            AppError::Coded { http, .. } => *http,
            AppError::Database(_) | AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    title: String,
    status: u16,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        // Never leak internal error details to clients.
        let title = match &self {
            AppError::Database(_) | AppError::Internal(_) => {
                tracing::error!(error = %self, "internal error");
                "Internal server error".to_string()
            }
            other => other.to_string(),
        };
        // Coded errors carry a RSCTF numeric ErrorCode in the body, distinct from
        // the HTTP transport status; everything else reflects the HTTP status.
        let body_status = match &self {
            AppError::Coded { code, .. } => *code,
            _ => status.as_u16(),
        };
        let body = ErrorBody {
            title,
            status: body_status,
        };
        (status, Json(body)).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;
