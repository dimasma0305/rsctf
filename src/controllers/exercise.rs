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
use crate::middlewares::rate_limiter::{limited, Policy};
use crate::models::data::{container, exercise_challenge, exercise_instance, flag_context};
use crate::services::container::ContainerSpec;
use crate::utils::crypto_utils::ct_eq;
use crate::utils::enums::{AnswerResult, ChallengeCategory, ContainerStatus};
use crate::utils::error::{AppError, AppResult};
use crate::utils::flag_generator;
use crate::utils::shared::{ArrayResponse, MessageResponse, RequestResponse};

const ELIGIBLE_EXERCISE_FLAGS_SQL: &str = r#"SELECT flag
      FROM "FlagContexts"
     WHERE exercise_id = $1
       AND (
           (id = $2 AND is_occupied = TRUE)
           OR is_occupied = FALSE
       )"#;

async fn eligible_exercise_flags(
    pool: &sqlx::PgPool,
    exercise_id: i32,
    current_flag_id: Option<i32>,
) -> AppResult<Vec<String>> {
    sqlx::query_scalar::<_, String>(ELIGIBLE_EXERCISE_FLAGS_SQL)
        .bind(exercise_id)
        .bind(current_flag_id)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/exercise", get(list))
        .route("/api/exercise/{id}", get(detail).post(submit))
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

fn user_container_lock_key(user_id: uuid::Uuid) -> String {
    format!("exercise-container-user:{user_id}")
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
            ORDER BY container.started_at ASC, instance.id ASC"#,
    )
    .bind(user_id)
    .bind(exercise_id)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
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
    let _e = load_exercise(&st, id).await?;
    let answer = model.flag.trim().to_string();
    if answer.is_empty() {
        return Err(AppError::bad_request("A flag is required"));
    }

    let lock_key = user_container_lock_key(user.id);
    let _instance_guard = crate::utils::single_flight::coalesce(&lock_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire(st.pg(), &lock_key).await?;
    let inst = user_instance(&st, id, user.id).await?;

    // Only the caller's current occupied flag and author-defined unoccupied
    // static flags are eligible. Other users' and stale instance flags share
    // the exercise id, so exercise-id-only fallback would cross that boundary.
    let eligible_flags = eligible_exercise_flags(
        st.pg(),
        id,
        inst.as_ref().and_then(|instance| instance.flag_id),
    )
    .await?;
    let accepted = eligible_flags.iter().any(|flag| ct_eq(flag, &answer));

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
    let flight_key = user_container_lock_key(user.id);
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
    let owned = other_owned_containers(st.pg(), user.id, id).await?;
    let maximum = usize::try_from(container_policy.max_exercise_container_count_per_user)
        .map_err(|_| AppError::internal("invalid exercise container limit"))?;
    if owned.len() >= maximum {
        if !container_policy.auto_destroy_on_limit_reached {
            distributed.release().await?;
            return Err(AppError::bad_request(format!(
                "The number of exercise containers cannot exceed {}",
                container_policy.max_exercise_container_count_per_user
            )));
        }
        let remove_count = owned.len() - maximum + 1;
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

    let flag = flag_generator::generate_flag_checked(
        e.flag_template.as_deref(),
        &flag_generator::exercise_user_hash(st.config.identity_hash_key.as_bytes(), id, user.id),
    )?;
    let game_kind = crate::services::container::game_kind_for_challenge(e.challenge_type);
    let platform_proxy =
        crate::controllers::admin::container_port_mapping(&st).await == "PlatformProxy";
    let is_proxy = crate::services::container::should_use_platform_proxy(
        game_kind,
        st.containers.requires_proxy(),
        platform_proxy,
        false,
    );
    let cuuid = uuid::Uuid::new_v4();
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
            expect_stop_at: Set(
                now + chrono::Duration::minutes(i64::from(container_policy.default_lifetime))
            ),
            is_proxy: Set(is_proxy),
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
    let lock_key = user_container_lock_key(user.id);
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

    #[test]
    fn eligible_flags_are_scoped_to_current_dynamic_or_static_rows() {
        assert!(ELIGIBLE_EXERCISE_FLAGS_SQL.contains("exercise_id = $1"));
        assert!(ELIGIBLE_EXERCISE_FLAGS_SQL.contains("id = $2 AND is_occupied = TRUE"));
        assert!(ELIGIBLE_EXERCISE_FLAGS_SQL.contains("OR is_occupied = FALSE"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn exercise_flags_reject_other_owners_and_cleanup_stale_instances() {
        use sqlx::postgres::PgPoolOptions;

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("exercise_flags_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = crate::migrations::test_pg_connect_options(&database_url)
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "FlagContexts" (
              id SERIAL PRIMARY KEY, flag TEXT NOT NULL,
              is_occupied BOOLEAN NOT NULL, exercise_id INTEGER
            );
            CREATE TABLE "ExerciseInstances" (
              id INTEGER PRIMARY KEY, container_id UUID,
              is_loaded BOOLEAN NOT NULL, flag_id INTEGER
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let own_flag_id = sqlx::query_scalar::<_, i32>(
            r#"INSERT INTO "FlagContexts" (flag, is_occupied, exercise_id)
               VALUES ('flag{own}', TRUE, 9) RETURNING id"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "FlagContexts" (flag, is_occupied, exercise_id)
               VALUES ('flag{other}', TRUE, 9), ('flag{static}', FALSE, 9)"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let eligible = eligible_exercise_flags(&pool, 9, Some(own_flag_id))
            .await
            .unwrap();
        assert!(eligible.iter().any(|flag| flag == "flag{own}"));
        assert!(eligible.iter().any(|flag| flag == "flag{static}"));
        assert!(!eligible.iter().any(|flag| flag == "flag{other}"));
        assert_eq!(
            eligible_exercise_flags(&pool, 9, None).await.unwrap(),
            vec!["flag{static}".to_string()]
        );

        let container_id = uuid::Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "ExerciseInstances" VALUES (41, $1, TRUE, $2)"#)
            .bind(container_id)
            .bind(own_flag_id)
            .execute(&pool)
            .await
            .unwrap();
        clear_exercise_container_owner(&pool, Some(41), container_id, None, Some(own_flag_id))
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_as::<_, (Option<uuid::Uuid>, bool, Option<i32>)>(
                r#"SELECT container_id, is_loaded, flag_id
                     FROM "ExerciseInstances" WHERE id = 41"#,
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            (None, false, None)
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "FlagContexts" WHERE id = $1"#)
                .bind(own_flag_id)
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
