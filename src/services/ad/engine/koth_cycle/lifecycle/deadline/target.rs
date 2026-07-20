use crate::utils::error::{AppError, AppResult};

async fn persist_target_state(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
    destroyed_container_ids: Option<&[String]>,
) -> AppResult<()> {
    sqlx::query(
        r#"WITH target AS (
             UPDATE "KothTargets"
                SET host = '', port = 0,
                    container_id = CASE
                      WHEN $3::text[] IS NULL THEN container_id ELSE NULL
                    END,
                    holder_participation_id = NULL, held_since = NULL
              WHERE game_id = $1 AND challenge_id = $2
                AND ($3::text[] IS NULL OR container_id IS NULL
                     OR container_id = ANY($3))
                AND ($3::text[] IS NULL
                     OR (NULLIF(BTRIM(host), '') IS NULL AND port = 0))
              RETURNING id
           )
           DELETE FROM "KothClaimStates" claim
            USING target WHERE claim.target_id = target.id"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(destroyed_container_ids)
    .execute(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

/// Make the target unroutable while retaining the exact runtime identity for
/// capture fencing and backend-destroy retries.
pub(super) async fn persist_deadline_target_deactivation(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<()> {
    persist_target_state(connection, game_id, challenge_id, None).await
}

/// Release only an inactive target identity included in the successfully
/// destroyed runtime set. A concurrently published replacement is untouched.
pub(in super::super) async fn clear_destroyed_deadline_target(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
    destroyed_container_ids: &[String],
) -> AppResult<()> {
    persist_target_state(
        connection,
        game_id,
        challenge_id,
        Some(destroyed_container_ids),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::{clear_destroyed_deadline_target, persist_deadline_target_deactivation};

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn deadline_target_identity_survives_until_exact_destroy_succeeds() {
        use sqlx::{Connection, PgConnection};

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
        let mut connection = PgConnection::connect(&database_url).await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "KothTargets" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL, host TEXT NOT NULL,
              port INTEGER NOT NULL, container_id TEXT,
              holder_participation_id INTEGER, held_since TIMESTAMPTZ
            );
            CREATE TEMP TABLE "KothClaimStates" (target_id INTEGER PRIMARY KEY);
            INSERT INTO "KothTargets" VALUES
              (1, 7, 9, '10.13.40.7', 8080, 'runtime-old', 11, clock_timestamp());
            INSERT INTO "KothClaimStates" VALUES (1);
            "#,
        )
        .execute(&mut connection)
        .await
        .unwrap();

        persist_deadline_target_deactivation(&mut connection, 7, 9)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_as::<_, (String, i32, Option<String>)>(
                r#"SELECT host, port, container_id FROM "KothTargets" WHERE id = 1"#,
            )
            .fetch_one(&mut connection)
            .await
            .unwrap(),
            (String::new(), 0, Some("runtime-old".to_string()))
        );

        clear_destroyed_deadline_target(&mut connection, 7, 9, &["runtime-other".to_string()])
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, Option<String>>(
                r#"SELECT container_id FROM "KothTargets" WHERE id = 1"#,
            )
            .fetch_one(&mut connection)
            .await
            .unwrap(),
            Some("runtime-old".to_string())
        );

        clear_destroyed_deadline_target(&mut connection, 7, 9, &["runtime-old".to_string()])
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, Option<String>>(
                r#"SELECT container_id FROM "KothTargets" WHERE id = 1"#,
            )
            .fetch_one(&mut connection)
            .await
            .unwrap(),
            None
        );
    }
}
