//! HTTP authorization and request parsing for A&D reconcile admission.
use super::super::*;
pub(crate) use crate::services::ad::provision::*;
use axum::http::HeaderMap;
use uuid::Uuid;

const IDEMPOTENCY_KEY_HEADER: &str = "idempotency-key";

fn reconcile_operation_id(headers: &HeaderMap) -> AppResult<Uuid> {
    let Some(raw) = headers.get(IDEMPOTENCY_KEY_HEADER) else {
        // Keep cached pre-idempotency clients and existing operator automation
        // working during a rolling deployment. Such a request gets protection
        // within this attempt, but only a client-supplied key can span retries.
        return Ok(Uuid::new_v4());
    };
    let raw = raw
        .to_str()
        .map_err(|_| AppError::bad_request("Idempotency-Key must be an ASCII UUID"))?;
    Uuid::parse_str(raw).map_err(|_| AppError::bad_request("Idempotency-Key must be a UUID"))
}

/// `POST /api/edit/games/{id}/ad/EnsureContainers` -> void.
///
/// Launch the platform-hosted A&D service container for every (accepted team,
/// platform-hosted A&D challenge) that doesn't already have one, register its
/// host:port, and plant the team's flag. Idempotent: services that already have
/// a `container_id` are skipped. Thin wrapper over [`ensure_ad_containers`]
/// (whole game, every accepted team).
pub async fn ad_ensure_containers(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
    headers: HeaderMap,
) -> AppResult<(
    axum::http::StatusCode,
    RequestResponse<crate::services::control_jobs::ControlJobModel>,
)> {
    manager_or_admin(&st, &user, game_id).await?;
    let operation = reconcile_operation_id(&headers)?;
    let input = serde_json::json!({ "ensureVpn": true, "ensureKoth": true });
    let fingerprint = super::super::control_jobs::fingerprint(&input)?;
    let job = crate::services::control_jobs::enqueue(
        st.pg(),
        crate::services::control_jobs::ControlJobKind::AdReconcile,
        &format!("game:{game_id}"),
        game_id,
        None,
        operation,
        &fingerprint,
        input,
    )
    .await?;
    crate::services::control_jobs::merge_reconcile_input(
        st.pg(),
        job.id,
        serde_json::json!({ "ensureVpn": true, "ensureKoth": true }),
    )
    .await?;
    let job = crate::services::control_jobs::get(st.pg(), job.id)
        .await?
        .ok_or_else(|| AppError::internal("queued reconcile job disappeared"))?;
    crate::services::control_jobs::kick(st);
    Ok((axum::http::StatusCode::ACCEPTED, RequestResponse::ok(job)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    #[test]
    fn reconcile_identity_is_backward_compatible_and_binds_each_service_retry() {
        let mut headers = HeaderMap::new();
        assert!(!reconcile_operation_id(&headers).unwrap().is_nil());
        headers.insert(
            IDEMPOTENCY_KEY_HEADER,
            HeaderValue::from_static("not-a-uuid"),
        );
        assert!(reconcile_operation_id(&headers).is_err());

        let first = Uuid::new_v4();
        headers.insert(
            IDEMPOTENCY_KEY_HEADER,
            HeaderValue::from_str(&first.to_string()).unwrap(),
        );
        assert_eq!(reconcile_operation_id(&headers).unwrap(), first);
        let service = ad_service_operation_id(Some(first), 7, 11, 13).unwrap();
        assert_eq!(
            ad_service_operation_id(Some(first), 7, 11, 13).as_deref(),
            Some(service.as_str())
        );
        assert_ne!(
            ad_service_operation_id(Some(Uuid::new_v4()), 7, 11, 13).as_deref(),
            Some(service.as_str())
        );
        assert_ne!(
            ad_service_operation_id(Some(first), 7, 11, 14).as_deref(),
            Some(service.as_str())
        );
        assert_eq!(ad_service_operation_id(None, 7, 11, 13), None);
    }
}
