use super::*;
pub(crate) use crate::services::player_containers::operations::*;

pub(crate) struct OperationRequest {
    pub operation_id: Uuid,
    pub may_adopt_stale: bool,
}

pub(crate) fn operation_request(headers: &HeaderMap) -> AppResult<OperationRequest> {
    let Some(value) = headers.get("x-rsctf-operation-id") else {
        return Ok(OperationRequest {
            operation_id: Uuid::new_v4(),
            // A legacy caller cannot resend a server-generated identity. Its
            // next request may adopt only a compatible ambiguous stale row.
            may_adopt_stale: true,
        });
    };
    let value = value
        .to_str()
        .map_err(|_| AppError::bad_request("Invalid container operation ID"))?;
    let operation_id = Uuid::parse_str(value)
        .map_err(|_| AppError::bad_request("Invalid container operation ID"))?;
    if operation_id.is_nil() {
        return Err(AppError::bad_request(
            "Container operation ID must be opaque",
        ));
    }
    Ok(OperationRequest {
        operation_id,
        may_adopt_stale: false,
    })
}
