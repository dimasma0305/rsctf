//! HTTP operation identity parsing; durable state lives in the application service.

pub(super) use crate::services::exercise_containers::operations::*;
use crate::utils::error::{AppError, AppResult};
use axum::http::HeaderMap;
use uuid::Uuid;

pub(super) struct OperationRequest {
    pub(super) operation_id: Uuid,
    pub(super) may_adopt_stale: bool,
}

pub(super) fn operation_request(headers: &HeaderMap) -> AppResult<OperationRequest> {
    let Some(value) = headers.get("x-rsctf-operation-id") else {
        return Ok(OperationRequest {
            operation_id: Uuid::new_v4(),
            may_adopt_stale: true,
        });
    };
    let value = value
        .to_str()
        .map_err(|_| AppError::bad_request("Invalid exercise container operation ID"))?;
    let operation_id = Uuid::parse_str(value)
        .map_err(|_| AppError::bad_request("Invalid exercise container operation ID"))?;
    if operation_id.is_nil() {
        return Err(AppError::bad_request(
            "Exercise container operation ID must be opaque",
        ));
    }
    Ok(OperationRequest {
        operation_id,
        may_adopt_stale: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_operation_identity_is_validated() {
        let mut headers = HeaderMap::new();
        headers.insert("x-rsctf-operation-id", "not-a-uuid".parse().unwrap());
        assert!(operation_request(&headers).is_err());
        headers.insert(
            "x-rsctf-operation-id",
            Uuid::nil().to_string().parse().unwrap(),
        );
        assert!(operation_request(&headers).is_err());

        let legacy = operation_request(&HeaderMap::new()).unwrap();
        assert!(legacy.may_adopt_stale);
        assert!(!legacy.operation_id.is_nil());
    }
}
