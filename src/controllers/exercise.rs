//! controllers/exercise.rs — ported from RSCTF `Controllers/ExerciseController.cs`.
//! Standalone per-user practice challenges (no game/team scope).

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use serde::{Deserialize, Serialize};

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::models::data::{container, exercise_challenge, exercise_instance, flag_context};
use crate::services::container::ContainerSpec;
use crate::utils::crypto_utils::ct_eq;
use crate::utils::enums::{AnswerResult, ChallengeCategory, ContainerStatus};
use crate::utils::error::{AppError, AppResult};
use crate::utils::flag_generator;
use crate::utils::shared::{ArrayResponse, MessageResponse, RequestResponse};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/exercise", get(list))
        .route("/api/exercise/{id}", get(detail).post(submit))
        .route(
            "/api/exercise/{id}/container",
            axum::routing::post(create_container).delete(destroy_container),
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

fn instance_lock_key(user_id: uuid::Uuid, exercise_id: i32) -> String {
    format!("exercise-container:{user_id}:{exercise_id}")
}

async fn solved_ids(
    st: &SharedState,
    user_id: uuid::Uuid,
) -> AppResult<std::collections::HashSet<i32>> {
    let insts = exercise_instance::Entity::find()
        .filter(exercise_instance::Column::UserId.eq(user_id))
        .filter(exercise_instance::Column::IsSolved.eq(true))
        .all(&st.db)
        .await?;
    Ok(insts.into_iter().map(|i| i.exercise_id).collect())
}

/// `GET /api/exercise` — published, enabled exercises.
pub async fn list(
    State(st): State<SharedState>,
    user: CurrentUser,
) -> AppResult<ArrayResponse<ExerciseBrief>> {
    let now = Utc::now();
    let solved = solved_ids(&st, user.id).await?;
    let items = exercise_challenge::Entity::find()
        .filter(exercise_challenge::Column::IsEnabled.eq(true))
        .filter(exercise_challenge::Column::PublishTimeUtc.lte(now))
        .order_by_asc(exercise_challenge::Column::Id)
        .all(&st.db)
        .await?;
    let total = items.len() as i64;
    let data = items
        .into_iter()
        .map(|e| ExerciseBrief {
            solved: solved.contains(&e.id),
            id: e.id,
            title: e.title,
            category: e.category,
            difficulty: e.difficulty,
            score: e.original_score,
        })
        .collect();
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
    backend_id: &str,
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
    sqlx::query(r#"DELETE FROM "Containers" WHERE id = $1 AND container_id = $2"#)
        .bind(container_id)
        .bind(backend_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
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
    clear_exercise_container_owner(pool, instance_id, container_id, backend_id, created_flag_id)
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
    let _e = load_exercise(&st, id).await?;
    let answer = model.flag.trim().to_string();
    if answer.is_empty() {
        return Err(AppError::bad_request("A flag is required"));
    }

    let lock_key = instance_lock_key(user.id, id);
    let _instance_guard = crate::utils::single_flight::coalesce(&lock_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire(st.pg(), &lock_key).await?;
    let inst = user_instance(&st, id, user.id).await?;

    // Dynamic (per-instance) flag first, else any static flag for the exercise.
    let mut accepted = false;
    if let Some(i) = &inst {
        if let Some(fid) = i.flag_id {
            if let Some(fc) = flag_context::Entity::find_by_id(fid).one(&st.db).await? {
                accepted = ct_eq(&fc.flag, &answer);
            }
        }
    }
    if !accepted {
        let statics = flag_context::Entity::find()
            .filter(flag_context::Column::ExerciseId.eq(id))
            .all(&st.db)
            .await?;
        accepted = statics.iter().any(|f| ct_eq(&f.flag, &answer));
    }

    let result = if accepted {
        AnswerResult::Accepted
    } else {
        AnswerResult::WrongAnswer
    };

    if accepted {
        match inst {
            Some(i) => {
                let mut am: exercise_instance::ActiveModel = i.into();
                am.is_solved = Set(true);
                am.update(&st.db).await?;
            }
            None => {
                exercise_instance::ActiveModel {
                    exercise_id: Set(id),
                    user_id: Set(user.id),
                    is_loaded: Set(false),
                    is_solved: Set(true),
                    flag_id: Set(None),
                    container_id: Set(None),
                    last_container_operation: Set(Utc::now()),
                    ..Default::default()
                }
                .insert(&st.db)
                .await?;
            }
        }
    }

    distributed.release().await?;
    Ok(RequestResponse::ok(result))
}

/// `POST /api/exercise/{id}/container` — provision a per-user practice container.
pub async fn create_container(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<String>> {
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

    // Serialize get-or-create for this user/exercise. Without the in-lock re-read,
    // concurrent or repeated POSTs overwrite the instance pointer and orphan every
    // previously created backend container.
    let flight_key = instance_lock_key(user.id, id);
    let _flight = crate::utils::single_flight::coalesce(&flight_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &flight_key)
            .await?;
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
                    distributed.release().await?;
                    return Ok(RequestResponse::ok(current.entry()));
                }
                destroy_owned_exercise_container_with(
                    st.pg(),
                    Some(instance.id),
                    container_id,
                    &current.container_id,
                    None,
                    crate::services::traffic::destroy_container_after_capture_fence(
                        &st,
                        &current.container_id,
                    ),
                )
                .await?;
            } else {
                sqlx::query(
                    r#"UPDATE "ExerciseInstances"
                          SET container_id = NULL, is_loaded = FALSE
                        WHERE id = $1 AND container_id = $2"#,
                )
                .bind(instance.id)
                .bind(container_id)
                .execute(st.pg())
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            }
            instance.container_id = None;
            instance.is_loaded = false;
        }
    }

    let flag = flag_generator::generate_flag(
        e.flag_template.as_deref(),
        &crate::utils::codec::sha256_str(&format!("EX@{id}@{}", user.id)),
    );
    let cuuid = uuid::Uuid::new_v4();
    let info = st
        .containers
        .create(ContainerSpec {
            game_kind: crate::services::container::game_kind_for_challenge(e.challenge_type),
            image: image.clone(),
            memory_limit: e.memory_limit.unwrap_or(64),
            cpu_count: e.cpu_count.unwrap_or(1),
            expose_port: e.expose_port.unwrap_or(80),
            publish_port: true,
            env: vec![],
            flag: Some(flag.clone()),
            ad_network: None,
            allow_egress: true,
            operation_id: Some(format!("container:{cuuid}")),
        })
        .await?;

    let backend_id = info.id.clone();
    let mut created_flag_id = None;
    let mut linked_exercise_instance_id = None;
    let existing_exercise_instance_id = existing.as_ref().map(|instance| instance.id);
    let persisted: AppResult<container::Model> = async {
        let now = Utc::now();
        let flag_row = flag_context::ActiveModel {
            flag: Set(flag),
            is_occupied: Set(true),
            attachment_id: Set(None),
            challenge_id: Set(None),
            exercise_id: Set(Some(id)),
            ..Default::default()
        }
        .insert(&st.db)
        .await?;
        created_flag_id = Some(flag_row.id);

        let c = container::ActiveModel {
            id: Set(cuuid),
            image: Set(image),
            container_id: Set(info.id),
            status: Set(ContainerStatus::Running),
            started_at: Set(now),
            expect_stop_at: Set(now + chrono::Duration::hours(2)),
            is_proxy: Set(st.containers.requires_proxy()),
            ip: Set(info.ip),
            port: Set(info.port),
            public_ip: Set(None),
            public_port: Set(None),
            game_instance_id: Set(None),
            exercise_instance_id: Set(existing_exercise_instance_id),
            ad_team_service_id: Set(None),
        }
        .insert(&st.db)
        .await?;

        let exercise_instance = match existing {
            Some(i) => {
                let mut am: exercise_instance::ActiveModel = i.into();
                am.container_id = Set(Some(cuuid));
                am.flag_id = Set(Some(flag_row.id));
                am.is_loaded = Set(true);
                am.last_container_operation = Set(now);
                am.update(&st.db).await?
            }
            None => {
                exercise_instance::ActiveModel {
                    exercise_id: Set(id),
                    user_id: Set(user.id),
                    is_loaded: Set(true),
                    is_solved: Set(false),
                    flag_id: Set(Some(flag_row.id)),
                    container_id: Set(Some(cuuid)),
                    last_container_operation: Set(now),
                    ..Default::default()
                }
                .insert(&st.db)
                .await?
            }
        };
        linked_exercise_instance_id = Some(exercise_instance.id);

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
        .bind(exercise_instance.id)
        .bind(id)
        .bind(user.id)
        .execute(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if linked.rows_affected() != 1 {
            return Err(AppError::internal(
                "exercise container ownership link changed during provisioning",
            ));
        }

        Ok(c)
    }
    .await;

    let c = match persisted {
        Ok(c) => c,
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

    distributed.release().await?;
    Ok(RequestResponse::ok(c.entry()))
}

/// `DELETE /api/exercise/{id}/container` — tear down the user's container.
pub async fn destroy_container(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<MessageResponse> {
    let lock_key = instance_lock_key(user.id, id);
    let _instance_guard = crate::utils::single_flight::coalesce(&lock_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire(st.pg(), &lock_key).await?;
    let inst = user_instance(&st, id, user.id)
        .await?
        .ok_or_else(|| AppError::not_found("No instance"))?;
    if let Some(cuuid) = inst.container_id {
        if let Some(c) = container::Entity::find_by_id(cuuid).one(&st.db).await? {
            destroy_owned_exercise_container_with(
                st.pg(),
                Some(inst.id),
                cuuid,
                &c.container_id,
                None,
                crate::services::traffic::destroy_container_after_capture_fence(
                    &st,
                    &c.container_id,
                ),
            )
            .await?;
        } else {
            sqlx::query(
                r#"UPDATE "ExerciseInstances"
                      SET container_id = NULL, is_loaded = FALSE
                    WHERE id = $1 AND container_id = $2"#,
            )
            .bind(inst.id)
            .bind(cuuid)
            .execute(st.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        }
    }
    distributed.release().await?;
    Ok(MessageResponse::ok("Container destroyed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failed_destroy_never_reaches_exercise_owner_cleanup() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://rsctf:rsctf@127.0.0.1:1/rsctf")
            .unwrap();
        let error = destroy_owned_exercise_container_with(
            &pool,
            Some(7),
            uuid::Uuid::nil(),
            "runtime-7",
            None,
            async { Err(AppError::internal("injected destroy failure")) },
        )
        .await
        .unwrap_err();

        assert_eq!(error.to_string(), "injected destroy failure");
    }
}
