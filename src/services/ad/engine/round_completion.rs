//! Durable ownership and recovery for the external round-finishing pipeline.

use super::*;

/// Upper recovery bound for long ticks. The SQL lease is also capped at one
/// second after the persisted round deadline, so a crashed owner cannot stall
/// later rounds for this full duration.
const ROUND_FINISH_LEASE_SECONDS: i32 = 300;

/// Stable for this process lifetime, unique across replicas and restarts. It
/// lets graceful shutdown release only leases owned by this replica after all
/// of its round workers have stopped. A hard crash still relies on the bounded
/// SQL lease timeout.
static PROCESS_ROUND_FINISH_OWNER: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| uuid::Uuid::new_v4().to_string());

#[derive(Debug)]
pub(crate) struct RoundFinishLease {
    token: String,
}

/// Lock the round row and prove that this transaction still belongs to the
/// live pipeline owner. The shared row lock serializes with claim, abandon, and
/// graceful process cleanup updates, so no fenced checker writer can commit
/// after ownership is cleared or handed to another replica.
pub(crate) async fn lock_owned_round_finish(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    game_id: i32,
    round_id: i32,
    lease: &RoundFinishLease,
) -> AppResult<()> {
    let owned = sqlx::query_scalar::<_, i32>(
        r#"SELECT id
             FROM "AdRounds"
            WHERE id = $1 AND game_id = $2
              AND finalized = FALSE
              AND pipeline_completed_at IS NULL
              AND pipeline_lease_token = $3
              AND pipeline_lease_until > clock_timestamp()
            FOR SHARE"#,
    )
    .bind(round_id)
    .bind(game_id)
    .bind(&lease.token)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if owned.is_none() {
        return Err(AppError::conflict(
            "Round-finishing lease was lost before checker persistence",
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) enum RoundFinishDisposition {
    Complete,
    Claimed(RoundFinishLease),
    InFlight,
}

/// Claim one prepared round's external work across every replica.
///
/// A crashed owner leaves a time-bounded lease; a later cron pass can then replay
/// flag delivery and checking, both of which persist through idempotent keys.
pub(crate) async fn claim_round_finish(
    db: &DatabaseConnection,
    game_id: i32,
    round_id: i32,
) -> AppResult<RoundFinishDisposition> {
    let token = format!(
        "{}:{}",
        PROCESS_ROUND_FINISH_OWNER.as_str(),
        uuid::Uuid::new_v4()
    );
    let row: Option<(bool, Option<String>)> = sqlx::query_as(
        r#"WITH target AS (
             SELECT id, pipeline_completed_at, end_time_utc
               FROM "AdRounds"
              WHERE id = $1 AND game_id = $2 AND finalized = FALSE
           ), claimed AS (
             UPDATE "AdRounds" round
                SET pipeline_lease_token = $3,
                    pipeline_lease_until = LEAST(
                      clock_timestamp() + ($4 * interval '1 second'),
                      GREATEST(
                        target.end_time_utc + interval '1 second',
                        clock_timestamp() + interval '5 seconds'
                      )
                    )
               FROM target
              WHERE round.id = target.id
                AND target.pipeline_completed_at IS NULL
                AND (
                  round.pipeline_lease_until IS NULL
                  OR round.pipeline_lease_until <= clock_timestamp()
                )
             RETURNING round.pipeline_lease_token
           )
           SELECT target.pipeline_completed_at IS NOT NULL,
                  claimed.pipeline_lease_token
             FROM target
             LEFT JOIN claimed ON TRUE"#,
    )
    .bind(round_id)
    .bind(game_id)
    .bind(&token)
    .bind(ROUND_FINISH_LEASE_SECONDS)
    .fetch_optional(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    match row {
        None => Err(AppError::not_found("Prepared round not found")),
        Some((true, _)) => Ok(RoundFinishDisposition::Complete),
        Some((false, Some(claimed))) if claimed == token => {
            Ok(RoundFinishDisposition::Claimed(RoundFinishLease { token }))
        }
        Some((false, _)) => Ok(RoundFinishDisposition::InFlight),
    }
}

/// Release every unfinished pipeline lease owned by this process.
///
/// The composition root calls this only after it has stopped and joined (or
/// aborted and joined) every background worker. At that point no local checker
/// can still commit under one of these tokens, so another replica may safely
/// recover immediately instead of waiting for the crash-recovery timeout.
pub async fn abandon_process_round_finishes(db: &DatabaseConnection) -> AppResult<u64> {
    let result = sqlx::query(
        r#"UPDATE "AdRounds"
              SET pipeline_lease_token = NULL,
                  pipeline_lease_until = NULL
            WHERE pipeline_completed_at IS NULL
              AND split_part(pipeline_lease_token, ':', 1) = $1"#,
    )
    .bind(PROCESS_ROUND_FINISH_OWNER.as_str())
    .execute(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(result.rows_affected())
}

/// Mark external work complete only while this caller still owns the lease.
pub(crate) async fn complete_round_finish(
    db: &DatabaseConnection,
    game_id: i32,
    round_id: i32,
    lease: &RoundFinishLease,
) -> AppResult<()> {
    let updated = sqlx::query(
        r#"UPDATE "AdRounds"
              SET pipeline_completed_at = clock_timestamp(),
                  pipeline_lease_token = NULL,
                  pipeline_lease_until = NULL
            WHERE id = $1
              AND game_id = $2
              AND finalized = FALSE
              AND pipeline_completed_at IS NULL
              AND pipeline_lease_token = $3"#,
    )
    .bind(round_id)
    .bind(game_id)
    .bind(&lease.token)
    .execute(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if updated.rows_affected() != 1 {
        return Err(AppError::conflict(
            "Round-finishing lease was lost before completion",
        ));
    }
    Ok(())
}

/// Release a still-owned lease after an ordinary, observed pipeline error so
/// the next scheduler tick can retry immediately. A process crash cannot call
/// this path and remains protected by the lease timeout.
pub(crate) async fn abandon_round_finish(
    db: &DatabaseConnection,
    game_id: i32,
    round_id: i32,
    lease: &RoundFinishLease,
) -> AppResult<()> {
    sqlx::query(
        r#"UPDATE "AdRounds"
              SET pipeline_lease_token = NULL,
                  pipeline_lease_until = NULL
            WHERE id = $1 AND game_id = $2
              AND pipeline_completed_at IS NULL
              AND pipeline_lease_token = $3"#,
    )
    .bind(round_id)
    .bind(game_id)
    .bind(&lease.token)
    .execute(db.get_postgres_connection_pool())
    .await
    .map(|_| ())
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Fence and settle a pipeline that missed its authoritative round deadline.
///
/// Completed probe batches win if they committed before this fence. Remaining
/// A&D placeholders and missing KotH samples become platform-attributed voids;
/// late writers are then rejected by the non-NULL SLA rows / immutable KotH
/// unique key. Clearing the lease lets the next scheduled round open without
/// inheriting checker latency from the prior tick.
pub(crate) async fn expire_overdue_round_finish(
    db: &DatabaseConnection,
    game_id: i32,
    round_id: i32,
    lease: &RoundFinishLease,
) -> AppResult<bool> {
    let mut control = super::koth_auth::acquire_game_lock(db, game_id).await?;
    let owned_overdue: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
             SELECT 1 FROM "AdRounds" round
              WHERE round.id = $1 AND round.game_id = $2
                AND round.finalized = FALSE
                AND round.pipeline_completed_at IS NULL
                AND round.pipeline_lease_token = $3
                AND round.end_time_utc <= clock_timestamp()
                AND round.id = (
                  SELECT latest.id FROM "AdRounds" latest
                   WHERE latest.game_id = round.game_id
                   ORDER BY latest.number DESC, latest.id DESC LIMIT 1
                )
           )"#,
    )
    .bind(round_id)
    .bind(game_id)
    .bind(&lease.token)
    .fetch_one(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !owned_overdue {
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(false);
    }
    // A deadline can interrupt publication before the ordinary per-service
    // receipts are committed. Seal every missing delivery first so each
    // unresolved checker placeholder is an explicit personal platform void.
    super::flag_delivery::complete_missing_flag_delivery_outcomes_transaction(
        control.transaction_mut(),
        game_id,
        round_id,
    )
    .await?;
    super::persistence::complete_unresolved_check_results_transaction(
        control.transaction_mut(),
        game_id,
        round_id,
    )
    .await?;
    super::persistence::complete_missing_koth_results_transaction(
        control.transaction_mut(),
        game_id,
        round_id,
    )
    .await?;
    let updated = sqlx::query(
        r#"UPDATE "AdRounds" round
              SET pipeline_completed_at = COALESCE(pipeline_completed_at, clock_timestamp()),
                  pipeline_lease_token = NULL,
                  pipeline_lease_until = NULL
            WHERE id = $1 AND game_id = $2 AND finalized = FALSE
              AND pipeline_completed_at IS NULL
              AND pipeline_lease_token = $3
              AND end_time_utc <= clock_timestamp()"#,
    )
    .bind(round_id)
    .bind(game_id)
    .bind(&lease.token)
    .execute(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let expired = updated.rows_affected() == 1;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(expired)
}

/// Rebuild the immutable preparation snapshot after a process restart.
pub(crate) async fn prepared_round_snapshot(
    db: &DatabaseConnection,
    game_id: i32,
    round_id: i32,
) -> AppResult<AdvancedRound> {
    let round: Option<(i32, i32, chrono::DateTime<Utc>, chrono::DateTime<Utc>)> = sqlx::query_as(
        r#"SELECT id, number, start_time_utc, end_time_utc
             FROM "AdRounds"
            WHERE id = $1 AND game_id = $2 AND finalized = FALSE"#,
    )
    .bind(round_id)
    .bind(game_id)
    .fetch_optional(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let (id, number, started_at, ends_at) =
        round.ok_or_else(|| AppError::not_found("Prepared round not found"))?;

    let flags = sqlx::query_as::<_, (i32, i32, i32, bool, Option<String>, String)>(
        r#"SELECT service.id, service.participation_id, service.challenge_id,
                  NOT challenge.ad_self_hosted, service.container_id, flag.flag
             FROM "AdFlags" flag
             JOIN "AdTeamServices" service ON service.id = flag.team_service_id
             JOIN "Participations" participation
               ON participation.id = service.participation_id
              AND participation.game_id = service.game_id
             JOIN "GameChallenges" challenge
               ON challenge.id = service.challenge_id
              AND challenge.game_id = service.game_id
            WHERE flag.round_id = $1
              AND service.game_id = $2
              AND participation.status = $3
              AND challenge.is_enabled = TRUE
              AND challenge.review_status = $4
              AND challenge."Type" = $5
            ORDER BY service.id, flag.id"#,
    )
    .bind(round_id)
    .bind(game_id)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeType::AttackDefense as i16)
    .fetch_all(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .into_iter()
    .map(
        |(team_service_id, participation_id, challenge_id, managed, container_id, flag)| {
            AdvancedRoundFlag {
                team_service_id,
                participation_id,
                challenge_id,
                managed,
                container_id,
                flag,
            }
        },
    )
    .collect();

    Ok(AdvancedRound {
        id,
        number,
        started_at,
        ends_at,
        created: false,
        flags,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectOptions, Database};

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn graceful_cleanup_releases_only_this_process_leases() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
        let mut options = ConnectOptions::new(database_url);
        options.max_connections(1).min_connections(1);
        let db = Database::connect(options).await.unwrap();
        let pool = db.get_postgres_connection_pool();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "AdRounds" (
              id INTEGER PRIMARY KEY, game_id INTEGER, finalized BOOLEAN,
              end_time_utc TIMESTAMPTZ, pipeline_completed_at TIMESTAMPTZ,
              pipeline_lease_token TEXT, pipeline_lease_until TIMESTAMPTZ
            );
            INSERT INTO "AdRounds" VALUES
              (1, 9, FALSE, clock_timestamp()+interval '1 minute', NULL, NULL, NULL),
              (2, 9, FALSE, clock_timestamp()+interval '1 minute', NULL,
               'another-process:token', clock_timestamp()+interval '1 minute');
            "#,
        )
        .execute(pool)
        .await
        .unwrap();

        assert!(matches!(
            claim_round_finish(&db, 9, 1).await.unwrap(),
            RoundFinishDisposition::Claimed(_)
        ));
        assert_eq!(abandon_process_round_finishes(&db).await.unwrap(), 1);
        let rows = sqlx::query_as::<_, (i32, Option<String>)>(
            r#"SELECT id, pipeline_lease_token FROM "AdRounds" ORDER BY id"#,
        )
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(rows[0], (1, None));
        assert_eq!(rows[1], (2, Some("another-process:token".to_string())));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn checker_persistence_lock_rejects_a_released_lease() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
        let mut options = ConnectOptions::new(database_url);
        options.max_connections(1).min_connections(1);
        let db = Database::connect(options).await.unwrap();
        let pool = db.get_postgres_connection_pool();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "AdRounds" (
              id INTEGER PRIMARY KEY, game_id INTEGER, finalized BOOLEAN,
              end_time_utc TIMESTAMPTZ, pipeline_completed_at TIMESTAMPTZ,
              pipeline_lease_token TEXT, pipeline_lease_until TIMESTAMPTZ
            );
            INSERT INTO "AdRounds" VALUES
              (1, 9, FALSE, clock_timestamp()+interval '1 minute', NULL, NULL, NULL);
            "#,
        )
        .execute(pool)
        .await
        .unwrap();

        let lease = match claim_round_finish(&db, 9, 1).await.unwrap() {
            RoundFinishDisposition::Claimed(lease) => lease,
            disposition => panic!("expected lease claim, got {disposition:?}"),
        };
        let mut owned = crate::utils::database::begin_sqlx_transaction(pool)
            .await
            .unwrap();
        lock_owned_round_finish(&mut owned, 9, 1, &lease)
            .await
            .unwrap();
        owned.commit().await.unwrap();

        abandon_round_finish(&db, 9, 1, &lease).await.unwrap();
        let mut released = crate::utils::database::begin_sqlx_transaction(pool)
            .await
            .unwrap();
        assert!(lock_owned_round_finish(&mut released, 9, 1, &lease)
            .await
            .is_err());
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn deadline_fence_is_idempotent_and_rejects_late_overwrite() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
        let mut options = ConnectOptions::new(database_url);
        options.max_connections(1).min_connections(1);
        let db = Database::connect(options).await.unwrap();
        let pool = db.get_postgres_connection_pool();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "Games" (id INTEGER PRIMARY KEY, end_time_utc TIMESTAMPTZ);
            CREATE TEMP TABLE "AdRounds" (
              id INTEGER PRIMARY KEY, game_id INTEGER, number INTEGER,
              start_time_utc TIMESTAMPTZ, end_time_utc TIMESTAMPTZ,
              finalized BOOLEAN,
              flags_published_at TIMESTAMPTZ, flag_delivery_failures INTEGER,
              pipeline_completed_at TIMESTAMPTZ,
              pipeline_lease_token TEXT, pipeline_lease_until TIMESTAMPTZ
            );
            CREATE TEMP TABLE "AdTeamServices" (
              id INTEGER PRIMARY KEY, game_id INTEGER, challenge_id INTEGER,
              container_id TEXT
            );
            CREATE TEMP TABLE "AdFlags" (
              round_id INTEGER, team_service_id INTEGER,
              PRIMARY KEY (round_id, team_service_id)
            );
            CREATE TEMP TABLE "AdFlagDeliveryResults" (
              round_id INTEGER, team_service_id INTEGER,
              delivery_kind TEXT, container_id TEXT, delivered BOOLEAN,
              attempts SMALLINT, failure_reason TEXT, completed_at TIMESTAMPTZ,
              PRIMARY KEY (round_id, team_service_id)
            );
            CREATE TEMP TABLE "AdCheckResults" (
              round_id INTEGER, team_service_id INTEGER, status SMALLINT, message TEXT,
              checked_at TIMESTAMPTZ, sla_credit DOUBLE PRECISION,
              flag_verified BOOLEAN
            );
            CREATE TEMP TABLE "GameChallenges" (
              id INTEGER, game_id INTEGER, is_enabled BOOLEAN,
              review_status SMALLINT, "Type" SMALLINT,
              ad_self_hosted BOOLEAN
            );
            CREATE TEMP TABLE "Participations" (id INTEGER, game_id INTEGER, status SMALLINT);
            CREATE TEMP TABLE "KothTargets" (
              game_id INTEGER, challenge_id INTEGER,
              holder_participation_id INTEGER, container_id TEXT
            );
            CREATE TEMP TABLE "KothCrownCycles" (
              id BIGINT, game_id INTEGER, challenge_id INTEGER,
              cycle_number INTEGER,
              planned_start_round INTEGER,
              planned_end_round INTEGER, replacement_container_id TEXT,
              old_container_id TEXT, reset_attempt INTEGER
            );
            CREATE TEMP TABLE "KothControlResults" (
              game_id INTEGER, challenge_id INTEGER, ad_round_id INTEGER,
              controlling_participation_id INTEGER,
              responsible_participation_id INTEGER, marker_observed BOOLEAN,
              status SMALLINT, error_message TEXT, checked_at TIMESTAMPTZ,
              is_scorable BOOLEAN, void_reason TEXT, cycle_id BIGINT,
              container_id TEXT, confirmation_streak INTEGER,
              confirmed_participation_id INTEGER, token_window_attempt INTEGER
            );
            CREATE UNIQUE INDEX ON "KothControlResults" (game_id,challenge_id,ad_round_id);
            INSERT INTO "Games" VALUES (9, clock_timestamp()+interval '1 hour');
            INSERT INTO "AdRounds" VALUES (
              4,9,1,clock_timestamp()-interval '1 minute',
              clock_timestamp()-interval '1 second',FALSE,
              NULL,0,NULL,NULL,NULL
            );
            INSERT INTO "GameChallenges" VALUES (6,9,TRUE,0,4,FALSE);
            INSERT INTO "AdTeamServices" VALUES (5,9,6,'container-5');
            INSERT INTO "AdFlags" VALUES (4,5);
            INSERT INTO "AdCheckResults" VALUES
              (4,5,3,'pending',clock_timestamp(),NULL,FALSE);
            "#,
        )
        .execute(pool)
        .await
        .unwrap();

        let lease = match claim_round_finish(&db, 9, 4).await.unwrap() {
            RoundFinishDisposition::Claimed(lease) => lease,
            disposition => panic!("expected first deadline owner, got {disposition:?}"),
        };
        assert!(matches!(
            claim_round_finish(&db, 9, 4).await.unwrap(),
            RoundFinishDisposition::InFlight
        ));
        assert!(expire_overdue_round_finish(&db, 9, 4, &lease)
            .await
            .unwrap());
        assert!(!expire_overdue_round_finish(&db, 9, 4, &lease)
            .await
            .unwrap());

        let round: (bool, bool, i32, bool) = sqlx::query_as(
            r#"SELECT flags_published_at IS NULL,
                      pipeline_completed_at > end_time_utc,
                      flag_delivery_failures,
                      pipeline_lease_token IS NULL
                 FROM "AdRounds" WHERE id=4"#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(round, (false, true, 1, true));
        let late = sqlx::query(
            r#"UPDATE "AdCheckResults" SET status=0, sla_credit=1.0
                WHERE round_id=4 AND sla_credit IS NULL"#,
        )
        .execute(pool)
        .await
        .unwrap();
        assert_eq!(late.rows_affected(), 0);
    }
}
