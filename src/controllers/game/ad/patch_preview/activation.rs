use super::*;
use crate::services::control_jobs::{self, ControlJobKind, ControlJobModel};
use crate::utils::shared::RequestResponse;
use axum::http::{HeaderMap, StatusCode};
use uuid::Uuid;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ActivateBuild {
    service: i32,
    attempt: Uuid,
}

pub(crate) async fn activate(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game, challenge, revision)): Path<(i32, i32, i32)>,
    headers: HeaderMap,
    Json(request): Json<ActivateBuild>,
) -> AppResult<(StatusCode, RequestResponse<ControlJobModel>)> {
    let who = caller(&st, &user, game).await?;
    let (mut roster, service, starter) = lock_source(st.pg(), who, challenge).await?;
    starter.ok_or_else(missing)?;
    if request.service != service {
        return Err(AppError::not_found("Private revision not found"));
    }
    let operation = crate::controllers::edit::control_jobs::operation_id(&headers)?;
    let scope = format!("ad-service:{service}");
    let mut input = serde_json::json!({
        "serviceId": service, "participationId": who.participation_id, "playerPolicy": true,
        "candidateRevision": revision, "candidateAttempt": request.attempt,
    });
    // The immutable request survives a worker publishing the image or a teammate
    // building another candidate before the browser retries its lost response.
    let fingerprint = crate::controllers::edit::control_jobs::fingerprint(&input)?;
    let target = sqlx::query_as::<_, (String, String, Option<String>)>(
        r#"SELECT container_id,registry_image_ref,runtime_profile FROM "AdPatchActivationTargets"
           WHERE service_id=$1 AND revision=$2 AND attempt=$3"#,
    )
    .bind(service)
    .bind(revision)
    .bind(request.attempt)
    .fetch_optional(&mut **roster.transaction_mut())
    .await
    .map_err(revisions::db_error)?;
    roster.release().await?;
    let Some((backend, image, profile)) = target else {
        if let Some(job) = control_jobs::get_ad_reset_for_participation(
            st.pg(),
            game,
            who.participation_id,
            None,
            Some(operation),
        )
        .await?
        {
            if job.kind == ControlJobKind::AdActivate.as_str()
                && job.scope_key == scope
                && job.fingerprint == fingerprint
            {
                return Ok((StatusCode::ACCEPTED, RequestResponse::ok(job)));
            }
        }
        return Err(AppError::conflict(
            "This service cannot activate the selected build",
        ));
    };
    if !crate::services::ad::reset::supports_patch_activation(&st, profile.as_deref()) {
        return Err(AppError::conflict(
            "This runtime cannot deploy hosted patch images",
        ));
    }
    input["expectedBackendId"] = backend.into();
    input["candidateImageRef"] = image.into();
    input["candidateRuntimeProfile"] = profile.into();
    let job = control_jobs::enqueue(
        st.pg(),
        ControlJobKind::AdActivate,
        &scope,
        game,
        Some(challenge),
        operation,
        &fingerprint,
        input,
    )
    .await?;
    control_jobs::kick(st);
    Ok((StatusCode::ACCEPTED, RequestResponse::ok(job)))
}
