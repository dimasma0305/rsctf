//! KotH hill provisioning + shared participation/team-name helpers — split from
//! koth/mod.rs to stay under the 1000-line rule.
use super::*;

/// Clear one exact backend publication before replacement. During active official
/// scoring a held target requires both a fresh confirmed-dead inspection by the
/// caller and a durable checker receipt for the same backend and holder.
async fn clear_target_for_replacement(
    st: &SharedState,
    target_id: i32,
    game_id: i32,
    challenge_id: i32,
    expected_container_id: &str,
    confirmed_dead: bool,
) -> AppResult<bool> {
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    let cleared = clear_target_for_replacement_locked(
        &mut *control.transaction_mut(),
        target_id,
        game_id,
        challenge_id,
        expected_container_id,
        confirmed_dead,
    )
    .await?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if cleared {
        crate::controllers::game::ad::invalidate_live_hill_snapshot(st, game_id).await;
    }
    Ok(cleared)
}

async fn clear_target_for_replacement_locked(
    connection: &mut sqlx::PgConnection,
    target_id: i32,
    game_id: i32,
    challenge_id: i32,
    expected_container_id: &str,
    confirmed_dead: bool,
) -> AppResult<bool> {
    let cleared = sqlx::query_scalar::<_, i32>(
        r#"UPDATE "KothTargets" target
              SET host = '', port = 0, container_id = NULL,
                  holder_participation_id = NULL, held_since = NULL
             FROM "Games" game, "GameChallenges" challenge
            WHERE target.id = $1
              AND target.game_id = $2
              AND target.challenge_id = $3
              AND target.container_id = $4
              AND game.id = target.game_id
              AND challenge.id = target.challenge_id
              AND challenge.game_id = target.game_id
              AND challenge.is_enabled = TRUE
              AND challenge.review_status = $7
              AND challenge."Type" = $8
              AND game.end_time_utc > clock_timestamp()
              AND (
                    target.holder_participation_id IS NULL
                    OR game.koth_scoring_start_round IS NULL
                    OR game.start_time_utc > clock_timestamp()
                    OR game.ad_scoring_paused = TRUE
                    OR ($5 AND EXISTS (
                         SELECT 1 FROM "KothControlResults" result
                          WHERE result.game_id = target.game_id
                            AND result.challenge_id = target.challenge_id
                            AND result.status = $6
                            AND result.responsible_participation_id =
                                target.holder_participation_id
                            AND result.dead_container_id = target.container_id
                            AND result.checked_at >= COALESCE(
                                  target.held_since, '-infinity'::timestamptz
                                )
                            AND result.id = (
                                  SELECT latest.id
                                    FROM "KothControlResults" latest
                                   WHERE latest.game_id = target.game_id
                                     AND latest.challenge_id = target.challenge_id
                                   ORDER BY latest.checked_at DESC, latest.id DESC
                                   LIMIT 1
                                )
                    ))
              )
        RETURNING target.id"#,
    )
    .bind(target_id)
    .bind(game_id)
    .bind(challenge_id)
    .bind(expected_container_id)
    .bind(confirmed_dead)
    .bind(crate::utils::enums::AdCheckStatus::Offline as i16)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeType::KingOfTheHill as i16)
    .fetch_optional(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .is_some();
    Ok(cleared)
}

/// Publish a replacement only into an unowned target slot. This runs while the
/// caller still holds the per-challenge provisioning lock, then briefly takes the
/// game-control lock; no container-runtime call occurs under the latter.
async fn publish_replacement_target(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
    host: &str,
    port: i32,
    container_id: Option<&str>,
) -> AppResult<bool> {
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    let published = sqlx::query_scalar::<_, i32>(
        r#"INSERT INTO "KothTargets"
                 (game_id, challenge_id, host, port, container_id,
                  holder_participation_id, held_since)
           SELECT game.id, challenge.id, $3, $4, $5, NULL, NULL
             FROM "Games" game
             JOIN "GameChallenges" challenge ON challenge.game_id = game.id
            WHERE game.id = $1
              AND challenge.id = $2
              AND game.end_time_utc > clock_timestamp()
              AND challenge.is_enabled = TRUE
              AND challenge.review_status = $6
              AND challenge."Type" = $7
           ON CONFLICT (game_id, challenge_id) DO UPDATE SET
             host = EXCLUDED.host,
             port = EXCLUDED.port,
             container_id = EXCLUDED.container_id,
             holder_participation_id = NULL,
             held_since = NULL
           WHERE "KothTargets".container_id IS NULL
        RETURNING id"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(host)
    .bind(port)
    .bind(container_id)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeType::KingOfTheHill as i16)
    .fetch_optional(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .is_some();
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if published {
        crate::controllers::game::ad::invalidate_live_hill_snapshot(st, game_id).await;
    }
    Ok(published)
}

/// Provision the shared hill for every enabled KotH challenge in a game: create the
/// `koth_target` row (so the hill appears on the board) and launch the single shared
/// hill container when the challenge is platform-hosted. Teams claim a hill by writing
/// their minted token into its `/koth/king` (the checker reads it each tick); nothing
/// is planted here. Idempotent — skips a hill that already has a running container.
/// Called from the operator "Ensure containers" action and on startup, so KotH hills
/// exist before the game runs.
pub async fn ensure_koth_hills(st: &SharedState, game_id: i32) -> AppResult<u64> {
    let crown_owned: bool = sqlx::query_scalar(
        r#"SELECT koth_scoring_start_round IS NOT NULL
             FROM "Games" WHERE id = $1"#,
    )
    .bind(game_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .unwrap_or(false);
    if crown_owned {
        // Once official scoring starts, every recreate belongs to the durable
        // cycle state machine. Provisioning must not bypass its
        // snapshot, token revocation, readiness, or cooldown phases.
        return Ok(0);
    }
    let hills = game_challenge::Entity::find()
        .filter(game_challenge::Column::GameId.eq(game_id))
        .filter(game_challenge::Column::IsEnabled.eq(true))
        .filter(game_challenge::Column::ReviewStatus.eq(ChallengeReviewStatus::Active))
        .filter(game_challenge::Column::ChallengeType.eq(ChallengeType::KingOfTheHill))
        .all(&st.db)
        .await?;
    let mut provisioned = 0u64;
    for c in &hills {
        // Keep target publication in the same critical section as shared
        // container reuse/creation. Teardown takes this lock too, so it cannot
        // destroy a backend that is republished immediately afterward.
        let flight_key = format!("shared-container:{}", c.id);
        let _flight = crate::utils::single_flight::coalesce(&flight_key).await;
        let distributed =
            crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &flight_key)
                .await?;
        let game_exists = game::Entity::find()
            .filter(game::Column::Id.eq(game_id))
            .filter(game::Column::EndTimeUtc.gte(Utc::now()))
            .one(&st.db)
            .await?
            .is_some();
        if !game_exists {
            distributed.release().await?;
            continue;
        }
        let Some(c) = game_challenge::Entity::find()
            .filter(game_challenge::Column::Id.eq(c.id))
            .filter(game_challenge::Column::GameId.eq(game_id))
            .filter(game_challenge::Column::IsEnabled.eq(true))
            .filter(game_challenge::Column::ReviewStatus.eq(ChallengeReviewStatus::Active))
            .filter(game_challenge::Column::ChallengeType.eq(ChallengeType::KingOfTheHill))
            .one(&st.db)
            .await?
        else {
            distributed.release().await?;
            continue;
        };
        let existing = koth_target::Entity::find()
            .filter(koth_target::Column::GameId.eq(game_id))
            .filter(koth_target::Column::ChallengeId.eq(c.id))
            .one(&st.db)
            .await?;
        // Skip only if the hill's container is actually ALIVE. A dead container
        // (docker reaped / crashed) must be recreated, not left as a stale endpoint
        // that reads Offline forever — get_or_create_shared_container below detects
        // the dead one and recreates it.
        let liveness = match existing
            .as_ref()
            .and_then(|t| t.container_id.as_deref())
            .filter(|x| !x.is_empty())
        {
            Some(container_id) => match st.containers.inspect_liveness(container_id).await {
                Ok(crate::services::container::ContainerLiveness::Running) => Some(true),
                Ok(crate::services::container::ContainerLiveness::Stopped) => Some(false),
                Ok(crate::services::container::ContainerLiveness::Unknown) => {
                    tracing::warn!(
                        challenge = c.id,
                        backend_id = container_id,
                        "ensure_koth_hills: backend is transitional; retaining publication"
                    );
                    distributed.release().await?;
                    continue;
                }
                Err(error) => {
                    tracing::warn!(
                        challenge = c.id,
                        backend_id = container_id,
                        %error,
                        "ensure_koth_hills: backend liveness is unknown; retaining publication"
                    );
                    distributed.release().await?;
                    continue;
                }
            },
            None => None,
        };
        if liveness == Some(true) {
            let backend_id = existing
                .as_ref()
                .and_then(|target| target.container_id.as_deref())
                .expect("alive KotH target has a backend id");
            if super::super::containers::refresh_shared_container_lease_locked(st, backend_id)
                .await?
            {
                distributed.release().await?;
                continue; // hill container is up and its managed lease is refreshed
            }
        }
        if let Some(target) = existing
            .as_ref()
            .filter(|target| target.container_id.is_some())
        {
            let backend_id = target
                .container_id
                .as_deref()
                .expect("filtered KotH target has a backend id");
            if !clear_target_for_replacement(
                st,
                target.id,
                game_id,
                c.id,
                backend_id,
                liveness == Some(false),
            )
            .await?
            {
                tracing::debug!(
                    challenge = c.id,
                    backend_id,
                    "ensure_koth_hills: replacement awaits matching dead-container evidence"
                );
                distributed.release().await?;
                continue;
            }
            crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
        }

        // Platform-hosted hills get the single shared container every team races
        // to control; a container-less hill falls back to the deterministic token.
        let has_image = c
            .container_image
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        let launched = if has_image {
            match super::super::containers::get_or_create_shared_container_locked(st, &c).await {
                Ok(container) => Some(container),
                Err(e) => {
                    tracing::warn!(challenge = c.id, error = %e, "ensure_koth_hills: hill container launch failed");
                    None
                }
            }
        } else {
            None
        };
        let (host, port, container_id) = launched.as_ref().map_or_else(
            || (String::new(), 0, None),
            |container| {
                (
                    container
                        .public_ip
                        .clone()
                        .filter(|value| !value.is_empty())
                        .unwrap_or_else(|| container.ip.clone()),
                    container.public_port.unwrap_or(container.port),
                    Some(container.container_id.as_str()),
                )
            },
        );
        if !publish_replacement_target(st, game_id, c.id, &host, port, container_id).await? {
            distributed.release().await?;
            drop(_flight);
            if let Some(container) = launched {
                let _ =
                    super::super::containers::destroy_managed_container_row(st, &container, false)
                        .await;
            }
            continue;
        };

        provisioned += 1;
        crate::services::ad_vpn::reconcile_for_deployment(&st.db).await?;
        distributed.release().await?;
    }
    Ok(provisioned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Connection;

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn held_target_clear_requires_current_identity_bound_receipt() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
        let mut connection = sqlx::PgConnection::connect(&database_url).await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "Games" (
              id INTEGER PRIMARY KEY, start_time_utc TIMESTAMPTZ NOT NULL,
              end_time_utc TIMESTAMPTZ NOT NULL, ad_scoring_paused BOOLEAN NOT NULL,
              koth_scoring_start_round INTEGER
            );
            CREATE TEMP TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, is_enabled BOOLEAN NOT NULL,
              review_status SMALLINT NOT NULL, "Type" SMALLINT NOT NULL
            );
            CREATE TEMP TABLE "KothTargets" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
              host TEXT NOT NULL, port INTEGER NOT NULL, container_id TEXT,
              holder_participation_id INTEGER, held_since TIMESTAMPTZ
            );
            CREATE TEMP TABLE "KothControlResults" (
              id SERIAL PRIMARY KEY, game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
              status SMALLINT NOT NULL,
              responsible_participation_id INTEGER, dead_container_id TEXT,
              checked_at TIMESTAMPTZ NOT NULL
            );
            "#,
        )
        .execute(&mut connection)
        .await
        .unwrap();

        let now = Utc::now();
        sqlx::query(r#"INSERT INTO "Games" VALUES (1, $1, $2, FALSE, 1)"#)
            .bind(now - chrono::Duration::minutes(5))
            .bind(now + chrono::Duration::minutes(5))
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "GameChallenges" VALUES (2, 1, TRUE, $1, $2)"#)
            .bind(ChallengeReviewStatus::Active as i16)
            .bind(ChallengeType::KingOfTheHill as i16)
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "KothTargets"
                 VALUES (3, 1, 2, '10.0.0.2', 8080, 'backend-a', 7, $1)"#,
        )
        .bind(now - chrono::Duration::seconds(10))
        .execute(&mut connection)
        .await
        .unwrap();

        assert!(
            !clear_target_for_replacement_locked(&mut connection, 3, 1, 2, "backend-a", true)
                .await
                .unwrap()
        );
        sqlx::query(
            r#"INSERT INTO "KothControlResults"
                 (game_id, challenge_id, status, responsible_participation_id,
                  dead_container_id, checked_at)
               VALUES (1, 2, $1, 7, 'backend-a', $2)"#,
        )
        .bind(crate::utils::enums::AdCheckStatus::Offline as i16)
        .bind(now - chrono::Duration::minutes(1))
        .execute(&mut connection)
        .await
        .unwrap();
        assert!(
            !clear_target_for_replacement_locked(&mut connection, 3, 1, 2, "backend-a", true)
                .await
                .unwrap()
        );

        sqlx::query(r#"UPDATE "KothControlResults" SET checked_at = $1"#)
            .bind(now)
            .execute(&mut connection)
            .await
            .unwrap();
        assert!(
            !clear_target_for_replacement_locked(&mut connection, 3, 1, 2, "backend-a", false)
                .await
                .unwrap()
        );
        sqlx::query(
            r#"INSERT INTO "KothControlResults"
                 (game_id, challenge_id, status, responsible_participation_id,
                  dead_container_id, checked_at)
               VALUES (1, 2, $1, 7, NULL, $2)"#,
        )
        .bind(crate::utils::enums::AdCheckStatus::Ok as i16)
        .bind(now + chrono::Duration::seconds(1))
        .execute(&mut connection)
        .await
        .unwrap();
        assert!(
            !clear_target_for_replacement_locked(&mut connection, 3, 1, 2, "backend-a", true)
                .await
                .unwrap()
        );
        sqlx::query(r#"DELETE FROM "KothControlResults" WHERE dead_container_id IS NULL"#)
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(r#"UPDATE "Games" SET end_time_utc = $1 WHERE id = 1"#)
            .bind(now - chrono::Duration::seconds(1))
            .execute(&mut connection)
            .await
            .unwrap();
        assert!(
            !clear_target_for_replacement_locked(&mut connection, 3, 1, 2, "backend-a", true)
                .await
                .unwrap()
        );
        sqlx::query(r#"UPDATE "Games" SET end_time_utc = $1 WHERE id = 1"#)
            .bind(now + chrono::Duration::minutes(5))
            .execute(&mut connection)
            .await
            .unwrap();
        assert!(
            clear_target_for_replacement_locked(&mut connection, 3, 1, 2, "backend-a", true)
                .await
                .unwrap()
        );
        let cleared: (String, i32, Option<String>, Option<i32>) = sqlx::query_as(
            r#"SELECT host, port, container_id, holder_participation_id
                 FROM "KothTargets" WHERE id = 3"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(cleared, (String::new(), 0, None, None));
    }
}
