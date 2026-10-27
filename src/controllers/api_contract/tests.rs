use crate::services::control_jobs::{ControlJobModel, ControlJobStatus};
use axum::{
    body::to_bytes,
    http::{Request, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use tower::ServiceExt;

#[test]
fn committed_contract_matches_rust_schemas_and_routes() {
    let committed: serde_json::Value = serde_json::from_slice(
        &std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/openapi/v1.json"))
            .expect("generate the OpenAPI contract"),
    )
    .unwrap();
    let generated = serde_json::to_value(super::document()).unwrap();
    assert_eq!(
        committed, generated,
        "run export-openapi and pnpm genapi after changing covered API contracts"
    );
    let operation = &generated["paths"]["/api/edit/jobs/{jobId}"]["get"];
    assert_eq!(
        operation["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/ControlJobModel"
    );
    assert_eq!(operation["security"].as_array().unwrap().len(), 2);
    assert_eq!(
        generated["components"]["securitySchemes"]["session"]["name"],
        "RSCTF_Token"
    );
    assert_eq!(
        generated["components"]["schemas"]["ControlJobStatus"]["enum"],
        serde_json::json!(["Queued", "Running", "Succeeded", "Failed", "Cancelled"])
    );
    assert_eq!(
        generated["components"]["schemas"]["ControlJobModel"]["properties"]["createdAtUtc"]["type"],
        "integer"
    );
}

#[tokio::test]
async fn successful_job_response_is_raw_camel_case_and_millisecond_typed() {
    let at = chrono::DateTime::from_timestamp_millis(1_700_000_000_123).unwrap();
    let job = ControlJobModel {
        id: uuid::Uuid::nil(),
        kind: "BuildBatch".into(),
        scope_key: "game:1".into(),
        game_id: 1,
        challenge_id: None,
        operation_id: uuid::Uuid::nil(),
        fingerprint: "a".repeat(64),
        status: ControlJobStatus::Queued,
        progress_current: 0,
        progress_total: 0,
        requested_generation: 1,
        result: None,
        error: None,
        cancellation_requested: false,
        created_at_utc: at,
        updated_at_utc: at,
        finished_at_utc: None,
    };
    let response = crate::utils::shared::RequestResponse::ok(job).into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 32_768).await.unwrap()).unwrap();
    assert!(value.get("data").is_none());
    assert_eq!(value["status"], "Queued");
    assert_eq!(value["createdAtUtc"], 1_700_000_000_123i64);
    assert_eq!(value["finishedAtUtc"], serde_json::Value::Null);
    assert_eq!(value["challengeId"], serde_json::Value::Null);
    assert!(value.get("created_at_utc").is_none());
}

#[tokio::test]
async fn specification_endpoint_is_json_and_error_contract_stays_numeric() {
    let response = Router::new()
        .route("/openapi/v1.json", get(super::serve))
        .oneshot(
            Request::builder()
                .uri("/openapi/v1.json")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 128_000).await.unwrap()).unwrap();
    assert_eq!(value["openapi"], "3.1.0");
    let response = crate::utils::error::AppError::Unauthorized.into_response();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024).await.unwrap()).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"status":401,"title":"unauthorized"})
    );
}
