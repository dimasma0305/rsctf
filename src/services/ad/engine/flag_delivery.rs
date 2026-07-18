//! Durable A&D flag-publication receipts and participant-safe failure evidence.

use super::*;
use std::collections::{HashMap, HashSet};

const UNAVAILABLE_DELIVERY_REASON: &str =
    "flag delivery target became unavailable before publication completed";
const EXPIRED_DELIVERY_REASON: &str = "round pipeline expired before flag delivery completed";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FlagDeliveryKind {
    Managed,
    External,
}

impl FlagDeliveryKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Managed => "Managed",
            Self::External => "External",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FlagDeliveryOutcome {
    pub(crate) team_service_id: i32,
    pub(crate) kind: FlagDeliveryKind,
    pub(crate) container_id: Option<String>,
    pub(crate) delivered: bool,
    pub(crate) attempts: i16,
    pub(crate) failure_reason: Option<String>,
    pub(crate) completed_at: chrono::DateTime<Utc>,
}

impl FlagDeliveryOutcome {
    pub(crate) fn succeeded(
        team_service_id: i32,
        kind: FlagDeliveryKind,
        container_id: Option<String>,
        attempts: usize,
    ) -> Self {
        Self {
            team_service_id,
            kind,
            container_id,
            delivered: true,
            attempts: i16::try_from(attempts).unwrap_or(i16::MAX),
            failure_reason: None,
            completed_at: Utc::now(),
        }
    }

    pub(crate) fn failed(
        team_service_id: i32,
        kind: FlagDeliveryKind,
        container_id: Option<String>,
        attempts: usize,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            team_service_id,
            kind,
            container_id,
            delivered: false,
            attempts: i16::try_from(attempts).unwrap_or(i16::MAX),
            failure_reason: Some(reason.into()),
            completed_at: Utc::now(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FlagDeliveryPublication {
    pub(crate) failure_count: i32,
}

fn validate_outcomes(outcomes: &[FlagDeliveryOutcome]) -> AppResult<()> {
    let mut service_ids = HashSet::with_capacity(outcomes.len());
    for outcome in outcomes {
        if !service_ids.insert(outcome.team_service_id) {
            return Err(AppError::conflict(format!(
                "duplicate flag-delivery outcome for service {}",
                outcome.team_service_id
            )));
        }
        if !(0..=5).contains(&outcome.attempts)
            || (outcome.delivered && (outcome.attempts == 0 || outcome.failure_reason.is_some()))
            || (!outcome.delivered
                && outcome
                    .failure_reason
                    .as_deref()
                    .is_none_or(|reason| reason.trim().is_empty()))
            || (outcome.kind == FlagDeliveryKind::External && outcome.container_id.is_some())
        {
            return Err(AppError::conflict(format!(
                "invalid flag-delivery outcome for service {}",
                outcome.team_service_id
            )));
        }
    }
    Ok(())
}

async fn complete_failed_check_results(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    round_id: i32,
) -> AppResult<()> {
    sqlx::query(
        r#"UPDATE "AdCheckResults" result
              SET status = $2,
                  message = delivery.failure_reason,
                  checked_at = delivery.completed_at,
                  sla_credit = 0.0,
                  flag_verified = FALSE
             FROM "AdFlagDeliveryResults" delivery
            WHERE delivery.round_id = $1
              AND delivery.delivered = FALSE
              AND result.round_id = delivery.round_id
              AND result.team_service_id = delivery.team_service_id
              AND result.sla_credit IS NULL"#,
    )
    .bind(round_id)
    .bind(AdCheckStatus::InternalError as i16)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(|error| AppError::internal(error.to_string()))
}

async fn insert_missing_outcomes(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    game_id: i32,
    round_id: i32,
    reason: &str,
    completed_at: chrono::DateTime<Utc>,
) -> AppResult<()> {
    sqlx::query(
        r#"INSERT INTO "AdFlagDeliveryResults"
             (round_id, team_service_id, delivery_kind, container_id,
              delivered, attempts, failure_reason, completed_at)
           SELECT flag.round_id, flag.team_service_id,
                  CASE WHEN challenge.ad_self_hosted THEN 'External' ELSE 'Managed' END,
                  CASE WHEN challenge.ad_self_hosted THEN NULL ELSE service.container_id END,
                  FALSE, 0, $3, $4
             FROM "AdFlags" flag
             JOIN "AdRounds" round ON round.id = flag.round_id
             JOIN "AdTeamServices" service ON service.id = flag.team_service_id
             JOIN "GameChallenges" challenge
               ON challenge.id = service.challenge_id
              AND challenge.game_id = service.game_id
            WHERE flag.round_id = $1
              AND round.game_id = $2
           ON CONFLICT (round_id, team_service_id) DO NOTHING"#,
    )
    .bind(round_id)
    .bind(game_id)
    .bind(reason)
    .bind(completed_at)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(|error| AppError::internal(error.to_string()))
}

async fn settle_round_publication(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    game_id: i32,
    round_id: i32,
    published_at: chrono::DateTime<Utc>,
) -> AppResult<FlagDeliveryPublication> {
    complete_failed_check_results(tx, round_id).await?;
    let row: Option<(i32,)> = sqlx::query_as(
        r#"UPDATE "AdRounds" round
              SET flags_published_at = COALESCE(round.flags_published_at, $3),
                  flag_delivery_failures = (
                    SELECT COUNT(*)::integer
                      FROM "AdFlagDeliveryResults" delivery
                     WHERE delivery.round_id = round.id
                       AND delivery.delivered = FALSE
                  )
            WHERE round.id = $1 AND round.game_id = $2
              AND round.finalized = FALSE
              AND NOT EXISTS (
                    SELECT 1 FROM "AdFlags" flag
                     WHERE flag.round_id = round.id
                       AND NOT EXISTS (
                         SELECT 1 FROM "AdFlagDeliveryResults" delivery
                          WHERE delivery.round_id = flag.round_id
                            AND delivery.team_service_id = flag.team_service_id
                       )
                  )
        RETURNING round.flag_delivery_failures"#,
    )
    .bind(round_id)
    .bind(game_id)
    .bind(published_at)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let (failure_count,) = row.ok_or_else(|| {
        AppError::conflict("flag delivery did not settle every service in the round")
    })?;
    Ok(FlagDeliveryPublication { failure_count })
}

async fn record_flag_delivery_outcomes_transaction(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    game_id: i32,
    round_id: i32,
    outcomes: &[FlagDeliveryOutcome],
) -> AppResult<FlagDeliveryPublication> {
    validate_outcomes(outcomes)?;
    let round: Option<(
        chrono::DateTime<Utc>,
        chrono::DateTime<Utc>,
        chrono::DateTime<Utc>,
        Option<chrono::DateTime<Utc>>,
        i32,
    )> = sqlx::query_as(
        r#"SELECT round.start_time_utc, round.end_time_utc, game.end_time_utc,
                  round.flags_published_at, round.flag_delivery_failures
             FROM "AdRounds" round
             JOIN "Games" game ON game.id = round.game_id
            WHERE round.id = $1 AND round.game_id = $2
              AND round.finalized = FALSE
            FOR UPDATE OF round"#,
    )
    .bind(round_id)
    .bind(game_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((round_start, round_end, game_end, published_at, failure_count)) = round else {
        return Err(AppError::conflict(
            "flag-publication round is no longer active",
        ));
    };
    if published_at.is_some() {
        return Ok(FlagDeliveryPublication { failure_count });
    }
    let now: chrono::DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if now >= round_end || now >= game_end {
        return Err(AppError::conflict(
            "flag delivery crossed the authoritative round deadline",
        ));
    }
    if outcomes.iter().any(|outcome| {
        outcome.completed_at < round_start
            || outcome.completed_at >= round_end
            || outcome.completed_at >= game_end
    }) {
        return Err(AppError::conflict(
            "flag-delivery evidence falls outside the scoring window",
        ));
    }

    if !outcomes.is_empty() {
        let service_ids: Vec<i32> = outcomes
            .iter()
            .map(|outcome| outcome.team_service_id)
            .collect();
        let roster: Vec<(i32, bool, Option<String>)> = sqlx::query_as(
            r#"SELECT service.id, NOT challenge.ad_self_hosted, service.container_id
                 FROM "AdFlags" flag
                 JOIN "AdTeamServices" service ON service.id = flag.team_service_id
                 JOIN "GameChallenges" challenge
                   ON challenge.id = service.challenge_id
                  AND challenge.game_id = service.game_id
                WHERE flag.round_id = $1
                  AND service.game_id = $2
                  AND service.id = ANY($3)
                ORDER BY service.id
                FOR SHARE OF service, challenge"#,
        )
        .bind(round_id)
        .bind(game_id)
        .bind(&service_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        let roster: HashMap<_, _> = roster
            .into_iter()
            .map(|(service_id, managed, container_id)| (service_id, (managed, container_id)))
            .collect();
        if roster.len() != outcomes.len()
            || outcomes.iter().any(|outcome| {
                roster
                    .get(&outcome.team_service_id)
                    .is_none_or(|(managed, container_id)| {
                        *managed != (outcome.kind == FlagDeliveryKind::Managed)
                            || (*managed && container_id != &outcome.container_id)
                    })
            })
        {
            return Err(AppError::conflict(
                "flag-delivery batch no longer matches the authoritative service target",
            ));
        }
        let kinds: Vec<&str> = outcomes
            .iter()
            .map(|outcome| outcome.kind.as_str())
            .collect();
        let container_ids: Vec<Option<&str>> = outcomes
            .iter()
            .map(|outcome| outcome.container_id.as_deref())
            .collect();
        let delivered: Vec<bool> = outcomes.iter().map(|outcome| outcome.delivered).collect();
        let attempts: Vec<i16> = outcomes.iter().map(|outcome| outcome.attempts).collect();
        let reasons: Vec<Option<&str>> = outcomes
            .iter()
            .map(|outcome| outcome.failure_reason.as_deref())
            .collect();
        let completed_at: Vec<chrono::DateTime<Utc>> = outcomes
            .iter()
            .map(|outcome| outcome.completed_at)
            .collect();
        sqlx::query(
            r#"INSERT INTO "AdFlagDeliveryResults"
                 (round_id, team_service_id, delivery_kind, container_id,
                  delivered, attempts, failure_reason, completed_at)
               SELECT $1, input.team_service_id, input.delivery_kind,
                      input.container_id, input.delivered, input.attempts,
                      input.failure_reason, input.completed_at
                 FROM UNNEST(
                   $2::integer[], $3::text[], $4::text[], $5::boolean[],
                   $6::smallint[], $7::text[], $8::timestamptz[]
                 ) AS input(
                   team_service_id, delivery_kind, container_id, delivered,
                   attempts, failure_reason, completed_at
                 )
               ON CONFLICT (round_id, team_service_id) DO NOTHING"#,
        )
        .bind(round_id)
        .bind(&service_ids)
        .bind(&kinds)
        .bind(&container_ids)
        .bind(&delivered)
        .bind(&attempts)
        .bind(&reasons)
        .bind(&completed_at)
        .execute(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }

    insert_missing_outcomes(tx, game_id, round_id, UNAVAILABLE_DELIVERY_REASON, now).await?;
    settle_round_publication(tx, game_id, round_id, now).await
}

pub(crate) async fn record_flag_delivery_outcomes(
    db: &DatabaseConnection,
    game_id: i32,
    round_id: i32,
    outcomes: &[FlagDeliveryOutcome],
) -> AppResult<FlagDeliveryPublication> {
    let mut control = super::koth_auth::acquire_game_lock(db, game_id).await?;
    let publication = record_flag_delivery_outcomes_transaction(
        control.transaction_mut(),
        game_id,
        round_id,
        outcomes,
    )
    .await?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(publication)
}

pub(super) async fn complete_missing_flag_delivery_outcomes_transaction(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    game_id: i32,
    round_id: i32,
) -> AppResult<()> {
    let completed_at: Option<chrono::DateTime<Utc>> = sqlx::query_scalar(
        r#"SELECT LEAST(clock_timestamp(), round.end_time_utc, game.end_time_utc)
             FROM "AdRounds" round
             JOIN "Games" game ON game.id = round.game_id
            WHERE round.id = $1 AND round.game_id = $2
              AND round.finalized = FALSE
            FOR UPDATE OF round"#,
    )
    .bind(round_id)
    .bind(game_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(completed_at) = completed_at else {
        return Ok(());
    };
    insert_missing_outcomes(tx, game_id, round_id, EXPIRED_DELIVERY_REASON, completed_at).await?;
    settle_round_publication(tx, game_id, round_id, completed_at)
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectOptions, Database};

    fn failure(service_id: i32) -> FlagDeliveryOutcome {
        FlagDeliveryOutcome::failed(
            service_id,
            FlagDeliveryKind::Managed,
            Some(format!("container-{service_id}")),
            3,
            "container exec failed",
        )
    }

    #[test]
    fn duplicate_service_outcomes_are_rejected_instead_of_double_counted() {
        let outcomes = vec![failure(7), failure(7)];
        assert!(matches!(
            validate_outcomes(&outcomes),
            Err(AppError::Conflict(_))
        ));
    }

    #[test]
    fn outcome_shape_rejects_false_success_and_external_container_identity() {
        let mut outcome = FlagDeliveryOutcome::succeeded(
            1,
            FlagDeliveryKind::External,
            Some("not-valid-for-external".into()),
            1,
        );
        assert!(validate_outcomes(&[outcome.clone()]).is_err());
        outcome.container_id = None;
        outcome.attempts = 0;
        assert!(validate_outcomes(&[outcome]).is_err());
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn publication_is_idempotent_and_counts_each_failed_service_once() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
        let mut options = ConnectOptions::new(database_url);
        options.max_connections(1).min_connections(1);
        let db = Database::connect(options).await.unwrap();
        let pool = db.get_postgres_connection_pool();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "Games" (
              id INTEGER PRIMARY KEY, end_time_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TEMP TABLE "AdRounds" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              start_time_utc TIMESTAMPTZ NOT NULL, end_time_utc TIMESTAMPTZ NOT NULL,
              finalized BOOLEAN NOT NULL, flags_published_at TIMESTAMPTZ,
              flag_delivery_failures INTEGER NOT NULL
            );
            CREATE TEMP TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              ad_self_hosted BOOLEAN NOT NULL
            );
            CREATE TEMP TABLE "AdTeamServices" (
              id INTEGER PRIMARY KEY, challenge_id INTEGER NOT NULL,
              game_id INTEGER NOT NULL, container_id TEXT
            );
            CREATE TEMP TABLE "AdFlags" (
              round_id INTEGER NOT NULL, team_service_id INTEGER NOT NULL,
              PRIMARY KEY (round_id, team_service_id)
            );
            CREATE TEMP TABLE "AdCheckResults" (
              round_id INTEGER NOT NULL, team_service_id INTEGER NOT NULL,
              status SMALLINT NOT NULL, message TEXT, checked_at TIMESTAMPTZ NOT NULL,
              sla_credit DOUBLE PRECISION, flag_verified BOOLEAN NOT NULL,
              PRIMARY KEY (round_id, team_service_id)
            );
            CREATE TEMP TABLE "AdFlagDeliveryResults" (
              round_id INTEGER NOT NULL, team_service_id INTEGER NOT NULL,
              delivery_kind TEXT NOT NULL, container_id TEXT, delivered BOOLEAN NOT NULL,
              attempts SMALLINT NOT NULL, failure_reason TEXT,
              completed_at TIMESTAMPTZ NOT NULL,
              PRIMARY KEY (round_id, team_service_id)
            );
            INSERT INTO "GameChallenges" VALUES (4, 7, FALSE);
            INSERT INTO "AdTeamServices" VALUES
              (11, 4, 7, 'container-11'), (12, 4, 7, 'container-12');
            INSERT INTO "AdFlags" VALUES (9, 11), (9, 12);
            INSERT INTO "AdCheckResults" VALUES
              (9, 11, 3, 'pending', clock_timestamp(), NULL, FALSE),
              (9, 12, 3, 'pending', clock_timestamp(), NULL, FALSE);
            "#,
        )
        .execute(pool)
        .await
        .unwrap();
        let now: chrono::DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "Games" VALUES (7, $1)"#)
            .bind(now + chrono::Duration::minutes(5))
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "AdRounds" VALUES (9, 7, $1, $2, FALSE, NULL, 0)"#)
            .bind(now - chrono::Duration::seconds(1))
            .bind(now + chrono::Duration::minutes(1))
            .execute(pool)
            .await
            .unwrap();

        let outcomes = vec![
            FlagDeliveryOutcome::failed(
                11,
                FlagDeliveryKind::Managed,
                Some("container-11".into()),
                3,
                "repair and push both failed",
            ),
            FlagDeliveryOutcome::succeeded(
                12,
                FlagDeliveryKind::Managed,
                Some("container-12".into()),
                1,
            ),
        ];
        let first = record_flag_delivery_outcomes(&db, 7, 9, &outcomes)
            .await
            .unwrap();
        let replay = record_flag_delivery_outcomes(&db, 7, 9, &outcomes)
            .await
            .unwrap();
        assert_eq!(first.failure_count, 1);
        assert_eq!(replay.failure_count, 1);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"SELECT COUNT(*) FROM "AdFlagDeliveryResults" WHERE round_id = 9"#,
            )
            .fetch_one(pool)
            .await
            .unwrap(),
            2
        );
        let failed_check: (i16, Option<String>, Option<f64>, bool) = sqlx::query_as(
            r#"SELECT status, message, sla_credit, flag_verified
                 FROM "AdCheckResults" WHERE round_id = 9 AND team_service_id = 11"#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(failed_check.0, AdCheckStatus::InternalError as i16);
        assert_eq!(
            failed_check.1.as_deref(),
            Some("repair and push both failed")
        );
        assert_eq!(failed_check.2, Some(0.0));
        assert!(!failed_check.3);
        let healthy_credit: Option<f64> = sqlx::query_scalar(
            r#"SELECT sla_credit FROM "AdCheckResults"
                WHERE round_id = 9 AND team_service_id = 12"#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(healthy_credit, None);
    }
}
