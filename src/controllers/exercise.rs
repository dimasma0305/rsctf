//! controllers/exercise.rs — ported from RSCTF `Controllers/ExerciseController.cs`.
//! Standalone per-user practice challenges (no game/team scope).

use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Json, Router};
use chrono::Utc;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::middlewares::rate_limiter::{limited, Policy};
use crate::models::data::{container, exercise_challenge, exercise_instance};
use crate::services::container::ContainerSpec;
use crate::utils::enums::{AnswerResult, ChallengeCategory, ContainerStatus};
use crate::utils::error::{AppError, AppResult};
use crate::utils::flag_generator;
use crate::utils::shared::{ArrayResponse, MessageResponse, RequestResponse};

mod operations;
pub(crate) use operations::sweep as sweep_container_operations;

const DEFAULT_EXERCISE_PAGE_SIZE: u64 = 24;
const MAX_EXERCISE_PAGE_SIZE: u64 = 50;
const MAX_EXERCISE_CATALOG_ROWS: u64 = 500;
const MAX_AUTO_DESTROY_PER_CREATE: usize = 2;
const MAX_TRACKED_EXERCISE_CONTAINERS: usize = 128;
const EXERCISE_OVERLOAD_RETRY_SECONDS: u64 = 2;
const MAX_EXERCISE_SUBMIT_BODY_BYTES: usize = 1_024;

const ELIGIBLE_EXERCISE_FLAG_SQL: &str = r#"SELECT EXISTS (
    SELECT 1
      FROM "FlagContexts" flag
     WHERE flag.exercise_id = $1
       AND flag.flag = $3
       AND (
           (flag.id = $2 AND flag.is_occupied = TRUE)
           OR flag.is_occupied = FALSE
       )
)"#;

async fn eligible_exercise_flag(
    connection: &mut sqlx::PgConnection,
    exercise_id: i32,
    current_flag_id: Option<i32>,
    answer: &str,
) -> AppResult<bool> {
    sqlx::query_scalar::<_, bool>(ELIGIBLE_EXERCISE_FLAG_SQL)
        .bind(exercise_id)
        .bind(current_flag_id)
        .bind(answer)
        .fetch_one(connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/exercise", limited(Policy::Query, get(list)))
        .route(
            "/api/exercise/{id}",
            get(detail).merge(
                limited(Policy::Submit, axum::routing::post(submit))
                    .layer(DefaultBodyLimit::max(MAX_EXERCISE_SUBMIT_BODY_BYTES)),
            ),
        )
        .route(
            "/api/exercise/{id}/container",
            limited(
                Policy::Container,
                axum::routing::post(create_container).delete(destroy_container),
            ),
        )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExerciseBrief {
    pub id: i32,
    pub title: String,
    pub category: ChallengeCategory,
    pub difficulty: i16,
    pub score: i32,
    pub solved: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExerciseListQuery {
    #[serde(default = "default_exercise_page_size")]
    count: u64,
    #[serde(default)]
    skip: u64,
}

fn default_exercise_page_size() -> u64 {
    DEFAULT_EXERCISE_PAGE_SIZE
}

impl ExerciseListQuery {
    fn limit(&self) -> i64 {
        self.count.clamp(1, MAX_EXERCISE_PAGE_SIZE) as i64
    }

    fn offset(&self) -> i64 {
        self.skip.min(MAX_EXERCISE_CATALOG_ROWS) as i64
    }
}

#[derive(sqlx::FromRow)]
struct ExerciseCatalogRow {
    id: Option<i32>,
    title: Option<String>,
    category: Option<i16>,
    difficulty: Option<i16>,
    score: Option<i32>,
    solved: Option<bool>,
    total_count: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExerciseDetail {
    pub id: i32,
    pub title: String,
    pub content: String,
    pub category: ChallengeCategory,
    pub difficulty: i16,
    pub score: i32,
    pub hints: Option<serde_json::Value>,
    pub solved: bool,
    pub entry: Option<String>,
}

#[derive(Deserialize)]
pub struct FlagSubmit {
    pub flag: String,
}

fn validated_exercise_answer(value: &str) -> AppResult<&str> {
    // Preserve the legacy exercise endpoint's public error contract while
    // enforcing the same UTF-8 byte ceiling as normal game submissions.
    if value.len() > crate::controllers::game::MAX_FLAG_LENGTH {
        return Err(AppError::bad_request("Flag is too long"));
    }
    let answer = value.trim();
    crate::utils::flag_policy::validate_normal(answer)
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    Ok(answer)
}

#[derive(sqlx::FromRow)]
struct OwnedExerciseContainer {
    instance_id: i32,
    container_uuid: uuid::Uuid,
    backend_id: String,
    flag_id: Option<i32>,
}

async fn other_owned_containers(
    pool: &sqlx::PgPool,
    user_id: uuid::Uuid,
    exercise_id: i32,
    limit: i64,
) -> AppResult<Vec<OwnedExerciseContainer>> {
    sqlx::query_as::<_, OwnedExerciseContainer>(
        r#"SELECT instance.id AS instance_id,
                  container.id AS container_uuid,
                  container.container_id AS backend_id,
                  instance.flag_id
             FROM "ExerciseInstances" instance
             JOIN "Containers" container ON container.id = instance.container_id
            WHERE instance.user_id = $1
              AND instance.exercise_id <> $2
              AND instance.is_loaded = TRUE
              AND instance.container_id IS NOT NULL
            ORDER BY container.started_at ASC, instance.id ASC
            LIMIT $3"#,
    )
    .bind(user_id)
    .bind(exercise_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

/// `GET /api/exercise` — published, enabled exercises.
pub async fn list(
    State(st): State<SharedState>,
    user: CurrentUser,
    Query(query): Query<ExerciseListQuery>,
) -> AppResult<ArrayResponse<ExerciseBrief>> {
    let rows = sqlx::query_as::<_, ExerciseCatalogRow>(
        r#"WITH bounded AS MATERIALIZED (
                SELECT exercise.id, exercise.title,
                       exercise.category::smallint AS category,
                       exercise.difficulty,
                       exercise.original_score AS score,
                       COALESCE(instance.is_solved, FALSE) AS solved
                  FROM "ExerciseChallenges" exercise
                  LEFT JOIN "ExerciseInstances" instance
                    ON instance.exercise_id = exercise.id
                   AND instance.user_id = $1
                 WHERE exercise.is_enabled = TRUE
                   AND exercise.publish_time_utc <= clock_timestamp()
                 ORDER BY exercise.id
                 LIMIT $4
             ), page AS (
                SELECT * FROM bounded ORDER BY id OFFSET $2 LIMIT $3
             )
             SELECT page.id, page.title, page.category, page.difficulty,
                    page.score, page.solved,
                    (SELECT COUNT(*)::bigint FROM bounded) AS total_count
               FROM (SELECT 1) anchor
               LEFT JOIN page ON TRUE
              ORDER BY page.id NULLS LAST"#,
    )
    .bind(user.id)
    .bind(query.offset())
    .bind(query.limit())
    .bind(MAX_EXERCISE_CATALOG_ROWS as i64)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let total = rows.first().map_or(0, |row| row.total_count);
    let data = rows
        .into_iter()
        .filter_map(|row| {
            Some((
                row.id?,
                row.title?,
                row.category?,
                row.difficulty?,
                row.score?,
                row.solved?,
            ))
        })
        .map(|(id, title, category, difficulty, score, solved)| {
            let category = <ChallengeCategory as sea_orm::ActiveEnum>::try_from_value(&category)
                .map_err(|error| AppError::internal(error.to_string()))?;
            Ok(ExerciseBrief {
                id,
                title,
                category,
                difficulty,
                score,
                solved,
            })
        })
        .collect::<AppResult<Vec<_>>>()?;
    Ok(ArrayResponse::new(data, total))
}

async fn load_exercise(st: &SharedState, id: i32) -> AppResult<exercise_challenge::Model> {
    exercise_challenge::Entity::find()
        .filter(exercise_challenge::Column::Id.eq(id))
        .filter(exercise_challenge::Column::IsEnabled.eq(true))
        .filter(exercise_challenge::Column::PublishTimeUtc.lte(Utc::now()))
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Exercise not found"))
}

async fn user_instance(
    st: &SharedState,
    exercise_id: i32,
    user_id: uuid::Uuid,
) -> AppResult<Option<exercise_instance::Model>> {
    Ok(exercise_instance::Entity::find()
        .filter(exercise_instance::Column::ExerciseId.eq(exercise_id))
        .filter(exercise_instance::Column::UserId.eq(user_id))
        .one(&st.db)
        .await?)
}

async fn clear_exercise_container_owner(
    pool: &sqlx::PgPool,
    instance_id: Option<i32>,
    container_id: uuid::Uuid,
    backend_id: Option<&str>,
    created_flag_id: Option<i32>,
) -> AppResult<()> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some(instance_id) = instance_id {
        sqlx::query(
            r#"UPDATE "ExerciseInstances"
                  SET container_id = NULL,
                      is_loaded = FALSE,
                      flag_id = CASE WHEN flag_id = $3 THEN NULL ELSE flag_id END
                WHERE id = $1 AND container_id = $2"#,
        )
        .bind(instance_id)
        .bind(container_id)
        .bind(created_flag_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    if let Some(backend_id) = backend_id {
        sqlx::query(r#"DELETE FROM "Containers" WHERE id = $1 AND container_id = $2"#)
            .bind(container_id)
            .bind(backend_id)
            .execute(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    if let Some(flag_id) = created_flag_id {
        sqlx::query(
            r#"DELETE FROM "FlagContexts" flag
                WHERE flag.id = $1
                  AND NOT EXISTS (
                      SELECT 1 FROM "ExerciseInstances" instance
                       WHERE instance.flag_id = flag.id
                  )"#,
        )
        .bind(flag_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

async fn destroy_owned_exercise_container_with<F>(
    pool: &sqlx::PgPool,
    instance_id: Option<i32>,
    container_id: uuid::Uuid,
    backend_id: &str,
    created_flag_id: Option<i32>,
    destroy: F,
) -> AppResult<()>
where
    F: std::future::Future<Output = AppResult<()>>,
{
    // Await destruction before opening the cleanup transaction. A failed
    // backend call therefore leaves every durable owner available for retry.
    destroy.await?;
    clear_exercise_container_owner(
        pool,
        instance_id,
        container_id,
        Some(backend_id),
        created_flag_id,
    )
    .await
}

/// `GET /api/exercise/{id}` — exercise detail for the current user.
pub async fn detail(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<ExerciseDetail>> {
    let e = load_exercise(&st, id).await?;
    let inst = user_instance(&st, id, user.id).await?;
    let entry = match &inst {
        Some(i) => match i.container_id {
            Some(cid) => container::Entity::find_by_id(cid)
                .one(&st.db)
                .await?
                .map(|c| c.entry()),
            None => None,
        },
        None => None,
    };
    Ok(RequestResponse::ok(ExerciseDetail {
        id: e.id,
        title: e.title,
        content: e.content,
        category: e.category,
        difficulty: e.difficulty,
        score: e.original_score,
        hints: e.hints,
        solved: inst.map(|i| i.is_solved).unwrap_or(false),
        entry,
    }))
}

/// `POST /api/exercise/{id}` — submit a flag.
pub async fn submit(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    Json(model): Json<FlagSubmit>,
) -> AppResult<RequestResponse<AnswerResult>> {
    let answer = validated_exercise_answer(&model.flag)?;
    let _e = load_exercise(&st, id).await?;

    let lock_key = format!("exercise-submit-user:{}", user.id);
    let Some(mut distributed) =
        crate::utils::single_flight::PgAdvisoryLock::try_acquire_exercise_grading(
            st.pg(),
            &lock_key,
        )
        .await?
    else {
        return Err(AppError::too_many_requests(EXERCISE_OVERLOAD_RETRY_SECONDS));
    };
    let current = sqlx::query_as::<_, (bool, Option<i32>)>(
        r#"SELECT is_solved, flag_id
             FROM "ExerciseInstances"
            WHERE exercise_id = $1 AND user_id = $2
            FOR UPDATE"#,
    )
    .bind(id)
    .bind(user.id)
    .fetch_optional(&mut **distributed.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    if current.is_some_and(|(is_solved, _)| is_solved) {
        distributed.release().await?;
        return Ok(RequestResponse::ok(AnswerResult::Accepted));
    }

    // Only the caller's current occupied flag and author-defined unoccupied
    // static flags are eligible. Other users' and stale instance flags share
    // the exercise id, so exercise-id-only fallback would cross that boundary.
    let accepted = eligible_exercise_flag(
        distributed.transaction_mut(),
        id,
        current.and_then(|(_, flag_id)| flag_id),
        answer,
    )
    .await?;

    let result = if accepted {
        AnswerResult::Accepted
    } else {
        AnswerResult::WrongAnswer
    };

    if accepted {
        sqlx::query(
            r#"INSERT INTO "ExerciseInstances"
                    (exercise_id, user_id, is_loaded, is_solved, flag_id,
                     container_id, last_container_operation)
               VALUES ($1, $2, FALSE, TRUE, NULL, NULL, clock_timestamp())
               ON CONFLICT (user_id, exercise_id) DO UPDATE
                   SET is_solved = TRUE
                 WHERE "ExerciseInstances".is_solved = FALSE"#,
        )
        .bind(id)
        .bind(user.id)
        .execute(&mut **distributed.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }

    distributed.release().await?;
    Ok(RequestResponse::ok(result))
}

/// `POST /api/exercise/{id}/container` — provision a per-user practice container.
pub async fn create_container(
    State(st): State<SharedState>,
    user: CurrentUser,
    headers: HeaderMap,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<String>> {
    // Reject hidden/nonexistent definitions before reserving durable work.
    let _ = load_exercise(&st, id).await?;
    let request = operations::operation_request(&headers)?;
    let operation_id = request.operation_id;
    let expected_container_id = user_instance(&st, id, user.id)
        .await?
        .and_then(|instance| instance.container_id);
    let operation = match operations::claim_create(
        st.pg(),
        operation_id,
        user.id,
        id,
        expected_container_id,
        request.may_adopt_stale,
    )
    .await?
    {
        operations::ClaimOutcome::Recovered(entry) => return Ok(RequestResponse::ok(entry)),
        operations::ClaimOutcome::Following => {
            let entry = operations::wait_for_result(st.pg(), operation_id).await?;
            return Ok(RequestResponse::ok(entry));
        }
        operations::ClaimOutcome::Owned(operation) => operation,
    };
    let owner_st = st.clone();
    let owner_user = user.clone();
    let owner_operation = operation.clone();
    let owner = operations::spawn_owner(st.pg().clone(), operation, async move {
        perform_create_container(owner_st, owner_user, id, owner_operation).await
    });
    Ok(RequestResponse::ok(operations::await_owner(owner).await?))
}

async fn perform_create_container(
    st: SharedState,
    user: CurrentUser,
    id: i32,
    operation: operations::ClaimedOperation,
) -> AppResult<String> {
    let e = load_exercise(&st, id).await?;
    if !e.challenge_type.is_container() {
        return Err(AppError::bad_request("Exercise has no container"));
    }
    let configured_image = e
        .container_image
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| AppError::bad_request("Exercise has no image configured"))?;
    // Exercises are standalone legacy records and have no reviewed build/pull
    // workflow or `build_image_digest` column. Their configured reference must
    // therefore already be immutable before it can cross the runtime boundary.
    let runtime_backend = if crate::services::challenge_workloads::uses_worker_runtime_for_type(
        &st,
        e.challenge_type,
    ) {
        crate::services::container::ContainerBackendKind::Worker
    } else {
        st.containers.backend_kind()
    };
    let image = crate::services::challenge_images::validate_runtime_reference(
        configured_image,
        runtime_backend,
        st.config.runtime_role,
        runtime_backend != crate::services::container::ContainerBackendKind::Worker
            && crate::services::challenge_images::shared_docker_daemon_acknowledged(),
    )?;
    let flag = flag_generator::generate_retryable_flag_checked(
        e.flag_template.as_deref(),
        &flag_generator::exercise_user_hash(st.config.identity_hash_key.as_bytes(), id, user.id),
        &operation.operation_id.to_string(),
    )?;

    // The durable active-user operation owns this lifecycle across replicas.
    // No pooled connection is retained while Docker or a worker is inspected.
    let mut existing = user_instance(&st, id, user.id).await?;
    if let Some(instance) = existing.as_mut() {
        if let Some(container_id) = instance.container_id {
            if let Some(current) = container::Entity::find_by_id(container_id)
                .one(&st.db)
                .await?
            {
                if current.image == image
                    && current.status == ContainerStatus::Running
                    && st.containers.is_running(&current.container_id).await
                {
                    return Ok(current.entry());
                }
                destroy_owned_exercise_container_with(
                    st.pg(),
                    Some(instance.id),
                    container_id,
                    &current.container_id,
                    instance.flag_id,
                    crate::services::traffic::destroy_container_after_capture_fence(
                        &st,
                        &current.container_id,
                    ),
                )
                .await?;
            } else {
                clear_exercise_container_owner(
                    st.pg(),
                    Some(instance.id),
                    container_id,
                    None,
                    instance.flag_id,
                )
                .await?;
            }
            instance.container_id = None;
            instance.is_loaded = false;
        }
    }

    let container_policy =
        crate::services::container_policy::ContainerPolicy::load(st.pg()).await?;
    let maximum = usize::try_from(container_policy.max_exercise_container_count_per_user)
        .map_err(|_| AppError::internal("invalid exercise container limit"))?;
    if maximum == 0 || maximum > MAX_TRACKED_EXERCISE_CONTAINERS {
        return Err(AppError::internal("invalid exercise container limit"));
    }
    let inventory_limit = maximum
        .saturating_add(MAX_AUTO_DESTROY_PER_CREATE)
        .saturating_add(1)
        .min(MAX_TRACKED_EXERCISE_CONTAINERS) as i64;
    let owned = other_owned_containers(st.pg(), user.id, id, inventory_limit).await?;
    if owned.len() >= maximum {
        if !container_policy.auto_destroy_on_limit_reached {
            return Err(AppError::bad_request(format!(
                "The number of exercise containers cannot exceed {}",
                container_policy.max_exercise_container_count_per_user
            )));
        }
        let remove_count = owned.len() - maximum + 1;
        if remove_count > MAX_AUTO_DESTROY_PER_CREATE {
            return Err(AppError::too_many_requests(EXERCISE_OVERLOAD_RETRY_SECONDS));
        }
        for old in owned.into_iter().take(remove_count) {
            destroy_owned_exercise_container_with(
                st.pg(),
                Some(old.instance_id),
                old.container_uuid,
                &old.backend_id,
                old.flag_id,
                crate::services::traffic::destroy_container_after_capture_fence(
                    &st,
                    &old.backend_id,
                ),
            )
            .await?;
        }
    }

    let game_kind = crate::services::container::game_kind_for_challenge(e.challenge_type);
    let platform_proxy =
        crate::controllers::admin::container_port_mapping(&st).await == "PlatformProxy";
    let is_proxy = crate::services::container::should_use_platform_proxy(
        game_kind,
        st.containers.requires_proxy(),
        platform_proxy,
        false,
    );
    let cuuid = operation.publication_id;
    operations::mark_runtime_started(st.pg(), &operation).await?;
    let info = st
        .containers
        .create(ContainerSpec {
            game_kind,
            image: image.clone(),
            memory_limit: e.memory_limit.unwrap_or(64),
            cpu_count: e.cpu_count.unwrap_or(1),
            storage_limit: crate::services::container::DEFAULT_CONTAINER_STORAGE_MB,
            expose_port: e.expose_port.unwrap_or(80),
            publish_port: true,
            proxy_only: is_proxy,
            env: vec![],
            flag: Some(flag.clone()),
            ad_network: None,
            allow_egress: true,
            control_plane_callback_ports: Vec::new(),
            network_mode: crate::utils::enums::NetworkMode::Open,
            operation_id: Some(format!("exercise-container:{}", operation.operation_id)),
        })
        .await?;
    operations::record_backend(st.pg(), &operation, &info.id).await?;

    let backend_id = info.id.clone();
    let mut created_flag_id = None;
    let mut linked_exercise_instance_id = None;
    let mut publication_outcome_ambiguous = false;
    let persisted: AppResult<String> = async {
        let now = Utc::now();
        let stop_at = now + chrono::Duration::minutes(i64::from(container_policy.default_lifetime));
        let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        let flag_id = sqlx::query_scalar::<_, i32>(
            r#"INSERT INTO "FlagContexts"
                   (flag, is_occupied, attachment_id, challenge_id, exercise_id)
               VALUES ($1, TRUE, NULL, NULL, $2)
               RETURNING id"#,
        )
        .bind(&flag)
        .bind(id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        created_flag_id = Some(flag_id);

        sqlx::query(
            r#"INSERT INTO "Containers"
                   (id, image, container_id, status, started_at, expect_stop_at,
                    is_proxy, ip, port, public_ip, public_port, game_instance_id,
                    exercise_instance_id, ad_team_service_id)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9,
                       NULL, NULL, NULL, NULL, NULL)"#,
        )
        .bind(cuuid)
        .bind(&image)
        .bind(&backend_id)
        .bind(ContainerStatus::Running as i16)
        .bind(now)
        .bind(stop_at)
        .bind(is_proxy)
        .bind(&info.ip)
        .bind(info.port)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

        let exercise_instance_id = sqlx::query_scalar::<_, i32>(
            r#"INSERT INTO "ExerciseInstances"
                    (exercise_id, user_id, is_loaded, is_solved, flag_id,
                     container_id, last_container_operation)
               VALUES ($1, $2, TRUE, FALSE, $3, $4, $5)
               ON CONFLICT (user_id, exercise_id) DO UPDATE
                   SET is_loaded = TRUE,
                       flag_id = EXCLUDED.flag_id,
                       container_id = EXCLUDED.container_id,
                       last_container_operation = EXCLUDED.last_container_operation
               RETURNING id"#,
        )
        .bind(id)
        .bind(user.id)
        .bind(flag_id)
        .bind(cuuid)
        .bind(now)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        linked_exercise_instance_id = Some(exercise_instance_id);

        // Persist both sides of the ownership relation. Existing deployments
        // historically populated only ExerciseInstances.container_id; the
        // proxy supports that legacy shape, while every new container gets the
        // explicit forward identity used for fail-closed authorization.
        let linked = sqlx::query(
            r#"UPDATE "Containers" container
                  SET exercise_instance_id = $2
                WHERE container.id = $1
                  AND container.game_instance_id IS NULL
                  AND (
                      container.exercise_instance_id IS NULL
                      OR container.exercise_instance_id = $2
                  )
                  AND EXISTS (
                      SELECT 1
                        FROM "ExerciseInstances" instance
                       WHERE instance.id = $2
                         AND instance.exercise_id = $3
                         AND instance.user_id = $4
                         AND instance.container_id = container.id
                         AND instance.is_loaded = TRUE
                  )"#,
        )
        .bind(cuuid)
        .bind(exercise_instance_id)
        .bind(id)
        .bind(user.id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if linked.rows_affected() != 1 {
            return Err(AppError::internal(
                "exercise container ownership link changed during provisioning",
            ));
        }

        let entry = if is_proxy {
            cuuid.to_string()
        } else {
            format!("{}:{}", info.ip, info.port)
        };
        if let Err(commit_error) = transaction.commit().await {
            let published = sqlx::query_scalar::<_, bool>(
                r#"SELECT EXISTS(
                       SELECT 1
                         FROM "Containers" container
                         JOIN "ExerciseInstances" instance
                           ON instance.id = container.exercise_instance_id
                          AND instance.container_id = container.id
                        WHERE container.id = $1
                          AND container.container_id = $2
                          AND container.status = $3
                          AND instance.id = $4
                          AND instance.user_id = $5
                          AND instance.exercise_id = $6
                          AND instance.flag_id = $7
                          AND instance.is_loaded = TRUE
                   )"#,
            )
            .bind(cuuid)
            .bind(&backend_id)
            .bind(ContainerStatus::Running as i16)
            .bind(exercise_instance_id)
            .bind(user.id)
            .bind(id)
            .bind(flag_id)
            .fetch_one(st.pg())
            .await;
            match published {
                Ok(true) => {
                    tracing::warn!(
                        %cuuid,
                        %commit_error,
                        "recovered ambiguous exercise publication commit"
                    );
                }
                Ok(false) => return Err(AppError::internal(commit_error.to_string())),
                Err(recovery_error) => {
                    publication_outcome_ambiguous = true;
                    return Err(AppError::internal(format!(
                        "exercise publication outcome is unavailable; retry the same operation ID: {commit_error}; recovery query failed: {recovery_error}"
                    )));
                }
            }
        }
        Ok(entry)
    }
    .await;

    let entry = match persisted {
        Ok(entry) => entry,
        Err(err) if publication_outcome_ambiguous => return Err(err),
        Err(err) => {
            if let Err(destroy_error) = destroy_owned_exercise_container_with(
                st.pg(),
                linked_exercise_instance_id,
                cuuid,
                &backend_id,
                created_flag_id,
                crate::services::traffic::destroy_container_after_capture_fence(&st, &backend_id),
            )
            .await
            {
                tracing::error!(
                    %backend_id,
                    %destroy_error,
                    "exercise publication rollback failed; retaining durable owner for retry"
                );
                return Err(AppError::internal(format!(
                    "{err}; exercise rollback failed: {destroy_error}"
                )));
            }
            return Err(err);
        }
    };

    Ok(entry)
}

/// `DELETE /api/exercise/{id}/container` — tear down the user's container.
pub async fn destroy_container(
    State(st): State<SharedState>,
    user: CurrentUser,
    headers: HeaderMap,
    Path(id): Path<i32>,
) -> AppResult<MessageResponse> {
    let request = operations::operation_request(&headers)?;
    let operation_id = request.operation_id;
    let expected_container_id = user_instance(&st, id, user.id)
        .await?
        .and_then(|instance| instance.container_id);
    let operation = match operations::claim_delete(
        st.pg(),
        operation_id,
        user.id,
        id,
        expected_container_id,
        request.may_adopt_stale,
    )
    .await?
    {
        operations::ClaimOutcome::Recovered(()) => {
            return Ok(MessageResponse::ok("Container destroyed"));
        }
        operations::ClaimOutcome::Following => {
            operations::wait_for_result::<()>(st.pg(), operation_id).await?;
            return Ok(MessageResponse::ok("Container destroyed"));
        }
        operations::ClaimOutcome::Owned(operation) => operation,
    };
    let owner_st = st.clone();
    let owner_user = user.clone();
    let owner_operation = operation.clone();
    let owner = operations::spawn_owner(st.pg().clone(), operation, async move {
        perform_destroy_container(owner_st, owner_user, id, owner_operation.publication_id).await
    });
    operations::await_owner(owner).await?;
    Ok(MessageResponse::ok("Container destroyed"))
}

async fn perform_destroy_container(
    st: SharedState,
    user: CurrentUser,
    id: i32,
    expected_container_id: uuid::Uuid,
) -> AppResult<()> {
    let Some(inst) = user_instance(&st, id, user.id).await? else {
        // An exact retry after a committed-but-lost response is success.
        return Ok(());
    };
    if let Some(cuuid) = inst.container_id {
        if cuuid != expected_container_id {
            return Err(AppError::conflict(
                "The exercise instance changed; refresh before deleting it",
            ));
        }
        if let Some(c) = container::Entity::find_by_id(cuuid).one(&st.db).await? {
            destroy_owned_exercise_container_with(
                st.pg(),
                Some(inst.id),
                cuuid,
                &c.container_id,
                inst.flag_id,
                crate::services::traffic::destroy_container_after_capture_fence(
                    &st,
                    &c.container_id,
                ),
            )
            .await?;
        } else {
            clear_exercise_container_owner(st.pg(), Some(inst.id), cuuid, None, inst.flag_id)
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "exercise/tests.rs"]
mod tests;
