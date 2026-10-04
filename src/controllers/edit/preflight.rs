//! Operator image preflight: a durable, replica-safe control-plane job that
//! pre-pulls and smoke-starts every enabled, approved challenge image.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Serialize;

use super::{manager_or_admin, CurrentUser, SharedState};
use crate::services::control_jobs::{ControlJobKind, ControlJobModel};
use crate::services::image_preflight::{self, ImagePreflightResultModel, PreflightSummary};
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImagePreflightModel {
    /// Latest preflight job for the game, in any state.
    pub job: Option<ControlJobModel>,
    /// Totals and capacity comparison; present once the job succeeded.
    pub summary: Option<PreflightSummary>,
    pub results: Vec<ImagePreflightResultModel>,
}

async fn ensure_game_exists(pool: &sqlx::PgPool, game_id: i32) -> AppResult<()> {
    let exists: bool = sqlx::query_scalar(r#"SELECT EXISTS(SELECT 1 FROM "Games" WHERE id = $1)"#)
        .bind(game_id)
        .fetch_one(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if exists {
        Ok(())
    } else {
        Err(AppError::not_found("Game not found"))
    }
}

/// Enqueue one preflight per game. An exact operation replay returns the same
/// job; a start whose participating challenge set differs from the active
/// job's is rejected with 409 until that job finishes.
pub async fn start_image_preflight(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
    headers: HeaderMap,
) -> AppResult<(StatusCode, RequestResponse<ControlJobModel>)> {
    manager_or_admin(&st, &user, game_id).await?;
    let operation_id = super::control_jobs::operation_id(&headers)?;
    ensure_game_exists(st.pg(), game_id).await?;
    let plan = image_preflight::plan(&st, game_id).await?;
    let job = crate::services::control_jobs::enqueue(
        st.pg(),
        ControlJobKind::ImagePreflight,
        &image_preflight::scope_key(game_id),
        game_id,
        None,
        operation_id,
        &plan.fingerprint,
        serde_json::json!({ "gameId": game_id, "challenges": plan.targets.len() }),
    )
    .await?;
    crate::services::control_jobs::kick(st);
    Ok((StatusCode::ACCEPTED, RequestResponse::ok(job)))
}

fn summary_of(job: &ControlJobModel) -> Option<PreflightSummary> {
    job.result
        .clone()
        .and_then(|result| serde_json::from_value(result).ok())
}

pub async fn get_image_preflight(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
) -> AppResult<RequestResponse<ImagePreflightModel>> {
    manager_or_admin(&st, &user, game_id).await?;
    ensure_game_exists(st.pg(), game_id).await?;
    let job = crate::services::control_jobs::latest_for_game(
        st.pg(),
        ControlJobKind::ImagePreflight,
        game_id,
    )
    .await?;
    let (summary, results) = match &job {
        Some(job) => (
            summary_of(job),
            image_preflight::load_results(st.pg(), job.id).await?,
        ),
        None => (None, Vec::new()),
    };
    Ok(RequestResponse::ok(ImagePreflightModel {
        job,
        summary,
        results,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::control_jobs::ControlJobStatus;
    use uuid::Uuid;

    fn job(result: Option<serde_json::Value>) -> ControlJobModel {
        ControlJobModel {
            id: Uuid::new_v4(),
            kind: "ImagePreflight".into(),
            scope_key: image_preflight::scope_key(7),
            game_id: 7,
            challenge_id: None,
            operation_id: Uuid::new_v4(),
            fingerprint: "f".repeat(64),
            status: ControlJobStatus::Succeeded,
            progress_current: 1,
            progress_total: 1,
            requested_generation: 1,
            result,
            error: None,
            cancellation_requested: false,
            created_at_utc: chrono::Utc::now(),
            updated_at_utc: chrono::Utc::now(),
            finished_at_utc: Some(chrono::Utc::now()),
        }
    }

    #[test]
    fn wire_model_is_camel_case_with_millisecond_times_and_tolerates_foreign_results() {
        let summary = PreflightSummary {
            challenges: 2,
            images: 1,
            succeeded: 2,
            ..PreflightSummary::default()
        };
        let model = ImagePreflightModel {
            job: Some(job(Some(serde_json::to_value(&summary).unwrap()))),
            summary: summary_of(&job(Some(serde_json::to_value(&summary).unwrap()))),
            results: vec![ImagePreflightResultModel {
                challenge_id: 1,
                challenge_title: "web".into(),
                image: "registry.example/ctf/web@sha256:abc".into(),
                backend: "Docker".into(),
                pull_status: "Succeeded".into(),
                start_status: "Failed".into(),
                duration_ms: 1500,
                error: Some("exited".into()),
                updated_at_utc: chrono::Utc::now(),
            }],
        };
        let wire = serde_json::to_value(&model).unwrap();
        assert_eq!(
            wire["summary"]["capacity"]["available"],
            serde_json::Value::Null
        );
        assert_eq!(wire["summary"]["capacity"]["requested"]["cpuMillis"], 0);
        assert_eq!(wire["results"][0]["challengeTitle"], "web");
        assert_eq!(wire["results"][0]["pullStatus"], "Succeeded");
        assert!(wire["results"][0]["updatedAtUtc"].is_u64());
        assert!(wire["job"]["finishedAtUtc"].is_u64());
        assert_eq!(wire["job"]["status"], "Succeeded");
        assert!(summary_of(&job(Some(serde_json::json!({ "launched": 3 })))).is_none());
        assert!(summary_of(&job(None)).is_none());
    }

    #[test]
    fn preflight_routes_gate_on_game_management_before_any_read() {
        let source = include_str!("preflight.rs");
        let start = source
            .split_once("pub async fn start_image_preflight(")
            .unwrap()
            .1
            .split_once("fn summary_of(")
            .unwrap()
            .0;
        let auth = start
            .find("manager_or_admin(&st, &user, game_id).await?")
            .unwrap();
        assert!(auth < start.find("image_preflight::plan").unwrap());
        assert!(auth < start.find("ensure_game_exists").unwrap());
        let get = source
            .split_once("pub async fn get_image_preflight(")
            .unwrap()
            .1;
        let auth = get
            .find("manager_or_admin(&st, &user, game_id).await?")
            .unwrap();
        assert!(auth < get.find("latest_for_game").unwrap());
    }

    async fn issue_user(
        pool: &sqlx::PgPool,
        state: &SharedState,
        role: crate::utils::enums::Role,
    ) -> (Uuid, String) {
        use sea_orm::ActiveEnum;
        let id = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "AspNetUsers" (id, role, user_name, security_stamp) VALUES ($1, $2, 't', 'stamp')"#,
        )
        .bind(id)
        .bind(role.into_value())
        .execute(pool)
        .await
        .unwrap();
        (id, state.token.issue(id, role, "t", "stamp").unwrap())
    }

    async fn send(
        state: &SharedState,
        method: &str,
        path: &str,
        token: Option<&str>,
        operation: Option<Uuid>,
    ) -> (u16, serde_json::Value) {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;
        // Each call is its own client: the route's concurrency policy must
        // not turn a second request from one address into a 429 here.
        static NEXT_CLIENT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(1);
        let client = NEXT_CLIENT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let peer = std::net::SocketAddr::from(([10, 200, (client >> 8) as u8, client as u8], 4000));
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .extension(axum::extract::ConnectInfo(peer));
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        if let Some(operation) = operation {
            request = request.header("Idempotency-Key", operation.to_string());
        }
        let response = crate::controllers::edit::router()
            .with_state(state.clone())
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap_or_default(),
        )
    }

    #[tokio::test]
    #[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn preflight_authorization_replay_and_conflict_boundaries() {
        use crate::{
            app_state::AppState,
            models::internal::configs::AppConfig,
            services::{
                cache::InMemoryCache, container::NoopContainerManager, token::TokenService,
            },
            storage::LocalBlobStorage,
            utils::enums::Role,
        };
        use std::sync::Arc;

        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&std::env::var("RSCTF_TEST_DATABASE_URL").unwrap())
            .await
            .unwrap();
        // One dedicated connection: temporary tables shadow every real table.
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "Games" (id integer primary key);
            CREATE TEMP TABLE "Participations" (id serial primary key, game_id integer, status smallint);
            CREATE TEMP TABLE "GameChallenges" (
                id integer primary key, game_id integer, "Type" smallint, category smallint,
                review_status smallint, build_status smallint, score_curve smallint,
                network_mode smallint, variant_mode smallint,
                variant_generator_build_status smallint, solve_receipt_mode smallint,
                title text
            );
            CREATE TEMP TABLE "GameManagers" (id serial primary key, game_id integer, user_id uuid);
            CREATE TEMP TABLE "AspNetUsers" (id uuid primary key, role smallint, user_name text, security_stamp text);
            CREATE TEMP TABLE "Configs" (config_key text primary key, value text, cache_keys jsonb);
            INSERT INTO "Games" VALUES (1),(2);
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        for migration in [
            crate::migrations::m0261_control_plane_jobs::UP_SQL,
            crate::migrations::m0263_control_job_cancellation::UP_SQL,
            crate::migrations::m0349_image_preflight_results::UP_SQL,
        ] {
            sqlx::raw_sql(&migration.replace(
                "CREATE TABLE IF NOT EXISTS",
                "CREATE TEMP TABLE IF NOT EXISTS",
            ))
            .execute(&pool)
            .await
            .unwrap();
        }
        let state = AppState::new(
            sea_orm::SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone()),
            Arc::new(AppConfig::default()),
            Arc::new(InMemoryCache::new()),
            Arc::new(LocalBlobStorage::new(
                std::env::temp_dir().join("rsctf-image-preflight-no-blobs"),
            )),
            TokenService::new("test-image-preflight-key-not-a-secret", 60),
            Arc::new(NoopContainerManager),
        );
        const START: &str = "/api/edit/games/1/preflight";

        // Anonymous, player, monitor: no start, no read.
        assert_eq!(
            send(&state, "POST", START, None, Some(Uuid::new_v4()))
                .await
                .0,
            401
        );
        assert_eq!(send(&state, "GET", START, None, None).await.0, 401);
        for role in [Role::User, Role::Monitor] {
            let (_, token) = issue_user(&pool, &state, role).await;
            let (status, _) = send(&state, "POST", START, Some(&token), Some(Uuid::new_v4())).await;
            assert_eq!(status, 403, "{role:?} start");
            assert_eq!(
                send(&state, "GET", START, Some(&token), None).await.0,
                403,
                "{role:?} read"
            );
        }
        // An organizer of game 2 cannot read or start game 1's preflight.
        let (other_id, other_token) = issue_user(&pool, &state, Role::User).await;
        sqlx::query(r#"INSERT INTO "GameManagers" (game_id, user_id) VALUES (2, $1)"#)
            .bind(other_id)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            send(&state, "GET", START, Some(&other_token), None).await.0,
            403
        );
        assert_eq!(
            send(
                &state,
                "POST",
                START,
                Some(&other_token),
                Some(Uuid::new_v4())
            )
            .await
            .0,
            403
        );
        let (status, own) = send(
            &state,
            "GET",
            "/api/edit/games/2/preflight",
            Some(&other_token),
            None,
        )
        .await;
        assert_eq!(status, 200, "{own}");
        assert!(own["job"].is_null());

        // Admin: missing operation id is a 400, unknown game a 404; a start is
        // accepted, its exact replay returns the same job, and the empty plan
        // runs to a successful summary off the request path.
        let (_, admin) = issue_user(&pool, &state, Role::Admin).await;
        assert_eq!(send(&state, "POST", START, Some(&admin), None).await.0, 400);
        assert_eq!(
            send(
                &state,
                "GET",
                "/api/edit/games/9/preflight",
                Some(&admin),
                None
            )
            .await
            .0,
            404
        );
        let operation = Uuid::new_v4();
        let (status, first) = send(&state, "POST", START, Some(&admin), Some(operation)).await;
        assert_eq!(status, 202, "{first}");
        assert_eq!(first["status"], "Queued");
        let (status, replay) = send(&state, "POST", START, Some(&admin), Some(operation)).await;
        assert_eq!(status, 202);
        assert_eq!(first["id"], replay["id"]);
        let job_id = Uuid::parse_str(first["id"].as_str().unwrap()).unwrap();
        let terminal = crate::services::control_jobs::wait_for_terminal(
            &pool,
            job_id,
            std::time::Duration::from_secs(20),
        )
        .await
        .unwrap();
        assert_eq!(
            terminal.status,
            ControlJobStatus::Succeeded,
            "{:?}",
            terminal.error
        );
        let (status, read) = send(&state, "GET", START, Some(&admin), None).await;
        assert_eq!(status, 200, "{read}");
        assert_eq!(read["job"]["id"], first["id"]);
        assert_eq!(read["results"], serde_json::json!([]));
        assert_eq!(read["summary"]["challenges"], 0);
        assert!(read["summary"]["capacity"]["available"].is_null());

        // While a preflight is active, a start for a different challenge set
        // (another fingerprint) is a 409; the same set coalesces onto it.
        let active_id = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "ControlPlaneJobs"
                  (id, kind, scope_key, game_id, operation_id, fingerprint, status)
               VALUES ($1, 'ImagePreflight', $2, 1, $3, $4, 1)"#,
        )
        .bind(active_id)
        .bind(image_preflight::scope_key(1))
        .bind(Uuid::new_v4())
        .bind("e".repeat(64))
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            send(&state, "POST", START, Some(&admin), Some(Uuid::new_v4()))
                .await
                .0,
            409
        );
        sqlx::query(r#"UPDATE "ControlPlaneJobs" SET fingerprint = $1 WHERE id = $2"#)
            .bind(first["fingerprint"].as_str().unwrap())
            .bind(active_id)
            .execute(&pool)
            .await
            .unwrap();
        let (status, coalesced) =
            send(&state, "POST", START, Some(&admin), Some(Uuid::new_v4())).await;
        assert_eq!(status, 202);
        assert_eq!(coalesced["id"], active_id.to_string());
        let (_, latest) = send(&state, "GET", START, Some(&admin), None).await;
        assert_eq!(latest["job"]["id"], active_id.to_string());
        let active: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*) FROM "ControlPlaneJobs" WHERE kind = 'ImagePreflight' AND status IN (0, 1)"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(active, 1);
        pool.close().await;
    }
}
