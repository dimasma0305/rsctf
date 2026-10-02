use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HintReleaseModel {
    pub operation_id: Uuid,
    pub expected_revision: i64,
}

#[derive(sqlx::FromRow)]
struct HintReleaseRow {
    revision: i64,
    title: String,
    is_enabled: bool,
    released_hint_count: i32,
    hint_count: i32,
}

const RELEASE_NEXT_SQL: &str = r#"
UPDATE "GameChallenges"
   SET released_hint_count = released_hint_count + 1,
       revision = revision + 1
 WHERE id = $1 AND game_id = $2 AND revision = $3
   AND released_hint_count < CASE
       WHEN json_typeof(hints) = 'array' THEN json_array_length(hints)
       ELSE 0
   END
RETURNING revision
"#;

/// Release the next saved hint. The strict prefix rule keeps publication
/// deterministic and prevents an organizer from accidentally skipping drafts.
pub async fn release_next(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
    Json(model): Json<HintReleaseModel>,
) -> AppResult<RequestResponse<ChallengeEditDetailModel>> {
    manager_or_admin(&st, &user, game_id).await?;
    if !(1..=9_007_199_254_740_990).contains(&model.expected_revision) {
        return Err(AppError::bad_request(
            "expectedRevision must be a positive safe integer",
        ));
    }

    let scope = format!("game:{game_id}:challenge:{challenge_id}");
    let fingerprint = crate::services::mutation_operations::fingerprint(
        "challenge-hint-release",
        &serde_json::json!({
            "challengeId": challenge_id,
            "expectedRevision": model.expected_revision,
        }),
    )?;
    if let Some(replay) = crate::services::mutation_operations::find_completed(
        st.pg(),
        user.id,
        "challenge-hint-release",
        &scope,
        model.operation_id,
        fingerprint,
    )
    .await?
    {
        if replay.result_id != challenge_id.to_string() {
            return Err(AppError::conflict(
                "operationId belongs to a different challenge",
            ));
        }
        return response(&st, game_id, challenge_id).await;
    }

    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    let replay = crate::services::mutation_operations::claim(
        control.transaction_mut(),
        user.id,
        "challenge-hint-release",
        &scope,
        model.operation_id,
        fingerprint,
    )
    .await?;

    if replay.is_none() {
        let current = sqlx::query_as::<_, HintReleaseRow>(
            r#"SELECT revision, title, is_enabled, released_hint_count,
                      CASE WHEN json_typeof(hints) = 'array'
                           THEN json_array_length(hints) ELSE 0 END AS hint_count
                 FROM "GameChallenges"
                WHERE id = $1 AND game_id = $2 AND deletion_pending = FALSE
                FOR UPDATE"#,
        )
        .bind(challenge_id)
        .bind(game_id)
        .fetch_optional(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::not_found("Challenge not found"))?;

        if current.revision != model.expected_revision {
            return Err(AppError::conflict(
                "Challenge revision changed; reload and retry the release",
            ));
        }
        if current.released_hint_count >= current.hint_count {
            return Err(AppError::bad_request(
                "All saved hints are already released",
            ));
        }

        let revision = sqlx::query_scalar::<_, i64>(RELEASE_NEXT_SQL)
            .bind(challenge_id)
            .bind(game_id)
            .bind(model.expected_revision)
            .fetch_optional(&mut **control.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
            .ok_or_else(|| AppError::conflict("Challenge revision changed; retry the release"))?;

        let active = sqlx::query_scalar::<_, bool>(
            r#"SELECT start_time_utc <= clock_timestamp()
                      AND end_time_utc >= clock_timestamp()
                 FROM "Games" WHERE id = $1"#,
        )
        .bind(game_id)
        .fetch_one(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        let effects = serde_json::json!({
            "title": current.title,
            "scoreboard": false,
            "vpn": false,
            "repoPush": false,
            "runtime": false,
            "newChallengeNotice": false,
            "newHintNotice": active && current.is_enabled,
        });
        sqlx::query(
            r#"INSERT INTO "ChallengeRevisionEffects"
                 (game_id, challenge_id, revision, effects)
               VALUES ($1, $2, $3, $4)
               ON CONFLICT (challenge_id, revision) DO NOTHING"#,
        )
        .bind(game_id)
        .bind(challenge_id)
        .bind(revision)
        .bind(effects)
        .execute(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        crate::services::mutation_operations::complete(
            control.transaction_mut(),
            user.id,
            "challenge-hint-release",
            &scope,
            model.operation_id,
            &challenge_id.to_string(),
            Some(revision),
        )
        .await?;
    }

    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    response(&st, game_id, challenge_id).await
}

async fn response(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<RequestResponse<ChallengeEditDetailModel>> {
    let challenge = load_challenge(st, game_id, challenge_id).await?;
    let flags = if challenge.challenge_type == ChallengeType::DynamicContainer {
        Vec::new()
    } else {
        load_flags(st, challenge_id).await?
    };
    Ok(RequestResponse::ok(
        ChallengeEditDetailModel::from_challenge(st, &challenge, flags).await?,
    ))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::RELEASE_NEXT_SQL;

    #[test]
    fn release_is_authorized_before_the_atomic_revision_write() {
        let source = include_str!("hints.rs");
        let handler = &source[source.find("pub async fn release_next").unwrap()..];
        let authorization = handler.find("manager_or_admin").unwrap();
        let mutation = handler
            .find("query_scalar::<_, i64>(RELEASE_NEXT_SQL)")
            .unwrap();
        assert!(authorization < mutation);
        assert!(source.contains("released_hint_count = released_hint_count + 1"));
        assert!(RELEASE_NEXT_SQL.contains("WHERE id = $1 AND game_id = $2 AND revision = $3"));
        assert!(source.contains("All saved hints are already released"));
        assert!(source.contains("\"newHintNotice\": active && current.is_enabled"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn concurrent_release_of_one_revision_publishes_exactly_one_hint() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("hint_release_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            r#"CREATE TABLE "GameChallenges" (
                id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
                revision BIGINT NOT NULL, hints JSON,
                released_hint_count INTEGER NOT NULL DEFAULT 0
            )"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "GameChallenges" (id, game_id, revision, hints)
               VALUES (7, 3, 1, '["one", "two"]')"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let release = |pool: sqlx::PgPool| async move {
            sqlx::query_scalar::<_, i64>(RELEASE_NEXT_SQL)
                .bind(7_i32)
                .bind(3_i32)
                .bind(1_i64)
                .fetch_optional(&pool)
                .await
                .unwrap()
        };
        let (left, right) = tokio::join!(release(pool.clone()), release(pool.clone()));
        assert_eq!(
            usize::from(left.is_some()) + usize::from(right.is_some()),
            1
        );
        let state: (i32, i64) = sqlx::query_as(
            r#"SELECT released_hint_count, revision FROM "GameChallenges" WHERE id = 7"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(state, (1, 2));

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
