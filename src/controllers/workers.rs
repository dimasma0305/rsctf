//! Administration and one-time enrollment for trusted outbound workers.
//!
//! These routes deliberately live outside the ordinary controller tree: split
//! deployments expose them only from the singleton network/control owner. A
//! worker keeps its private key locally and exchanges a short-lived opaque
//! token plus CSR for a client-only certificate signed by the worker CA.

use axum::extract::{Path, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, PRAGMA};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use rsctf_worker_protocol::{EnrollmentRequest, EnrollmentResponse};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::AdminUser;
use crate::middlewares::rate_limiter::{limited, Policy};
use crate::services::worker_store::{
    CreateWorker, WorkerAdministrativeState, WorkerCertificate, WorkerNode, WorkerStoreError,
};
use crate::utils::codec::random_token;
use crate::utils::error::{AppError, AppResult};

const ENROLLMENT_TOKEN_BYTES: usize = 32;
const ENROLLMENT_TOKEN_LIFETIME_MINUTES: i64 = 15;
const MAX_WORKER_NAME_CHARS: usize = 128;
const MAX_ENROLLMENT_TOKEN_CHARS: usize = 1_024;
const MAX_CSR_BYTES: usize = 64 * 1024;
const WORKER_SECRET_CACHE_CONTROL: &str = "private, no-store";
const WORKER_BOOTSTRAP: &str = include_str!("../../scripts/bootstrap-worker.sh");
const WINDOWS_WORKER_BOOTSTRAP: &str = include_str!("../../scripts/bootstrap-worker.ps1");

pub fn public_router() -> Router<SharedState> {
    Router::new()
        .route("/install/worker", get(worker_bootstrap))
        .route("/install/worker.ps1", get(windows_worker_bootstrap))
}

pub fn router() -> Router<SharedState> {
    Router::new()
        .route(
            "/api/admin/workers",
            limited(Policy::Query, get(list_workers))
                .merge(limited(Policy::Register, post(create_worker))),
        )
        .route(
            "/api/admin/workers/{id}/token",
            limited(Policy::Register, post(issue_enrollment_token)),
        )
        .route(
            "/api/admin/workers/{id}/state",
            limited(Policy::Container, put(update_worker_state)),
        )
        .route(
            "/api/workers/enroll",
            limited(Policy::Register, post(enroll_worker)),
        )
}

/// Public installer bootstrap. Downloading software grants no worker access;
/// enrollment still requires a short-lived, one-use token and then mTLS.
async fn worker_bootstrap() -> impl IntoResponse {
    (
        [
            (CONTENT_TYPE, "text/x-shellscript; charset=utf-8"),
            (CACHE_CONTROL, "public, max-age=300"),
        ],
        WORKER_BOOTSTRAP,
    )
}

async fn windows_worker_bootstrap() -> impl IntoResponse {
    (
        [
            (CONTENT_TYPE, "text/plain; charset=utf-8"),
            (CACHE_CONTROL, "public, max-age=300"),
        ],
        WINDOWS_WORKER_BOOTSTRAP,
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateWorkerModel {
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateWorkerStateModel {
    pub state: WorkerStateModel,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub enum WorkerStateModel {
    Enabled,
    Draining,
    Disabled,
}

impl From<WorkerStateModel> for WorkerAdministrativeState {
    fn from(value: WorkerStateModel) -> Self {
        match value {
            WorkerStateModel::Enabled => Self::Enabled,
            WorkerStateModel::Draining => Self::Draining,
            WorkerStateModel::Disabled => Self::Disabled,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerCapacityModel {
    pub cpu_millis: i64,
    pub memory_bytes: i64,
    pub slots: i32,
}

/// Administrative worker metadata. Enrollment secrets and certificate
/// fingerprints are intentionally absent.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerModel {
    pub id: Uuid,
    pub name: String,
    pub administrative_state: WorkerStateModel,
    pub platform_os: Option<String>,
    pub architecture: Option<String>,
    pub runtime_kind: Option<String>,
    pub runtime_version: Option<String>,
    pub labels: Value,
    pub capabilities: Value,
    pub capacity: WorkerCapacityModel,
    pub certificate_serial: Option<String>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub certificate_expires_at: Option<DateTime<Utc>>,
    pub online: bool,
    pub session_id: Option<Uuid>,
    pub session_epoch: i64,
    pub boot_id: Option<Uuid>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub heartbeat_at: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub lease_expires_at: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub created_at: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub updated_at: DateTime<Utc>,
}

impl From<WorkerNode> for WorkerModel {
    fn from(worker: WorkerNode) -> Self {
        let online = worker.session_id.is_some()
            && worker
                .lease_expires_at
                .as_ref()
                .is_some_and(|expires_at| *expires_at > Utc::now());
        Self {
            id: worker.id,
            name: worker.name,
            administrative_state: match worker.administrative_state {
                WorkerAdministrativeState::Enabled => WorkerStateModel::Enabled,
                WorkerAdministrativeState::Draining => WorkerStateModel::Draining,
                WorkerAdministrativeState::Disabled => WorkerStateModel::Disabled,
            },
            platform_os: worker
                .platform_os
                .map(|platform| platform.as_str().to_owned()),
            architecture: worker.architecture,
            runtime_kind: worker.runtime_kind,
            runtime_version: worker.runtime_version,
            labels: worker.labels,
            capabilities: worker.capabilities,
            capacity: WorkerCapacityModel {
                cpu_millis: worker.capacity.cpu_millis,
                memory_bytes: worker.capacity.memory_bytes,
                slots: worker.capacity.slots,
            },
            certificate_serial: worker.certificate_serial,
            certificate_expires_at: worker.certificate_expires_at,
            online,
            session_id: worker.session_id,
            session_epoch: worker.session_epoch,
            boot_id: worker.boot_id,
            heartbeat_at: worker.heartbeat_at,
            lease_expires_at: worker.lease_expires_at,
            created_at: worker.created_at,
            updated_at: worker.updated_at,
        }
    }
}

/// One-use secret. This type deliberately omits `Debug` so accidental handler
/// logging cannot expose the enrollment credential.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollmentTokenModel {
    pub worker_id: Uuid,
    pub token: String,
    #[serde(with = "crate::utils::datetime::millis")]
    pub expires_at: DateTime<Utc>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedWorkerModel {
    pub worker: WorkerModel,
    pub enrollment: EnrollmentTokenModel,
}

/// `GET /api/admin/workers` — operational metadata only; never returns token
/// hashes, certificate fingerprints, or CA material.
pub async fn list_workers(
    State(st): State<SharedState>,
    _admin: AdminUser,
) -> AppResult<Json<Vec<WorkerModel>>> {
    let workers = st.worker_store.list_workers().await.map_err(store_error)?;
    Ok(Json(workers.into_iter().map(WorkerModel::from).collect()))
}

/// `POST /api/admin/workers` — create an enabled identity and reveal its
/// 15-minute token exactly once.
pub async fn create_worker(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Json(model): Json<CreateWorkerModel>,
) -> AppResult<Response> {
    require_issuer(&st)?;
    let name = validate_worker_name(model.name)?;
    let worker_id = Uuid::now_v7();
    let enrollment = new_enrollment_token(worker_id)?;
    let worker = st
        .worker_store
        .create_worker(CreateWorker {
            id: worker_id,
            name,
            enrollment_token_hash: hash_enrollment_token(&enrollment.token),
            enrollment_token_expires_at: enrollment.expires_at,
        })
        .await
        .map_err(store_error)?;

    Ok(worker_secret_response(Json(CreatedWorkerModel {
        worker: worker.into(),
        enrollment,
    })))
}

/// `POST /api/admin/workers/{id}/token` — invalidate any unused token and
/// issue a new 15-minute credential. Existing certificates remain valid until
/// the new CSR is exchanged.
pub async fn issue_enrollment_token(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(worker_id): Path<Uuid>,
) -> AppResult<Response> {
    require_issuer(&st)?;
    let enrollment = new_enrollment_token(worker_id)?;
    let updated = st
        .worker_store
        .issue_enrollment_token(
            worker_id,
            hash_enrollment_token(&enrollment.token),
            enrollment.expires_at,
        )
        .await
        .map_err(store_error)?;
    if !updated {
        return Err(AppError::not_found("Worker not found"));
    }
    Ok(worker_secret_response(Json(enrollment)))
}

/// `PUT /api/admin/workers/{id}/state` — draining keeps the active route but
/// prevents placement; disabling durably fences and immediately drops the live
/// control/data session.
pub async fn update_worker_state(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(worker_id): Path<Uuid>,
    Json(model): Json<UpdateWorkerStateModel>,
) -> AppResult<Json<WorkerModel>> {
    let state = WorkerAdministrativeState::from(model.state);
    let updated = st
        .worker_store
        .set_administrative_state(worker_id, state)
        .await
        .map_err(store_error)?;
    if !updated {
        return Err(AppError::not_found("Worker not found"));
    }

    if state == WorkerAdministrativeState::Disabled {
        if let Some(service) = &st.workers {
            service.registry().disconnect(worker_id).await;
        }
    }

    let worker = st
        .worker_store
        .get_worker(worker_id)
        .await
        .map_err(store_error)?
        .ok_or_else(|| AppError::not_found("Worker not found"))?;
    Ok(Json(worker.into()))
}

/// `POST /api/workers/enroll` — validate the one-use token, sign the worker's
/// locally generated CSR, then atomically consume the token while binding the
/// exact certificate fingerprint.
pub async fn enroll_worker(
    State(st): State<SharedState>,
    Json(request): Json<EnrollmentRequest>,
) -> AppResult<Response> {
    validate_enrollment_request(&request)?;
    let issuer = require_issuer(&st)?.clone();
    let token_hash = hash_enrollment_token(&request.token);
    let worker_id = st
        .worker_store
        .resolve_enrollment_token(token_hash)
        .await
        .map_err(store_error)?
        .ok_or(AppError::Unauthorized)?;

    let csr_pem = request.csr_pem;
    let signing_issuer = issuer.clone();
    let issued = tokio::task::spawn_blocking(move || signing_issuer.issue(worker_id, &csr_pem))
        .await
        .map_err(|error| AppError::internal(format!("worker certificate task failed: {error}")))?
        .map_err(|error| {
            tracing::warn!(%worker_id, %error, "worker CSR rejected");
            AppError::bad_request("Invalid worker certificate signing request")
        })?;

    let enrolled = st
        .worker_store
        .enroll_certificate(
            token_hash,
            WorkerCertificate {
                fingerprint_sha256: issued.fingerprint_sha256,
                serial: issued.serial,
                expires_at: issued.expires_at,
            },
        )
        .await
        .map_err(store_error)?
        .ok_or(AppError::Unauthorized)?;
    if enrolled.id != worker_id {
        return Err(AppError::internal(
            "enrollment token resolved to a different worker",
        ));
    }

    // Re-enrollment supersedes an existing certificate. The database fence is
    // authoritative; dropping the old socket makes revocation immediate too.
    if let Some(service) = &st.workers {
        service.registry().disconnect(worker_id).await;
    }

    Ok(worker_secret_response(Json(EnrollmentResponse {
        worker_id,
        control_address: issuer.public_endpoint().to_owned(),
        data_address: issuer.public_endpoint().to_owned(),
        server_name: issuer.server_name().to_owned(),
        certificate_pem: issued.certificate_pem,
        ca_pem: issued.ca_certificate_pem,
    })))
}

/// Enrollment tokens and newly-issued certificate material are one-time
/// operator secrets. Browsers and intermediary caches must never retain them.
fn worker_secret_response(body: impl IntoResponse) -> Response {
    (
        [
            (CACHE_CONTROL, WORKER_SECRET_CACHE_CONTROL),
            (PRAGMA, "no-cache"),
        ],
        body,
    )
        .into_response()
}

fn require_issuer(
    st: &SharedState,
) -> AppResult<&std::sync::Arc<crate::services::worker_pki::WorkerIssuer>> {
    st.worker_issuer
        .as_ref()
        .ok_or_else(|| AppError::unavailable("Trusted worker enrollment is not configured"))
}

fn validate_worker_name(name: String) -> AppResult<String> {
    let name = name.trim().to_owned();
    if name.is_empty() {
        return Err(AppError::bad_request("Worker name is required"));
    }
    if name.chars().count() > MAX_WORKER_NAME_CHARS {
        return Err(AppError::bad_request(format!(
            "Worker name must be at most {MAX_WORKER_NAME_CHARS} characters"
        )));
    }
    Ok(name)
}

fn validate_enrollment_request(request: &EnrollmentRequest) -> AppResult<()> {
    if request.token.is_empty() || request.token.len() > MAX_ENROLLMENT_TOKEN_CHARS {
        return Err(AppError::Unauthorized);
    }
    if request.csr_pem.is_empty() || request.csr_pem.len() > MAX_CSR_BYTES {
        return Err(AppError::bad_request(
            "Invalid worker certificate signing request",
        ));
    }
    Ok(())
}

fn new_enrollment_token(worker_id: Uuid) -> AppResult<EnrollmentTokenModel> {
    let expires_at = Utc::now()
        .checked_add_signed(Duration::minutes(ENROLLMENT_TOKEN_LIFETIME_MINUTES))
        .ok_or_else(|| AppError::internal("worker enrollment expiry is out of range"))?;
    Ok(EnrollmentTokenModel {
        worker_id,
        token: random_token(ENROLLMENT_TOKEN_BYTES),
        expires_at,
    })
}

fn hash_enrollment_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn store_error(error: WorkerStoreError) -> AppError {
    match error {
        WorkerStoreError::InvalidInput(message) => AppError::bad_request(message),
        WorkerStoreError::Conflict(message) => AppError::conflict(message),
        WorkerStoreError::Database(error) => {
            AppError::internal(format!("worker store database error: {error}"))
        }
        WorkerStoreError::InvalidStoredData(message) => {
            AppError::internal(format!("invalid worker store data: {message}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrollment_tokens_have_expected_entropy_and_hash_shape() {
        let token = new_enrollment_token(Uuid::nil()).expect("token");
        assert!(token.token.len() >= 43);
        assert_eq!(hash_enrollment_token(&token.token).len(), 32);
        assert!(token.expires_at > Utc::now() + Duration::minutes(14));
    }

    #[test]
    fn bootstrap_keeps_enrollment_secrets_out_of_process_arguments() {
        assert!(WORKER_BOOTSTRAP.contains("read -r -s ENROLLMENT_TOKEN </dev/tty"));
        assert!(WORKER_BOOTSTRAP.contains("--token-stdin"));
        assert!(!WORKER_BOOTSTRAP.contains("--token \"$ENROLLMENT_TOKEN\""));
        assert!(!WORKER_BOOTSTRAP.contains("?token="));
        assert!(WINDOWS_WORKER_BOOTSTRAP
            .contains("Read-Host 'One-time enrollment token' -AsSecureString"));
        assert!(WINDOWS_WORKER_BOOTSTRAP.contains("--token-stdin"));
        assert!(!WINDOWS_WORKER_BOOTSTRAP.contains("--token $plainToken"));
        assert!(!WINDOWS_WORKER_BOOTSTRAP.contains("?token="));
    }

    #[tokio::test]
    async fn public_bootstrap_has_script_content_type_and_cache_policy() {
        let response = worker_bootstrap().await.into_response();
        assert_eq!(
            response.headers()[CONTENT_TYPE],
            "text/x-shellscript; charset=utf-8"
        );
        assert_eq!(response.headers()[CACHE_CONTROL], "public, max-age=300");
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("bootstrap body");
        assert_eq!(body.as_ref(), WORKER_BOOTSTRAP.as_bytes());

        let windows_response = windows_worker_bootstrap().await.into_response();
        assert_eq!(
            windows_response.headers()[CONTENT_TYPE],
            "text/plain; charset=utf-8"
        );
        assert_eq!(
            windows_response.headers()[CACHE_CONTROL],
            "public, max-age=300"
        );
        let windows_body = axum::body::to_bytes(windows_response.into_body(), 1024 * 1024)
            .await
            .expect("Windows bootstrap body");
        assert_eq!(windows_body.as_ref(), WINDOWS_WORKER_BOOTSTRAP.as_bytes());
    }

    #[test]
    fn worker_state_uses_string_wire_values() {
        assert_eq!(
            serde_json::to_value(WorkerStateModel::Draining).unwrap(),
            serde_json::json!("Draining")
        );
        let request: UpdateWorkerStateModel =
            serde_json::from_value(serde_json::json!({ "state": "Disabled" })).unwrap();
        assert!(matches!(request.state, WorkerStateModel::Disabled));
    }

    #[test]
    fn validates_name_and_enrollment_input_bounds() {
        assert!(validate_worker_name("  worker-1  ".into()).is_ok());
        assert!(validate_worker_name("  ".into()).is_err());
        assert!(validate_worker_name("x".repeat(MAX_WORKER_NAME_CHARS + 1)).is_err());

        let oversized = EnrollmentRequest {
            token: "x".repeat(MAX_ENROLLMENT_TOKEN_CHARS + 1),
            csr_pem: "csr".into(),
        };
        assert!(validate_enrollment_request(&oversized).is_err());
    }

    #[test]
    fn one_time_worker_material_is_private_and_never_cached() {
        let response = worker_secret_response(Json(serde_json::json!({
            "token": "one-time-secret",
        })));
        assert_eq!(
            response.headers().get(CACHE_CONTROL).unwrap(),
            WORKER_SECRET_CACHE_CONTROL
        );
        assert_eq!(response.headers().get(PRAGMA).unwrap(), "no-cache");
    }
}
