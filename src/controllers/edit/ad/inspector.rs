//! Short-lived, service-owned A&D inspector containers.

use super::*;

const INSPECTOR_LIFETIME_MINUTES: i64 = 30;

#[derive(Clone, Debug, PartialEq, sqlx::FromRow)]
struct InspectorDefinition {
    challenge_id: i32,
    challenge_type: i16,
    ad_self_hosted: bool,
    build_status: i16,
    build_image_digest: Option<String>,
    memory_limit: Option<i32>,
    cpu_count: Option<i32>,
    expose_port: Option<i32>,
    workload_spec: Option<JsonValue>,
}

#[derive(Debug, sqlx::FromRow)]
struct ExistingInspector {
    id: Uuid,
    backend_id: String,
    image: String,
}

const LOAD_DEFINITION_SQL: &str = r#"SELECT challenge.id AS challenge_id,
                  challenge."Type" AS challenge_type,
                  challenge.ad_self_hosted,
                  challenge.build_status,
                  challenge.build_image_digest,
                  challenge.memory_limit,
                  challenge.cpu_count,
                  challenge.expose_port,
                  challenge.workload_spec
             FROM "AdTeamServices" service
             JOIN "GameChallenges" challenge
               ON challenge.id = service.challenge_id
              AND challenge.game_id = service.game_id
             JOIN "Games" game ON game.id = service.game_id
             JOIN "Participations" participation
               ON participation.id = service.participation_id
              AND participation.game_id = service.game_id
             JOIN "Teams" team ON team.id = participation.team_id
            WHERE service.id = $1
              AND service.game_id = $2
              AND game.deletion_pending = FALSE
              AND game.end_time_utc >= clock_timestamp()
              AND challenge.deletion_pending = FALSE
              AND challenge.is_enabled = TRUE
              AND challenge.review_status = $3
              AND participation.status = $4
              AND team.deletion_pending = FALSE"#;

async fn find_definition(
    pool: &sqlx::PgPool,
    game_id: i32,
    service_id: i32,
) -> AppResult<Option<InspectorDefinition>> {
    sqlx::query_as::<_, InspectorDefinition>(LOAD_DEFINITION_SQL)
        .bind(service_id)
        .bind(game_id)
        .bind(ChallengeReviewStatus::Active as i16)
        .bind(ParticipationStatus::Accepted as i16)
        .fetch_optional(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

async fn load_definition(
    st: &SharedState,
    game_id: i32,
    service_id: i32,
) -> AppResult<InspectorDefinition> {
    find_definition(st.pg(), game_id, service_id)
        .await?
        .ok_or_else(|| AppError::not_found("A&D service not found"))
}

/// Destruction remains available after a spawn-eligibility fence is set. Only
/// the exact game/service relationship is relevant: disabled, suspended,
/// ended, or deletion-pending owners must still be able to eagerly reap their
/// existing inspector instead of waiting for the TTL/orphan sweeper.
async fn require_service_in_game(
    pool: &sqlx::PgPool,
    game_id: i32,
    service_id: i32,
) -> AppResult<()> {
    let belongs = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS (
             SELECT 1 FROM "AdTeamServices"
              WHERE id = $1 AND game_id = $2
           )"#,
    )
    .bind(service_id)
    .bind(game_id)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if belongs {
        Ok(())
    } else {
        Err(AppError::not_found("A&D service not found"))
    }
}

fn validate_definition(st: &SharedState, definition: &InspectorDefinition) -> AppResult<String> {
    if definition.challenge_type != ChallengeType::AttackDefense as i16 {
        return Err(AppError::bad_request(
            "Inspector containers require an Attack-Defense service",
        ));
    }
    if definition.ad_self_hosted {
        return Err(AppError::bad_request(
            "Self-hosted services must be inspected on the team's worker",
        ));
    }
    if definition.workload_spec.is_some() {
        return Err(AppError::bad_request(
            "Aggregate A&D workloads do not expose a single inspector target",
        ));
    }
    if st.containers.backend_kind() != crate::services::container::ContainerBackendKind::Docker {
        return Err(AppError::bad_request(
            "Interactive inspector containers currently require the Docker control runtime",
        ));
    }
    crate::services::challenge_images::runtime_image_from_build_fields(
        st,
        definition.build_status,
        definition.build_image_digest.as_deref(),
    )
}

async fn load_existing(st: &SharedState, service_id: i32) -> AppResult<Option<ExistingInspector>> {
    sqlx::query_as::<_, ExistingInspector>(
        r#"SELECT id, container_id AS backend_id, image
             FROM "Containers"
            WHERE ad_team_service_id = $1"#,
    )
    .bind(service_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

async fn destroy_exact(
    st: &SharedState,
    service_id: i32,
    inspector: &ExistingInspector,
) -> AppResult<()> {
    st.containers.destroy(&inspector.backend_id).await?;
    sqlx::query(
        r#"DELETE FROM "Containers"
            WHERE id = $1 AND container_id = $2 AND ad_team_service_id = $3"#,
    )
    .bind(inspector.id)
    .bind(&inspector.backend_id)
    .bind(service_id)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

/// Tear down the current inspector while the caller holds
/// `ad-inspector:{service_id}`. Challenge/game deletion use this before the
/// service row can cascade away its database-side exec capability.
pub(crate) async fn destroy_service_inspector_locked(
    st: &SharedState,
    service_id: i32,
) -> AppResult<()> {
    if let Some(existing) = load_existing(st, service_id).await? {
        destroy_exact(st, service_id, &existing).await?;
    }
    Ok(())
}

/// Serialize inspector teardown across replicas. Callers must first persist
/// the eligibility fence that prevents a replacement (challenge/game/team
/// deletion or participation suspension) before invoking this helper.
pub(crate) async fn destroy_service_inspector(st: &SharedState, service_id: i32) -> AppResult<()> {
    let lock_key = format!("ad-inspector:{service_id}");
    let _local = crate::utils::single_flight::coalesce(&lock_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &lock_key)
            .await?;
    let result = destroy_service_inspector_locked(st, service_id).await;
    let released = distributed.release().await.map_err(AppError::from);
    match (result, released) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

/// Spawn or reuse the one short-lived inspector owned by this A&D team service.
/// The public GUID is a `Containers.id`, so the exec hub resolves it through its
/// normal database ownership boundary instead of accepting a raw Docker id.
pub async fn ad_spawn_inspector(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, service_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<JsonValue>> {
    manager_or_admin(&st, &user, game_id).await?;
    let lock_key = format!("ad-inspector:{service_id}");
    let _local = crate::utils::single_flight::coalesce(&lock_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &lock_key)
            .await?;

    let result = async {
        let definition = load_definition(&st, game_id, service_id).await?;
        let image = validate_definition(&st, &definition)?;

        if let Some(existing) = load_existing(&st, service_id).await? {
            if existing.image == image && st.containers.is_running(&existing.backend_id).await {
                sqlx::query(
                    r#"UPDATE "Containers"
                          SET expect_stop_at = CURRENT_TIMESTAMP + ($2 * interval '1 minute')
                        WHERE id = $1 AND ad_team_service_id = $3"#,
                )
                .bind(existing.id)
                .bind(INSPECTOR_LIFETIME_MINUTES)
                .bind(service_id)
                .execute(st.pg())
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
                return Ok(RequestResponse::ok(
                    json!({ "containerGuid": existing.id }),
                ));
            }
            destroy_exact(&st, service_id, &existing).await?;
        }

        let public_id = Uuid::new_v4();
        let operation_id = Some(format!("ad-inspector:{service_id}:{image}"));
        let info = st
            .containers
            .create(ContainerSpec {
                game_kind: crate::services::container::game_kind_for_challenge(
                    ChallengeType::AttackDefense,
                ),
                image: image.clone(),
                memory_limit: definition.memory_limit.unwrap_or(64),
                cpu_count: definition.cpu_count.unwrap_or(1),
                expose_port: definition.expose_port.unwrap_or(80),
                publish_port: false,
                env: Vec::new(),
                flag: None,
                ad_network: None,
                allow_egress: false,
                operation_id,
            })
            .await?;

        let publish = async {
            let current = load_definition(&st, game_id, service_id).await?;
            if current != definition || validate_definition(&st, &current)? != image {
                return Err(AppError::conflict(
                    "The A&D service definition changed while its inspector was starting; retry",
                ));
            }
            sqlx::query(
                r#"INSERT INTO "Containers"
                   (id, image, container_id, status, started_at, expect_stop_at,
                    is_proxy, ip, port, public_ip, public_port,
                    game_instance_id, exercise_instance_id, ad_team_service_id)
                   VALUES
                   ($1, $2, $3, $4, CURRENT_TIMESTAMP,
                    CURRENT_TIMESTAMP + ($5 * interval '1 minute'),
                    TRUE, $6, $7, NULL, NULL, NULL, NULL, $8)"#,
            )
            .bind(public_id)
            .bind(&image)
            .bind(&info.id)
            .bind(ContainerStatus::Running as i16)
            .bind(INSPECTOR_LIFETIME_MINUTES)
            .bind(&info.ip)
            .bind(info.port)
            .bind(service_id)
            .execute(st.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            Ok(RequestResponse::ok(json!({ "containerGuid": public_id })))
        }
        .await;

        if publish.is_err() {
            if let Err(error) = st.containers.destroy(&info.id).await {
                tracing::warn!(backend_id = %info.id, %error, "unpublished A&D inspector cleanup failed");
            }
        }
        publish
    }
    .await;

    let released = distributed.release().await.map_err(AppError::from);
    match (result, released) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(response), Ok(())) => Ok(response),
    }
}

/// Destroy only the inspector owned by this exact service and public GUID.
/// A stale close from an old browser tab cannot tear down its replacement.
pub async fn ad_destroy_inspector(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, service_id, container_guid)): Path<(i32, i32, String)>,
) -> AppResult<MessageResponse> {
    manager_or_admin(&st, &user, game_id).await?;
    let requested = Uuid::parse_str(&container_guid)
        .map_err(|_| AppError::bad_request("Invalid inspector container GUID"))?;
    let lock_key = format!("ad-inspector:{service_id}");
    let _local = crate::utils::single_flight::coalesce(&lock_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &lock_key)
            .await?;

    let result = async {
        // Preserve path ownership even when the requested GUID is already gone.
        require_service_in_game(st.pg(), game_id, service_id).await?;
        if let Some(existing) = load_existing(&st, service_id).await? {
            if existing.id == requested {
                destroy_exact(&st, service_id, &existing).await?;
            }
        }
        Ok(MessageResponse::ok(""))
    }
    .await;
    let released = distributed.release().await.map_err(AppError::from);
    match (result, released) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(response), Ok(())) => Ok(response),
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;

    #[test]
    fn inspector_lifetime_is_bounded_and_identity_is_service_scoped() {
        assert!((1..=60).contains(&INSPECTOR_LIFETIME_MINUTES));
        let first = format!("ad-inspector:{}:{}", 7, "sha256:a");
        let second = format!("ad-inspector:{}:{}", 8, "sha256:a");
        assert_ne!(first, second);
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn definition_eligibility_fences_every_destructive_transition() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("ad_inspector_gate_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TABLE "Games" (
                 id INTEGER PRIMARY KEY,
                 deletion_pending BOOLEAN NOT NULL,
                 end_time_utc TIMESTAMPTZ NOT NULL
               );
               CREATE TABLE "Teams" (
                 id INTEGER PRIMARY KEY,
                 deletion_pending BOOLEAN NOT NULL
               );
               CREATE TABLE "Participations" (
                 id INTEGER PRIMARY KEY,
                 game_id INTEGER NOT NULL,
                 team_id INTEGER NOT NULL,
                 status SMALLINT NOT NULL
               );
               CREATE TABLE "GameChallenges" (
                 id INTEGER PRIMARY KEY,
                 game_id INTEGER NOT NULL,
                 "Type" SMALLINT NOT NULL,
                 ad_self_hosted BOOLEAN NOT NULL,
                 build_status SMALLINT NOT NULL,
                 build_image_digest TEXT,
                 memory_limit INTEGER,
                 cpu_count INTEGER,
                 expose_port INTEGER,
                 workload_spec JSONB,
                 deletion_pending BOOLEAN NOT NULL,
                 is_enabled BOOLEAN NOT NULL,
                 review_status SMALLINT NOT NULL
               );
               CREATE TABLE "AdTeamServices" (
                 id INTEGER PRIMARY KEY,
                 game_id INTEGER NOT NULL,
                 challenge_id INTEGER NOT NULL,
                 participation_id INTEGER NOT NULL
               );"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(
            r#"INSERT INTO "Games" VALUES
                 (1, FALSE, clock_timestamp() + interval '1 hour');
               INSERT INTO "Teams" VALUES (2, FALSE);
               INSERT INTO "Participations" VALUES (3, 1, 2, 1);
               INSERT INTO "GameChallenges" VALUES
                 (4, 1, 5, FALSE, 2,
                  'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                  64, 1, 8080, NULL, FALSE, TRUE, 0);
               INSERT INTO "AdTeamServices" VALUES (5, 1, 4, 3);"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        assert!(find_definition(&pool, 1, 5).await.unwrap().is_some());
        assert!(find_definition(&pool, 9, 5).await.unwrap().is_none());
        require_service_in_game(&pool, 1, 5).await.unwrap();
        assert!(matches!(
            require_service_in_game(&pool, 9, 5).await,
            Err(AppError::NotFound(_))
        ));

        for (disable, restore) in [
            (
                r#"UPDATE "Participations" SET status = 3 WHERE id = 3"#,
                r#"UPDATE "Participations" SET status = 1 WHERE id = 3"#,
            ),
            (
                r#"UPDATE "Teams" SET deletion_pending = TRUE WHERE id = 2"#,
                r#"UPDATE "Teams" SET deletion_pending = FALSE WHERE id = 2"#,
            ),
            (
                r#"UPDATE "GameChallenges" SET is_enabled = FALSE WHERE id = 4"#,
                r#"UPDATE "GameChallenges" SET is_enabled = TRUE WHERE id = 4"#,
            ),
            (
                r#"UPDATE "GameChallenges" SET review_status = 2 WHERE id = 4"#,
                r#"UPDATE "GameChallenges" SET review_status = 0 WHERE id = 4"#,
            ),
            (
                r#"UPDATE "GameChallenges" SET deletion_pending = TRUE WHERE id = 4"#,
                r#"UPDATE "GameChallenges" SET deletion_pending = FALSE WHERE id = 4"#,
            ),
            (
                r#"UPDATE "Games" SET deletion_pending = TRUE WHERE id = 1"#,
                r#"UPDATE "Games" SET deletion_pending = FALSE WHERE id = 1"#,
            ),
            (
                r#"UPDATE "Games" SET end_time_utc = clock_timestamp() - interval '1 second'
                    WHERE id = 1"#,
                r#"UPDATE "Games" SET end_time_utc = clock_timestamp() + interval '1 hour'
                    WHERE id = 1"#,
            ),
        ] {
            sqlx::query(disable).execute(&pool).await.unwrap();
            assert!(
                find_definition(&pool, 1, 5).await.unwrap().is_none(),
                "transition failed to fence inspector publication: {disable}"
            );
            require_service_in_game(&pool, 1, 5)
                .await
                .expect("an eligibility fence blocked eager inspector destruction");
            sqlx::query(restore).execute(&pool).await.unwrap();
            assert!(find_definition(&pool, 1, 5).await.unwrap().is_some());
        }

        sqlx::query(r#"DELETE FROM "AdTeamServices" WHERE id = 5"#)
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            require_service_in_game(&pool, 1, 5).await,
            Err(AppError::NotFound(_))
        ));

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
