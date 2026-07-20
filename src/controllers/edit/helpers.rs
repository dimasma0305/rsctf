//! edit: shared query helpers (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;

pub(crate) async fn load_game(st: &SharedState, id: i32) -> AppResult<game::Model> {
    game::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Game not found"))
}

/// Lock the parent event in a caller-owned transaction and reject every child
/// mutation after hard deletion reaches its durable point of no return.
/// Callers also own the per-game advisory fence, so this snapshot and their
/// child write linearize against both deletion phases on every replica.
pub(crate) async fn require_game_mutable(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
) -> AppResult<()> {
    let deletion_pending = sqlx::query_scalar::<_, bool>(
        r#"SELECT deletion_pending FROM "Games" WHERE id = $1 FOR SHARE"#,
    )
    .bind(game_id)
    .fetch_optional(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    match deletion_pending {
        None => Err(AppError::not_found("Game not found")),
        Some(true) => Err(AppError::conflict("Game is being deleted")),
        Some(false) => Ok(()),
    }
}

#[cfg(test)]
mod mutable_game_tests {
    use std::str::FromStr;
    use std::time::Duration;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::require_game_mutable;

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn committed_and_inflight_game_deletion_fences_reject_child_mutations() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_game_mutable_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(3)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TABLE "Games" (
                 id INTEGER PRIMARY KEY,
                 deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
               );
               INSERT INTO "Games" VALUES (1, FALSE), (2, TRUE);"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let mut committed = pool.acquire().await.unwrap();
        let error = require_game_mutable(&mut committed, 2)
            .await
            .expect_err("child mutation crossed a committed game-deletion fence");
        assert_eq!(error.status(), axum::http::StatusCode::CONFLICT);

        let mut deletion = pool.begin().await.unwrap();
        sqlx::query(r#"UPDATE "Games" SET deletion_pending = TRUE WHERE id = 1"#)
            .execute(&mut *deletion)
            .await
            .unwrap();
        let second_pool = pool.clone();
        let mut child = tokio::spawn(async move {
            let mut transaction = second_pool.begin().await.unwrap();
            let result = require_game_mutable(&mut transaction, 1).await;
            transaction.rollback().await.unwrap();
            result
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut child)
                .await
                .is_err(),
            "child mutation did not wait for the parent deletion decision"
        );
        deletion.commit().await.unwrap();
        let error = tokio::time::timeout(Duration::from_secs(2), child)
            .await
            .expect("child mutation remained blocked")
            .unwrap()
            .expect_err("child mutation crossed the newly committed deletion fence");
        assert_eq!(error.status(), axum::http::StatusCode::CONFLICT);

        drop(committed);
        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}

pub(crate) fn validate_koth_crown_shape(
    epoch_ticks: i32,
    cycle_ticks: i32,
    champion_cooldown_ticks: i32,
    claim_confirmation_ticks: i32,
) -> AppResult<()> {
    use crate::services::ad_engine::koth_cycle::CrownShapeError;

    let message = match crate::services::ad_engine::koth_cycle::validate_crown_shape(
        epoch_ticks,
        cycle_ticks,
        champion_cooldown_ticks,
        claim_confirmation_ticks,
    ) {
        Ok(()) => return Ok(()),
        Err(CrownShapeError::Epoch) => "KotH epoch ticks must be between 2 and 64.",
        Err(CrownShapeError::Cycle) => {
            "KotH cycle ticks must divide the KotH epoch into at least two cycles."
        }
        Err(CrownShapeError::ChampionCooldown) => {
            "KotH champion cooldown ticks must be between 0 and one less than the cycle length."
        }
        Err(CrownShapeError::ClaimConfirmation) => {
            "KotH claim confirmation ticks must be between 1 and the cycle length."
        }
    };
    Err(AppError::bad_request(message))
}

#[cfg(test)]
mod koth_crown_config_tests {
    use super::validate_koth_crown_shape;

    #[test]
    fn crown_cycle_defaults_and_boundaries_are_validated_together() {
        assert!(validate_koth_crown_shape(12, 3, 1, 2).is_ok());
        assert!(validate_koth_crown_shape(2, 1, 0, 1).is_ok());
        assert!(validate_koth_crown_shape(1, 1, 0, 1).is_err());
        assert!(validate_koth_crown_shape(12, 12, 1, 2).is_err());
        assert!(validate_koth_crown_shape(12, 5, 1, 2).is_err());
        assert!(validate_koth_crown_shape(12, 3, 3, 2).is_err());
        assert!(validate_koth_crown_shape(12, 3, 1, 4).is_err());
    }
}

/// Read the immutable official epoch boundary while the caller holds the shared
/// per-game A&D/KotH control lock. Every mutation that can change the scored
/// challenge set uses this helper before writing.
pub(crate) async fn ad_epoch_scoring_started_locked(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
) -> AppResult<bool> {
    sqlx::query_scalar::<_, bool>(
        r#"SELECT ad_scoring_start_round IS NOT NULL
                  OR koth_scoring_start_round IS NOT NULL
             FROM "Games" WHERE id = $1"#,
    )
    .bind(game_id)
    .fetch_optional(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))
}

pub(crate) fn ensure_ad_roster_status_mutable(
    scoring_started: bool,
    current: Option<ParticipationStatus>,
    requested: ParticipationStatus,
) -> AppResult<()> {
    let reversible_sanction = matches!(
        (current, requested),
        (
            Some(ParticipationStatus::Accepted),
            ParticipationStatus::Suspended
        ) | (
            Some(ParticipationStatus::Suspended),
            ParticipationStatus::Accepted
        )
    );
    if scoring_started && current != Some(requested) && !reversible_sanction {
        return Err(AppError::bad_request(
            "Only suspension and reinstatement can change participation status after A&D/KotH epoch scoring has started.",
        ));
    }
    Ok(())
}

pub(crate) async fn flush_ad_scoreboard(st: &SharedState, game_id: i32) {
    crate::controllers::game::ad::hard_invalidate_ad_scoreboard(st, game_id).await;
    crate::controllers::game::koth::invalidate_live_hill_cache(st.cache.as_ref(), game_id).await;
    st.cache.remove(&format!("_KothScoreBoard_{game_id}")).await;
    st.cache
        .remove(&format!("_KothScoreBoardFrozen_{game_id}"))
        .await;
    st.cache.remove(&format!("_KothTimeline_{game_id}")).await;
    st.cache
        .remove(&format!("_KothTimelineFrozen_{game_id}"))
        .await;
}

const AD_EVENT_CLOSEOUT_MESSAGE: &str =
    "checker pass did not complete before event-close grace expired";
const KOTH_EVENT_CLOSEOUT_MESSAGE: &str = "checker result unavailable; scoring sample void";

/// Reopen the latest round when an ended event is extended into the future.
///
/// Event closeout writes explicit synthetic evidence so an ended board can
/// settle. That evidence must become pending again when the same round resumes,
/// but a genuine checker sample is immutable and must survive the extension.
/// The caller holds the game-control and both scoring-rollup locks, and this
/// helper performs every mutation in that transaction.
pub(crate) async fn reopen_latest_round_for_end_extension(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    previous_end: DateTime<Utc>,
    next_end: DateTime<Utc>,
) -> AppResult<Option<i32>> {
    let round_id = sqlx::query_scalar::<_, i32>(
        r#"UPDATE "AdRounds" round SET finalized = FALSE
            WHERE round.id = (
                  SELECT latest.id FROM "AdRounds" latest
                   WHERE latest.game_id = $1
                   ORDER BY latest.number DESC, latest.id DESC LIMIT 1
            )
              AND round.finalized = TRUE
              AND EXISTS (
                  SELECT 1 FROM "Games" game
                   WHERE game.id = $1
                     AND game.end_time_utc = $2
                     AND game.end_time_utc <= clock_timestamp()
                     AND $3 > clock_timestamp()
              )
          RETURNING round.id"#,
    )
    .bind(game_id)
    .bind(previous_end)
    .bind(next_end)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(round_id) = round_id else {
        return Ok(None);
    };

    let reopened_ad = sqlx::query(
        r#"UPDATE "AdCheckResults" result
              SET message = 'checker not yet executed (event reopened)',
                  checked_at = round.start_time_utc,
                  sla_credit = NULL
             FROM "AdRounds" round
            WHERE result.round_id = round.id
              AND round.id = $1
              AND round.game_id = $2
              AND result.status = $3
              AND result.message = $4
              AND result.checked_at = $5
              AND result.sla_credit = 0.0
              AND result.flag_verified = FALSE"#,
    )
    .bind(round_id)
    .bind(game_id)
    .bind(crate::services::ad_engine::AdCheckStatus::InternalError as i16)
    .bind(AD_EVENT_CLOSEOUT_MESSAGE)
    .bind(previous_end)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();

    let reopened_koth = sqlx::query(
        r#"DELETE FROM "KothControlResults" result
            USING "AdRounds" round
            WHERE result.ad_round_id = round.id
              AND round.id = $1
              AND round.game_id = $2
              AND result.game_id = $2
              AND result.controlling_participation_id IS NULL
              AND result.marker_observed = FALSE
              AND result.status = $3
              AND result.error_message = $4
              AND result.checked_at = $5
              AND result.dead_container_id IS NULL"#,
    )
    .bind(round_id)
    .bind(game_id)
    .bind(crate::services::ad_engine::AdCheckStatus::InternalError as i16)
    .bind(KOTH_EVENT_CLOSEOUT_MESSAGE)
    .bind(previous_end)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();

    if reopened_ad > 0 || reopened_koth > 0 {
        sqlx::query(
            r#"UPDATE "AdRounds"
                  SET pipeline_completed_at = NULL,
                      pipeline_lease_token = NULL,
                      pipeline_lease_until = NULL
                WHERE id = $1 AND game_id = $2"#,
        )
        .bind(round_id)
        .bind(game_id)
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }

    Ok(Some(round_id))
}

pub(crate) async fn flush_game_scoreboards(st: &SharedState, game_id: i32) {
    st.cache.remove(&format!("_ScoreBoard_{game_id}")).await;
    st.cache
        .remove(&format!("_ScoreBoardFrozen_{game_id}"))
        .await;
    flush_ad_scoreboard(st, game_id).await;
}

#[cfg(test)]
mod roster_status_tests {
    use super::ensure_ad_roster_status_mutable;
    use crate::utils::enums::ParticipationStatus;

    #[test]
    fn official_roster_is_immutable_after_scoring_starts() {
        let statuses = [
            ParticipationStatus::Pending,
            ParticipationStatus::Accepted,
            ParticipationStatus::Rejected,
            ParticipationStatus::Suspended,
            ParticipationStatus::Unsubmitted,
        ];
        for current in statuses {
            for requested in statuses {
                let expected = current == requested
                    || matches!(
                        (current, requested),
                        (
                            ParticipationStatus::Accepted,
                            ParticipationStatus::Suspended
                        ) | (
                            ParticipationStatus::Suspended,
                            ParticipationStatus::Accepted
                        )
                    );
                assert_eq!(
                    ensure_ad_roster_status_mutable(true, Some(current), requested).is_ok(),
                    expected,
                    "unexpected active-roster transition {current:?} -> {requested:?}"
                );
            }
        }
        assert!(ensure_ad_roster_status_mutable(
            true,
            Some(ParticipationStatus::Accepted),
            ParticipationStatus::Rejected,
        )
        .is_err());
        assert!(
            ensure_ad_roster_status_mutable(true, None, ParticipationStatus::Accepted,).is_err()
        );
        assert!(ensure_ad_roster_status_mutable(
            true,
            Some(ParticipationStatus::Accepted),
            ParticipationStatus::Suspended,
        )
        .is_ok());
        assert!(ensure_ad_roster_status_mutable(
            true,
            Some(ParticipationStatus::Suspended),
            ParticipationStatus::Accepted,
        )
        .is_ok());
        assert!(ensure_ad_roster_status_mutable(
            true,
            Some(ParticipationStatus::Suspended),
            ParticipationStatus::Rejected,
        )
        .is_err());
        assert!(ensure_ad_roster_status_mutable(
            false,
            Some(ParticipationStatus::Pending),
            ParticipationStatus::Accepted,
        )
        .is_ok());
    }
}

#[cfg(test)]
mod reopen_round_tests {
    use super::{
        reopen_latest_round_for_end_extension, AD_EVENT_CLOSEOUT_MESSAGE,
        KOTH_EVENT_CLOSEOUT_MESSAGE,
    };
    use crate::services::ad_engine::AdCheckStatus;
    use chrono::{Duration, Utc};
    use sqlx::{Connection, PgConnection};

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn end_extension_reopens_only_synthetic_closeout_evidence() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
        let mut connection = PgConnection::connect(&database_url).await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "Games" (
              id INTEGER PRIMARY KEY, end_time_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TEMP TABLE "AdRounds" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, number INTEGER NOT NULL,
              start_time_utc TIMESTAMPTZ NOT NULL, finalized BOOLEAN NOT NULL,
              pipeline_completed_at TIMESTAMPTZ,
              pipeline_lease_token TEXT, pipeline_lease_until TIMESTAMPTZ
            );
            CREATE TEMP TABLE "AdCheckResults" (
              id INTEGER PRIMARY KEY, round_id INTEGER NOT NULL, status SMALLINT NOT NULL,
              message TEXT, checked_at TIMESTAMPTZ NOT NULL,
              sla_credit DOUBLE PRECISION, flag_verified BOOLEAN NOT NULL
            );
            CREATE TEMP TABLE "KothControlResults" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
              ad_round_id INTEGER NOT NULL, controlling_participation_id INTEGER,
              marker_observed BOOLEAN NOT NULL, status SMALLINT NOT NULL,
              error_message TEXT, checked_at TIMESTAMPTZ NOT NULL, dead_container_id TEXT
            );
            "#,
        )
        .execute(&mut connection)
        .await
        .unwrap();

        let now = Utc::now();
        let previous_end = now - Duration::minutes(1);
        let next_end = now + Duration::hours(1);
        let round_start = previous_end - Duration::minutes(5);
        sqlx::query(r#"INSERT INTO "Games" VALUES (1, $1), (2, $1)"#)
            .bind(previous_end)
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "AdRounds" VALUES
                 (10, 1, 7, $1, TRUE, $2, 'old-lease', $3),
                 (20, 2, 9, $1, TRUE, $2, NULL, NULL)"#,
        )
        .bind(round_start)
        .bind(previous_end)
        .bind(next_end)
        .execute(&mut connection)
        .await
        .unwrap();

        sqlx::query(
            r#"INSERT INTO "AdCheckResults" VALUES
                 (1, 10, $1, $2, $3, 0.0, FALSE),
                 (2, 10, $1, 'genuine checker failure', $3, 0.0, FALSE),
                 (3, 10, $4, NULL, $5, 1.0, TRUE),
                 (4, 20, $4, NULL, $5, 1.0, TRUE)"#,
        )
        .bind(AdCheckStatus::InternalError as i16)
        .bind(AD_EVENT_CLOSEOUT_MESSAGE)
        .bind(previous_end)
        .bind(AdCheckStatus::Ok as i16)
        .bind(previous_end - Duration::seconds(1))
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "KothControlResults" VALUES
                 (1, 1, 101, 10, NULL, FALSE, $1, $2, $3, NULL),
                 (2, 1, 102, 10, NULL, FALSE, $1,
                    'genuine checker failure', $3, NULL),
                 (3, 1, 103, 10, 7, TRUE, $4, NULL, $5, NULL),
                 (4, 2, 201, 20, 7, TRUE, $4, NULL, $5, NULL)"#,
        )
        .bind(AdCheckStatus::InternalError as i16)
        .bind(KOTH_EVENT_CLOSEOUT_MESSAGE)
        .bind(previous_end)
        .bind(AdCheckStatus::Ok as i16)
        .bind(previous_end - Duration::seconds(1))
        .execute(&mut connection)
        .await
        .unwrap();

        assert_eq!(
            reopen_latest_round_for_end_extension(&mut connection, 1, previous_end, next_end,)
                .await
                .unwrap(),
            Some(10)
        );
        let round: (bool, Option<chrono::DateTime<Utc>>, Option<String>) = sqlx::query_as(
            r#"SELECT finalized, pipeline_completed_at, pipeline_lease_token
                 FROM "AdRounds" WHERE id = 10"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(round, (false, None, None));

        let checks: Vec<(i32, Option<String>, Option<f64>)> = sqlx::query_as(
            r#"SELECT id, message, sla_credit FROM "AdCheckResults"
                WHERE round_id = 10 ORDER BY id"#,
        )
        .fetch_all(&mut connection)
        .await
        .unwrap();
        assert_eq!(
            checks[0].1.as_deref(),
            Some("checker not yet executed (event reopened)")
        );
        assert_eq!(checks[0].2, None);
        assert_eq!(checks[1].1.as_deref(), Some("genuine checker failure"));
        assert_eq!(checks[1].2, Some(0.0));
        assert_eq!(checks[2].2, Some(1.0));
        let koth_ids: Vec<i32> = sqlx::query_scalar(
            r#"SELECT id FROM "KothControlResults" WHERE ad_round_id = 10 ORDER BY id"#,
        )
        .fetch_all(&mut connection)
        .await
        .unwrap();
        assert_eq!(koth_ids, vec![2, 3]);

        // A round containing only genuine evidence is reopened for submissions,
        // but its completed pipeline and immutable samples do not get replayed.
        assert_eq!(
            reopen_latest_round_for_end_extension(&mut connection, 2, previous_end, next_end,)
                .await
                .unwrap(),
            Some(20)
        );
        let genuine_round: (bool, Option<chrono::DateTime<Utc>>) = sqlx::query_as(
            r#"SELECT finalized, pipeline_completed_at FROM "AdRounds" WHERE id = 20"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert!(!genuine_round.0);
        assert!(genuine_round.1.is_some());
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"SELECT COUNT(*) FROM "AdCheckResults" WHERE round_id = 20"#,
            )
            .fetch_one(&mut connection)
            .await
            .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"SELECT COUNT(*) FROM "KothControlResults" WHERE ad_round_id = 20"#,
            )
            .fetch_one(&mut connection)
            .await
            .unwrap(),
            1
        );
    }
}

pub(crate) async fn load_challenge(
    st: &SharedState,
    game_id: i32,
    c_id: i32,
) -> AppResult<game_challenge::Model> {
    game_challenge::Entity::find()
        .filter(game_challenge::Column::Id.eq(c_id))
        .filter(game_challenge::Column::GameId.eq(game_id))
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Challenge not found"))
}

/// Ensure every division of `game_id` has a per-challenge config row for
/// `challenge_id`. Insert-if-missing seeded with the division's default
/// permissions — never clobbers an existing (possibly admin-tuned) row.
pub(crate) async fn seed_division_configs(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<()> {
    let divisions = division::Entity::find()
        .filter(division::Column::GameId.eq(game_id))
        .all(&st.db)
        .await?;
    for div in divisions {
        let existing = division_challenge_config::Entity::find_by_id((div.id, challenge_id))
            .one(&st.db)
            .await?;
        if existing.is_none() {
            let am = division_challenge_config::ActiveModel {
                division_id: Set(div.id),
                challenge_id: Set(challenge_id),
                permissions: Set(div.default_permissions),
            };
            am.insert(&st.db).await?;
        }
    }
    Ok(())
}

/// Batch-resolve challenge id -> title (skips the query on an empty id list to
/// avoid a degenerate `IN ()`).
pub(crate) async fn load_challenge_titles(
    st: &SharedState,
    ids: Vec<i32>,
) -> AppResult<std::collections::HashMap<i32, String>> {
    if ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let map = game_challenge::Entity::find()
        .filter(game_challenge::Column::Id.is_in(ids))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|c| (c.id, c.title))
        .collect();
    Ok(map)
}

/// Batch-resolve user id -> username (skips the query on an empty id list).
pub(crate) async fn load_user_names(
    st: &SharedState,
    ids: Vec<Uuid>,
) -> AppResult<std::collections::HashMap<Uuid, String>> {
    if ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let map = user::Entity::find()
        .filter(user::Column::Id.is_in(ids))
        .all(&st.db)
        .await?
        .into_iter()
        .filter_map(|u| u.user_name.map(|n| (u.id, n)))
        .collect();
    Ok(map)
}

/// Delete an attachment row and, for a local attachment, release its blob.
/// Mirrors RSCTF `BlobRepository.DeleteAttachment`. Idempotent when the row is
/// already gone.
pub(crate) async fn delete_attachment(st: &SharedState, attachment_id: i32) -> AppResult<()> {
    let deleted_hash =
        crate::services::blob_refs::delete_attachment(st.pg(), attachment_id).await?;
    if let Some(hash) = deleted_hash {
        if let Err(error) =
            crate::services::blob_refs::purge_if_unreferenced(st.pg(), st.storage.as_ref(), &hash)
                .await
        {
            tracing::warn!(%error, %hash, "deleted attachment blob purge failed");
        }
    }
    Ok(())
}

pub(super) async fn revoke_and_destroy_backend(
    st: &SharedState,
    backend_id: &str,
) -> AppResult<()> {
    let game_ids = crate::services::ad_vpn::stage_backend_endpoint_deactivation_retaining_identity(
        &st.db, backend_id,
    )
    .await?;
    for game_id in game_ids {
        crate::controllers::game::ad::invalidate_live_hill_snapshot(st, game_id).await;
    }
    crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
    crate::services::traffic::destroy_container_after_capture_fence(st, backend_id).await
}

/// Fail-closed teardown of every backend container the game owns: per-team
/// instance containers, service-owned inspectors, and per-challenge
/// test/shared containers. Durable
/// owners remain attached until each backend has been fenced and destroyed, so
/// a failed hard deletion is exactly retryable instead of leaking a runtime.
pub(crate) async fn destroy_game_containers(st: &SharedState, game_id: i32) -> AppResult<()> {
    let mut container_ids: Vec<Uuid> = sqlx::query_scalar(
        r#"SELECT container.id
             FROM "Containers" container
             JOIN "AdTeamServices" service
               ON service.id = container.ad_team_service_id
            WHERE service.game_id = $1
            ORDER BY container.id"#,
    )
    .bind(game_id)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    // The game-end marker is durable before this helper runs. Drain every
    // first-publication transaction before snapshotting service rows; existing
    // rows are then serialized by their narrower per-pair locks below.
    crate::services::ad::service_lifecycle::drain_publications(st.pg(), [game_id]).await?;
    let ad_services = ad_team_service::Entity::find()
        .filter(ad_team_service::Column::GameId.eq(game_id))
        .all(&st.db)
        .await?;
    let koth_backends: Vec<(i32, String)> = koth_target::Entity::find()
        .filter(koth_target::Column::GameId.eq(game_id))
        .all(&st.db)
        .await?
        .into_iter()
        .filter_map(|target| {
            target
                .container_id
                .map(|backend_id| (target.challenge_id, backend_id))
        })
        .collect();
    for service in ad_services {
        let key = crate::services::ad::service_lifecycle::service_lock_key(
            service.participation_id,
            service.challenge_id,
        );
        let _local = crate::utils::single_flight::coalesce(&key).await;
        let distributed =
            crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &key)
                .await?;
        let teardown =
            crate::services::ad::service_lifecycle::destroy_persisted_service(st, service.id).await;
        let released = distributed.release().await;
        teardown?;
        released?;
    }

    // Per-team instance containers, reached via the game's participations.
    let part_ids: Vec<i32> = participation::Entity::find()
        .filter(participation::Column::GameId.eq(game_id))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|p| p.id)
        .collect();
    if !part_ids.is_empty() {
        let instances = game_instance::Entity::find()
            .filter(game_instance::Column::ParticipationId.is_in(part_ids))
            .all(&st.db)
            .await?;
        container_ids.extend(instances.into_iter().filter_map(|i| i.container_id));
    }

    // Per-challenge test + shared containers.
    let challenges = game_challenge::Entity::find()
        .filter(game_challenge::Column::GameId.eq(game_id))
        .all(&st.db)
        .await?;
    for c in challenges {
        container_ids.extend(c.test_container_id);
        container_ids.extend(c.shared_container_id);
    }

    let mut destroyed_backends = std::collections::HashSet::new();
    for cid in container_ids {
        if let Some(c) = container::Entity::find_by_id(cid).one(&st.db).await? {
            if crate::controllers::game::destroy_managed_container_row(st, &c, false).await? {
                destroyed_backends.insert(c.container_id);
            }
        }
    }

    // Untracked or damaged KotH targets may lack their Containers bookkeeping row.
    // Still serialize their endpoint revocation and backend destroy with hill
    // provisioning so the stale target cannot be republished afterward.
    for (challenge_id, backend_id) in koth_backends {
        if destroyed_backends.contains(&backend_id) {
            continue;
        }
        let key = format!("shared-container:{challenge_id}");
        let _local = crate::utils::single_flight::coalesce(&key).await;
        let distributed =
            crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &key)
                .await?;
        let teardown = async {
            revoke_and_destroy_backend(st, &backend_id).await?;
            sqlx::query(
                r#"UPDATE "KothTargets"
                      SET container_id = NULL
                    WHERE game_id = $1 AND challenge_id = $2
                      AND container_id = $3
                      AND NULLIF(BTRIM(host), '') IS NULL AND port = 0"#,
            )
            .bind(game_id)
            .bind(challenge_id)
            .bind(&backend_id)
            .execute(st.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            AppResult::Ok(())
        }
        .await;
        let released = distributed.release().await;
        teardown?;
        released?;
    }
    Ok(())
}

pub(super) async fn destroy_test_container_with<F>(
    pool: &sqlx::PgPool,
    challenge_id: i32,
    container_id: Uuid,
    backend_id: &str,
    destroy: F,
) -> AppResult<()>
where
    F: std::future::Future<Output = AppResult<()>>,
{
    destroy.await?;
    sqlx::query(
        r#"WITH owner AS (
             SELECT id FROM "GameChallenges"
              WHERE id = $1 AND test_container_id = $2
           ), removed AS (
             DELETE FROM "Containers" container USING owner
              WHERE container.id = $2 AND container.container_id = $3
              RETURNING container.id
           )
           UPDATE "GameChallenges"
              SET test_container_id = NULL
            WHERE id = $1 AND test_container_id IN (SELECT id FROM removed)"#,
    )
    .bind(challenge_id)
    .bind(container_id)
    .bind(backend_id)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

/// Final test-container sweep performed while `test-containers-game:{game_id}`
/// is held. The earlier generic teardown can race a test creation; this fresh
/// query is the deletion barrier that catches anything published before the
/// game-scoped gate was acquired.
pub(crate) async fn destroy_game_test_containers_locked(
    st: &SharedState,
    game_id: i32,
) -> AppResult<()> {
    let challenges = game_challenge::Entity::find()
        .filter(game_challenge::Column::GameId.eq(game_id))
        .all(&st.db)
        .await?;
    for challenge in challenges {
        let Some(container_id) = challenge.test_container_id else {
            continue;
        };
        if let Some(container) = container::Entity::find_by_id(container_id)
            .one(&st.db)
            .await?
        {
            destroy_test_container_with(
                st.pg(),
                challenge.id,
                container_id,
                &container.container_id,
                revoke_and_destroy_backend(st, &container.container_id),
            )
            .await?;
            continue;
        }
        sqlx::query(
            r#"UPDATE "GameChallenges"
                  SET test_container_id = NULL
                WHERE id = $1 AND test_container_id = $2"#,
        )
        .bind(challenge.id)
        .bind(container_id)
        .execute(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "helpers_teardown_tests.rs"]
mod teardown_tests;

pub(crate) async fn load_flags(st: &SharedState, c_id: i32) -> AppResult<Vec<FlagInfoModel>> {
    let flags = flag_context::Entity::find()
        .filter(flag_context::Column::ChallengeId.eq(c_id))
        .all(&st.db)
        .await?;
    let mut out = Vec::with_capacity(flags.len());
    for f in flags {
        let attachment = match f.attachment_id {
            Some(aid) => match attachment::Entity::find_by_id(aid).one(&st.db).await? {
                Some(a) => {
                    let file = match a.local_file_id {
                        Some(fid) => local_file::Entity::find_by_id(fid).one(&st.db).await?,
                        None => None,
                    };
                    Some(AttachmentInfoModel::from_attachment(&a, file.as_ref()))
                }
                None => None,
            },
            None => None,
        };
        out.push(FlagInfoModel {
            id: f.id,
            flag: f.flag,
            attachment,
        });
    }
    Ok(out)
}
