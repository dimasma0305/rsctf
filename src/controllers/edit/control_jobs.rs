//! Manager-visible recovery reads for durable control-plane operations.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use uuid::Uuid;

use super::{manager_or_admin, CurrentUser, SharedState};
use crate::services::control_jobs::ControlJobModel;
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

pub(crate) const OPERATION_ID_HEADER: &str = "idempotency-key";

pub(crate) fn operation_id(headers: &HeaderMap) -> AppResult<Uuid> {
    let raw = headers
        .get(OPERATION_ID_HEADER)
        .ok_or_else(|| AppError::bad_request("Idempotency-Key header is required"))?
        .to_str()
        .map_err(|_| AppError::bad_request("Idempotency-Key must be an ASCII UUID"))?;
    Uuid::parse_str(raw).map_err(|_| AppError::bad_request("Idempotency-Key must be a UUID"))
}

pub(crate) use crate::services::control_jobs::fingerprint;

async fn authorize_job(
    st: &SharedState,
    user: &CurrentUser,
    job: Option<ControlJobModel>,
) -> AppResult<ControlJobModel> {
    let job = job.ok_or_else(|| AppError::not_found("Control job not found"))?;
    manager_or_admin(st, user, job.game_id).await?;
    Ok(job)
}

#[utoipa::path(
    get, path = "/api/edit/jobs/{jobId}", tag = "ControlJobs",
    params(("jobId" = Uuid, Path, description = "Durable job ID")),
    responses(
        (status = 200, description = "Raw control job model", body = ControlJobModel),
        (status = 401, description = "Authentication required", body = crate::utils::error::ErrorBody),
        (status = 403, description = "Event manager or administrator required", body = crate::utils::error::ErrorBody),
        (status = 404, description = "Job not found", body = crate::utils::error::ErrorBody)
    ),
    security(("session" = []), ("bearer" = []))
)]
pub async fn get_control_job(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(job_id): Path<Uuid>,
) -> AppResult<RequestResponse<ControlJobModel>> {
    let job = authorize_job(
        &st,
        &user,
        crate::services::control_jobs::get(st.pg(), job_id).await?,
    )
    .await?;
    Ok(RequestResponse::ok(job))
}

#[utoipa::path(
    get, path = "/api/edit/jobs/operations/{operationId}", tag = "ControlJobs",
    params(("operationId" = Uuid, Path, description = "Original idempotency operation ID")),
    responses(
        (status = 200, description = "Raw control job model", body = ControlJobModel),
        (status = 401, description = "Authentication required", body = crate::utils::error::ErrorBody),
        (status = 403, description = "Event manager or administrator required", body = crate::utils::error::ErrorBody),
        (status = 404, description = "Operation not found", body = crate::utils::error::ErrorBody)
    ),
    security(("session" = []), ("bearer" = []))
)]
pub async fn get_control_job_by_operation(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(operation_id): Path<Uuid>,
) -> AppResult<RequestResponse<ControlJobModel>> {
    let job = authorize_job(
        &st,
        &user,
        crate::services::control_jobs::get_by_operation(st.pg(), operation_id).await?,
    )
    .await?;
    Ok(RequestResponse::ok(job))
}

#[utoipa::path(
    post, path = "/api/edit/jobs/{jobId}", tag = "ControlJobs",
    params(("jobId" = Uuid, Path, description = "Durable job ID")),
    responses(
        (status = 200, description = "Raw job with cancellation intent", body = ControlJobModel),
        (status = 409, description = "Cancellation not supported for this running operation", body = crate::utils::error::ErrorBody),
        (status = 401, description = "Authentication required", body = crate::utils::error::ErrorBody),
        (status = 403, description = "Event manager or administrator required", body = crate::utils::error::ErrorBody),
        (status = 404, description = "Job not found", body = crate::utils::error::ErrorBody)
    ),
    security(("session" = []), ("bearer" = []))
)]
pub async fn cancel_control_job(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(job_id): Path<Uuid>,
) -> AppResult<RequestResponse<ControlJobModel>> {
    authorize_job(
        &st,
        &user,
        crate::services::control_jobs::get(st.pg(), job_id).await?,
    )
    .await?;
    let job = crate::services::control_jobs::request_cancellation(st.pg(), job_id).await?;
    Ok(RequestResponse::ok(job))
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};
    use uuid::Uuid;

    use super::{operation_id, OPERATION_ID_HEADER};

    #[test]
    fn operation_identity_is_strict_and_opaque() {
        let mut headers = HeaderMap::new();
        assert!(operation_id(&headers).is_err());
        headers.insert(OPERATION_ID_HEADER, HeaderValue::from_static("not-a-uuid"));
        assert!(operation_id(&headers).is_err());
        let expected = Uuid::new_v4();
        headers.insert(
            OPERATION_ID_HEADER,
            HeaderValue::from_str(&expected.to_string()).unwrap(),
        );
        assert_eq!(operation_id(&headers).unwrap(), expected);
    }
}
