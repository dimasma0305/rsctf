use std::time::Duration;

use uuid::Uuid;

use crate::utils::enums::Role;

use super::lease_cache::LeaseCache;

const EXERCISE_LEASE_SQL: &str = r#"SELECT EXISTS (
    SELECT 1
      FROM "ExerciseInstances" instance
      JOIN "ExerciseChallenges" exercise ON exercise.id = instance.exercise_id
      JOIN "Containers" container ON container.id = instance.container_id
      JOIN "AspNetUsers" account ON account.id = instance.user_id
     WHERE instance.id = $1
       AND instance.exercise_id = $2
       AND instance.user_id = $3
       AND instance.is_loaded = TRUE
       AND instance.container_id = $4
       AND exercise.is_enabled = TRUE
       AND exercise.publish_time_utc <= clock_timestamp()
       AND container.is_proxy = TRUE
       AND container.game_instance_id IS NULL
       AND account.security_stamp = $5
       AND account.email_confirmed = TRUE
       AND account.role <> $6
       AND (
           container.exercise_instance_id IS NULL
           OR container.exercise_instance_id = instance.id
       )
)"#;

pub(in crate::controllers::proxy) async fn exercise_lease_is_valid(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    expected_security_stamp: &str,
    exercise_instance_id: i32,
    exercise_id: i32,
    container_id: Uuid,
) -> bool {
    let key = ExerciseLeaseKey {
        user_id,
        security_stamp: expected_security_stamp.to_owned(),
        exercise_instance_id,
        exercise_id,
        container_id,
    };
    EXERCISE_LEASES
        .validate(key, || {
            exercise_lease_is_valid_authoritative(
                pool,
                user_id,
                expected_security_stamp,
                exercise_instance_id,
                exercise_id,
                container_id,
            )
        })
        .await
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ExerciseLeaseKey {
    user_id: Uuid,
    security_stamp: String,
    exercise_instance_id: i32,
    exercise_id: i32,
    container_id: Uuid,
}

static EXERCISE_LEASES: std::sync::LazyLock<LeaseCache<ExerciseLeaseKey>> =
    std::sync::LazyLock::new(|| LeaseCache::new(8_192, Duration::from_millis(250)));

async fn exercise_lease_is_valid_authoritative(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    expected_security_stamp: &str,
    exercise_instance_id: i32,
    exercise_id: i32,
    container_id: Uuid,
) -> bool {
    sqlx::query_scalar::<_, bool>(EXERCISE_LEASE_SQL)
        .bind(exercise_instance_id)
        .bind(exercise_id)
        .bind(user_id)
        .bind(container_id)
        .bind(expected_security_stamp)
        .bind(Role::Banned as i16)
        .fetch_one(pool)
        .await
        .unwrap_or(false)
}
