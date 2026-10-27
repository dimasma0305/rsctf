//! Incremental Rust-derived API contract. The legacy client covers additional
//! routes; new schema coverage must preserve its manually extended behavior.
use axum::Json;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

#[derive(OpenApi)]
#[openapi(
    info(title = "RSCTF API", version = "1",
        description = "Incremental contract: durable control-job reads and cancellation. Other established routes remain supported by the legacy client."),
    paths(
        crate::controllers::edit::control_jobs::get_control_job,
        crate::controllers::edit::control_jobs::get_control_job_by_operation,
        crate::controllers::edit::control_jobs::cancel_control_job
    ),
    components(schemas(
        crate::services::control_jobs::ControlJobModel,
        crate::services::control_jobs::ControlJobStatus,
        crate::utils::error::ErrorBody
    )),
    modifiers(&Authentication)
)]
struct Contract;

struct Authentication;
impl Modify for Authentication {
    fn modify(&self, document: &mut utoipa::openapi::OpenApi) {
        let components = document.components.as_mut().expect("contract components");
        components.add_security_scheme(
            "session",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
                "RSCTF_Token",
                "Session cookie; unsafe requests also require the platform CSRF policy",
            ))),
        );
        components.add_security_scheme(
            "bearer",
            SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build()),
        );
    }
}

pub fn document() -> utoipa::openapi::OpenApi {
    Contract::openapi()
}

pub(crate) async fn serve() -> Json<utoipa::openapi::OpenApi> {
    Json(document())
}

#[cfg(test)]
mod authorization_tests;
#[cfg(test)]
mod tests;
