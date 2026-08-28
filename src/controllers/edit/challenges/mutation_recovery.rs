use super::*;

pub(super) const INSERTABLE_GAME_SQL: &str =
    r#"SELECT NOT deletion_pending FROM "Games" WHERE id = $1 FOR SHARE"#;
const CREATE_OPERATION_RETENTION: i64 = 128;

pub(super) async fn claim_challenge_create_operation(
    connection: &mut sqlx::PgConnection,
    actor_id: Uuid,
    game_id: i32,
    operation_id: Uuid,
    request_digest: &str,
) -> AppResult<Option<i32>> {
    sqlx::query(
        r#"INSERT INTO "ChallengeCreateOperations"
                  (actor_id, game_id, operation_id, request_digest)
           VALUES ($1, $2, $3, $4)
           ON CONFLICT (actor_id, game_id, operation_id) DO NOTHING"#,
    )
    .bind(actor_id)
    .bind(game_id)
    .bind(operation_id)
    .bind(request_digest)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let (stored_digest, challenge_id) = sqlx::query_as::<_, (String, Option<i32>)>(
        r#"SELECT request_digest, challenge_id
             FROM "ChallengeCreateOperations"
            WHERE actor_id = $1 AND game_id = $2 AND operation_id = $3
            FOR UPDATE"#,
    )
    .bind(actor_id)
    .bind(game_id)
    .bind(operation_id)
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if stored_digest != request_digest {
        return Err(AppError::conflict(
            "Challenge create operation was already used with different input",
        ));
    }
    Ok(challenge_id)
}

pub(super) async fn complete_challenge_create_operation(
    connection: &mut sqlx::PgConnection,
    actor_id: Uuid,
    game_id: i32,
    operation_id: Uuid,
    challenge_id: i32,
) -> AppResult<()> {
    sqlx::query(
        r#"UPDATE "ChallengeCreateOperations"
              SET challenge_id = $4, completed_at_utc = clock_timestamp()
            WHERE actor_id = $1 AND game_id = $2 AND operation_id = $3
              AND challenge_id IS NULL"#,
    )
    .bind(actor_id)
    .bind(game_id)
    .bind(operation_id)
    .bind(challenge_id)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"DELETE FROM "ChallengeCreateOperations" old
            WHERE old.created_at_utc < clock_timestamp() - INTERVAL '7 days'
               OR (old.actor_id = $1 AND old.ctid IN (
                    SELECT ctid FROM "ChallengeCreateOperations"
                     WHERE actor_id = $1
                     ORDER BY created_at_utc DESC, operation_id DESC
                    OFFSET $2
               ))"#,
    )
    .bind(actor_id)
    .bind(CREATE_OPERATION_RETENTION)
    .execute(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

/// Publish one complete projected definition on the caller-owned game-control
/// transaction. The revision predicate is the operator's stale-write fence;
/// division seeding happens before that same transaction commits.
pub(super) async fn update_challenge_row_locked(
    connection: &mut sqlx::PgConnection,
    projected: &game_challenge::Model,
    expected_revision: i64,
) -> AppResult<i64> {
    let mut query = sqlx::QueryBuilder::<sqlx::Postgres>::new(r#"UPDATE "GameChallenges" SET "#);
    {
        let mut set = query.separated(", ");
        set.push("title = ").push_bind(&projected.title);
        set.push("content = ").push_bind(&projected.content);
        set.push("flag_template = ")
            .push_bind(&projected.flag_template);
        set.push("category = ").push_bind(projected.category as i16);
        set.push("hints = ")
            .push_bind(projected.hints.as_ref().map(sqlx::types::Json));
        set.push("is_enabled = ").push_bind(projected.is_enabled);
        set.push("file_name = ").push_bind(&projected.file_name);
        set.push("deadline_utc = ")
            .push_bind(projected.deadline_utc);
        set.push("submission_limit = ")
            .push_bind(projected.submission_limit);
        set.push("container_image = ")
            .push_bind(&projected.container_image);
        set.push("build_status = ")
            .push_bind(projected.build_status as i16);
        set.push("build_image_digest = ")
            .push_bind(&projected.build_image_digest);
        set.push("last_build_log = ")
            .push_bind(&projected.last_build_log);
        set.push("memory_limit = ")
            .push_bind(projected.memory_limit);
        set.push("cpu_count = ").push_bind(projected.cpu_count);
        set.push("storage_limit = ")
            .push_bind(projected.storage_limit);
        set.push("expose_port = ").push_bind(projected.expose_port);
        set.push("workload_spec = ")
            .push_bind(projected.workload_spec.as_ref().map(sqlx::types::Json));
        set.push("original_score = ")
            .push_bind(projected.original_score);
        set.push("min_score_rate = ")
            .push_bind(projected.min_score_rate);
        set.push("difficulty = ").push_bind(projected.difficulty);
        set.push("score_curve = ")
            .push_bind(projected.score_curve as i16);
        set.push("enable_traffic_capture = ")
            .push_bind(projected.enable_traffic_capture);
        set.push("disable_blood_bonus = ")
            .push_bind(projected.disable_blood_bonus);
        set.push("network_mode = ")
            .push_bind(projected.network_mode.map(|value| value as i16));
        set.push("enable_shared_container = ")
            .push_bind(projected.enable_shared_container);
        set.push("ad_checker_image = ")
            .push_bind(&projected.ad_checker_image);
        set.push("ad_allow_egress = ")
            .push_bind(projected.ad_allow_egress);
        set.push("ad_allow_self_reset = ")
            .push_bind(projected.ad_allow_self_reset);
        set.push("ad_ssh_requires_flag = ")
            .push_bind(projected.ad_ssh_requires_flag);
        set.push("ad_self_hosted = ")
            .push_bind(projected.ad_self_hosted);
        set.push("ad_scoring_weight = ")
            .push_bind(projected.ad_scoring_weight);
        set.push("variant_mode = ")
            .push_bind(projected.variant_mode as i16);
        set.push("variant_generator_image = ")
            .push_bind(&projected.variant_generator_image);
        set.push("variant_generator_digest = ")
            .push_bind(&projected.variant_generator_digest);
        set.push("variant_generator_build_context_subdir = ")
            .push_bind(&projected.variant_generator_build_context_subdir);
        set.push("variant_generator_build_status = ")
            .push_bind(projected.variant_generator_build_status as i16);
        set.push("variant_generator_last_build_log = ")
            .push_bind(&projected.variant_generator_last_build_log);
        set.push("solve_receipt_mode = ")
            .push_bind(projected.solve_receipt_mode as i16);
        set.push("receipt_verifier_identity = ")
            .push_bind(&projected.receipt_verifier_identity);
        set.push("revision = revision + 1");
    }
    query
        .push(" WHERE id = ")
        .push_bind(projected.id)
        .push(" AND game_id = ")
        .push_bind(projected.game_id)
        .push(" AND revision = ")
        .push_bind(expected_revision)
        .push(" AND deletion_pending = FALSE RETURNING revision");
    query
        .build_query_scalar::<i64>()
        .fetch_optional(connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| {
            AppError::conflict(
                "Challenge changed in another editor or is being deleted; reload before saving",
            )
        })
}
