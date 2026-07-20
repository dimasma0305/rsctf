//! Preservation fence for participation-owned competition and audit history.
//!
//! A participation is part of the scoring identity. Once it owns evidence, a
//! division move would reinterpret that evidence and deleting the row would
//! cascade scoring rows or orphan audit rows that deliberately have no foreign
//! key. Keep the policy and the physical-delete predicate together so admin
//! review, leave, and re-registration cannot drift apart.

use crate::utils::enums::ParticipationStatus;
use crate::utils::error::{AppError, AppResult};

/// Every durable row that can establish Jeopardy, A&D, KotH, or anti-cheat
/// activity for a participation. Provisioning-only rows such as
/// `AdTeamServices` are not evidence by themselves; their scored children are.
const COMPETITION_EVIDENCE_SELECT_SQL: &str = r#"
    SELECT 1 FROM "Participations" participation
     WHERE participation.id = $1 AND participation.writeup_id IS NOT NULL
    UNION ALL
    SELECT 1 FROM "Submissions" submission
     WHERE submission.participation_id = $1
    UNION ALL
    SELECT 1 FROM "FirstSolves" first_solve
     WHERE first_solve.participation_id = $1
    UNION ALL
    SELECT 1 FROM "SuspicionEvents" suspicion
     WHERE suspicion.participation_id = $1
    UNION ALL
    SELECT 1 FROM "HoneypotHits" hit
     WHERE hit.participation_id = $1
    UNION ALL
    SELECT 1 FROM "ContainerAccessEvents" access
     WHERE access.container_owner_participation_id = $1
        OR access.accessing_participation_id = $1
    UNION ALL
    SELECT 1 FROM "FlagEgressEvents" egress
     WHERE egress.participation_id = $1
    UNION ALL
    SELECT 1 FROM "TrafficCaptureFailures" failure
     WHERE failure.participation_id = $1
    UNION ALL
    SELECT 1
      FROM "AdFlags" flag
      JOIN "AdTeamServices" service ON service.id = flag.team_service_id
     WHERE service.participation_id = $1
    UNION ALL
    SELECT 1
      FROM "AdCheckResults" result
      JOIN "AdTeamServices" service ON service.id = result.team_service_id
     WHERE service.participation_id = $1
    UNION ALL
    SELECT 1
      FROM "AdFlagDeliveryResults" delivery
      JOIN "AdTeamServices" service ON service.id = delivery.team_service_id
     WHERE service.participation_id = $1
    UNION ALL
    SELECT 1 FROM "AdAttacks" attack
     WHERE attack.attacker_participation_id = $1
    UNION ALL
    SELECT 1
      FROM "AdAttacks" attack
      JOIN "AdTeamServices" service ON service.id = attack.victim_team_service_id
     WHERE service.participation_id = $1
    UNION ALL
    SELECT 1 FROM "AdEpochServiceRollups" rollup
     WHERE rollup.participation_id = $1
    UNION ALL
    SELECT 1 FROM "AdEpochTeamRollups" rollup
     WHERE rollup.participation_id = $1
    UNION ALL
    SELECT 1 FROM "KothTargets" target
     WHERE target.holder_participation_id = $1
    UNION ALL
    SELECT 1 FROM "KothTokens" token
     WHERE token.participation_id = $1
    UNION ALL
    SELECT 1 FROM "KothControlResults" result
     WHERE result.controlling_participation_id = $1
        OR result.responsible_participation_id = $1
        OR result.provisional_participation_id = $1
        OR result.confirmed_participation_id = $1
    UNION ALL
    SELECT 1 FROM "KothCrownCycles" cycle
     WHERE cycle.champion_participation_id = $1
        OR cycle.provisional_participation_id = $1
        OR cycle.confirmed_participation_id = $1
    UNION ALL
    SELECT 1 FROM "KothCycleCooldowns" cooldown
     WHERE cooldown.participation_id = $1
    UNION ALL
    SELECT 1 FROM "KothClaimStates" claim
     WHERE claim.provisional_participation_id = $1
        OR claim.confirmed_participation_id = $1
    UNION ALL
    SELECT 1 FROM "KothAcquisitions" acquisition
     WHERE acquisition.participation_id = $1
    UNION ALL
    SELECT 1 FROM "KothEpochTeamRollups" rollup
     WHERE rollup.participation_id = $1
    UNION ALL
    SELECT 1 FROM "KothEpochHillRollups" rollup
     WHERE rollup.participation_id = $1
"#;

pub(crate) async fn has_competition_evidence(
    connection: &mut sqlx::PgConnection,
    participation_id: i32,
) -> AppResult<bool> {
    let sql = format!("SELECT EXISTS ({COMPETITION_EVIDENCE_SELECT_SQL})");
    sqlx::query_scalar(&sql)
        .bind(participation_id)
        .fetch_one(connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

/// Hold each attributed participation against status mutation or physical
/// cleanup until an audit insert commits. Callers must insert the audit row in
/// this same transaction. Stable ordering keeps the two-party container-access
/// case deadlock-free across replicas.
#[cfg(test)]
async fn lock_participations_for_audit_insert(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    participation_ids: &[i32],
) -> AppResult<bool> {
    let mut participation_ids = participation_ids.to_vec();
    participation_ids.sort_unstable();
    participation_ids.dedup();
    if participation_ids.is_empty() {
        return Ok(true);
    }
    let locked: Vec<i32> = sqlx::query_scalar(
        r#"SELECT id
              FROM "Participations"
             WHERE id = ANY($1)
             ORDER BY id
             FOR SHARE"#,
    )
    .bind(&participation_ids)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(locked.len() == participation_ids.len())
}

/// Canonical lock order for an attributed audit insert. Hard game/challenge
/// deletion takes the conflicting row locks in the same outer-to-inner order,
/// so either this writer commits evidence first or it observes a suspended or
/// deletion-pending scope and emits no late evidence that could poison a
/// previously authorized deletion retry.
pub(crate) async fn lock_audit_insert_scope(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    challenge_id: Option<i32>,
    participation_ids: &[i32],
) -> AppResult<bool> {
    lock_audit_insert_scope_sqlx(transaction, game_id, challenge_id, participation_ids)
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

/// `sqlx`-native form for infrastructure workers whose surrounding operation
/// already exposes `sqlx::Error` rather than an HTTP-facing [`AppError`].
pub(crate) async fn lock_audit_insert_scope_sqlx(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    challenge_id: Option<i32>,
    participation_ids: &[i32],
) -> Result<bool, sqlx::Error> {
    let game_exists = sqlx::query_scalar::<_, i32>(
        r#"SELECT id
              FROM "Games"
             WHERE id = $1 AND deletion_pending = FALSE
             FOR SHARE"#,
    )
    .bind(game_id)
    .fetch_optional(&mut **transaction)
    .await?
    .is_some();
    if !game_exists {
        return Ok(false);
    }
    if let Some(challenge_id) = challenge_id {
        let challenge_exists = sqlx::query_scalar::<_, i32>(
            r#"SELECT id
                  FROM "GameChallenges"
                 WHERE id = $1 AND game_id = $2
                   AND is_enabled = TRUE
                   AND deletion_pending = FALSE
                 FOR SHARE"#,
        )
        .bind(challenge_id)
        .bind(game_id)
        .fetch_optional(&mut **transaction)
        .await?
        .is_some();
        if !challenge_exists {
            return Ok(false);
        }
    }

    let mut participation_ids = participation_ids.to_vec();
    participation_ids.sort_unstable();
    participation_ids.dedup();
    if participation_ids.is_empty() {
        return Ok(true);
    }
    let locked: Vec<i32> = sqlx::query_scalar(
        r#"SELECT participation.id
              FROM "Participations" participation
              JOIN "Teams" team ON team.id = participation.team_id
             WHERE participation.id = ANY($1)
               AND participation.game_id = $2
               AND participation.status = $3
               AND team.deletion_pending = FALSE
             ORDER BY participation.id
             FOR SHARE OF participation"#,
    )
    .bind(&participation_ids)
    .bind(game_id)
    .bind(ParticipationStatus::Accepted as i16)
    .fetch_all(&mut **transaction)
    .await?;
    Ok(locked.len() == participation_ids.len())
}

/// Reject mutations that would erase or reinterpret an established scoring
/// identity. Suspension remains available as the reversible administrative
/// sanction and deliberately does not clear the division.
pub(crate) async fn ensure_evidence_preserving_update(
    connection: &mut sqlx::PgConnection,
    participation_id: i32,
    current_status: ParticipationStatus,
    requested_status: ParticipationStatus,
    current_division_id: Option<i32>,
    requested_division_id: Option<i32>,
) -> AppResult<()> {
    let changes_status = current_status != requested_status;
    let reversible_sanction = matches!(
        (current_status, requested_status),
        (
            ParticipationStatus::Accepted,
            ParticipationStatus::Suspended
        ) | (
            ParticipationStatus::Suspended,
            ParticipationStatus::Accepted
        )
    );
    let rewrites_participation = changes_status && !reversible_sanction;
    let changes_division = current_division_id != requested_division_id;
    if !rewrites_participation && !changes_division {
        return Ok(());
    }
    if !has_competition_evidence(connection, participation_id).await? {
        return Ok(());
    }
    if rewrites_participation {
        if requested_status != ParticipationStatus::Rejected {
            return Err(AppError::bad_request(
                "A participation with competition evidence may only move between Accepted and Suspended.",
            ));
        }
        return Err(AppError::bad_request(
            "A participation with competition evidence cannot be rejected; suspend it instead.",
        ));
    }
    Err(AppError::bad_request(
        "Participation division cannot change after competition evidence exists.",
    ))
}

async fn delete_unlinked_without_evidence(
    connection: &mut sqlx::PgConnection,
    participation_id: i32,
    include_pending: bool,
) -> AppResult<bool> {
    // The caller has already selected the participation FOR UPDATE. That lock is
    // essential: a concurrent submission first takes FOR SHARE, so either its
    // evidence commits before this statement's fresh snapshot or it cannot start
    // until after the parent is gone. The NOT EXISTS checks remain in this same
    // DELETE as a final atomic defense for legacy rejected rows.
    let sql = format!(
        r#"DELETE FROM "Participations" participation
            WHERE participation.id = $1
              AND (
                    participation.status = $2
                    OR ($3 AND participation.status = $4)
              )
              AND NOT EXISTS (
                    SELECT 1 FROM "UserParticipations" membership
                     WHERE membership.participation_id = participation.id
              )
              AND NOT EXISTS ({COMPETITION_EVIDENCE_SELECT_SQL})
          RETURNING participation.id"#,
    );
    let deleted = sqlx::query_scalar::<_, i32>(&sql)
        .bind(participation_id)
        .bind(ParticipationStatus::Rejected as i16)
        .bind(include_pending)
        .bind(ParticipationStatus::Pending as i16)
        .fetch_optional(connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(deleted.is_some())
}

/// Remove an unlinked rejected registration only if it owns no competition
/// evidence. Used when a user re-registers through another team.
pub(crate) async fn delete_unlinked_rejected_without_evidence(
    connection: &mut sqlx::PgConnection,
    participation_id: i32,
) -> AppResult<bool> {
    delete_unlinked_without_evidence(connection, participation_id, false).await
}

/// Remove an unlinked pending/rejected registration only if it owns no
/// competition evidence. Used by the player leave path.
pub(crate) async fn delete_unlinked_pending_or_rejected_without_evidence(
    connection: &mut sqlx::PgConnection,
    participation_id: i32,
) -> AppResult<bool> {
    delete_unlinked_without_evidence(connection, participation_id, true).await
}

/// Minimal relation shapes used by isolated PostgreSQL controller regressions.
/// Production receives the full schema from migrations; keeping this fixture
/// beside the query makes newly-added evidence relations impossible to forget
/// in those focused tests.
#[cfg(test)]
pub(crate) async fn create_test_evidence_tables(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS "Submissions" (
          id INTEGER GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
          participation_id INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS "FirstSolves" (participation_id INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS "SuspicionEvents" (participation_id INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS "HoneypotHits" (participation_id INTEGER);
        CREATE TABLE IF NOT EXISTS "ContainerAccessEvents" (
          container_owner_participation_id INTEGER NOT NULL,
          accessing_participation_id INTEGER
        );
        CREATE TABLE IF NOT EXISTS "FlagEgressEvents" (participation_id INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS "TrafficCaptureFailures" (participation_id INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS "AdTeamServices" (
          id INTEGER GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
          participation_id INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS "AdFlags" (team_service_id INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS "AdCheckResults" (team_service_id INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS "AdFlagDeliveryResults" (team_service_id INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS "AdAttacks" (
          attacker_participation_id INTEGER NOT NULL,
          victim_team_service_id INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS "AdEpochServiceRollups" (participation_id INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS "AdEpochTeamRollups" (participation_id INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS "KothTargets" (holder_participation_id INTEGER);
        CREATE TABLE IF NOT EXISTS "KothTokens" (participation_id INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS "KothControlResults" (
          controlling_participation_id INTEGER,
          responsible_participation_id INTEGER,
          provisional_participation_id INTEGER,
          confirmed_participation_id INTEGER
        );
        CREATE TABLE IF NOT EXISTS "KothCrownCycles" (
          champion_participation_id INTEGER,
          provisional_participation_id INTEGER,
          confirmed_participation_id INTEGER
        );
        CREATE TABLE IF NOT EXISTS "KothCycleCooldowns" (participation_id INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS "KothClaimStates" (
          provisional_participation_id INTEGER,
          confirmed_participation_id INTEGER
        );
        CREATE TABLE IF NOT EXISTS "KothAcquisitions" (participation_id INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS "KothEpochTeamRollups" (participation_id INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS "KothEpochHillRollups" (participation_id INTEGER NOT NULL);
        "#,
    )
    .execute(pool)
    .await
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn anti_cheat_evidence_preserves_participation_identity() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!(
            "rsctf_participation_audit_{}",
            uuid::Uuid::new_v4().simple()
        );
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin_pool)
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
            r#"
            CREATE TABLE "Participations" (
              id INTEGER PRIMARY KEY,
              status SMALLINT NOT NULL,
              division_id INTEGER,
              writeup_id INTEGER
            );
            CREATE TABLE "UserParticipations" (participation_id INTEGER NOT NULL);
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        create_test_evidence_tables(&pool).await.unwrap();

        let cases = [
            (
                "submitted writeup",
                r#"UPDATE "Participations" SET writeup_id = 42 WHERE id = $1"#,
            ),
            (
                "suspicion event",
                r#"INSERT INTO "SuspicionEvents" (participation_id) VALUES ($1)"#,
            ),
            (
                "honeypot hit",
                r#"INSERT INTO "HoneypotHits" (participation_id) VALUES ($1)"#,
            ),
            (
                "container owner access",
                r#"INSERT INTO "ContainerAccessEvents"
                     (container_owner_participation_id, accessing_participation_id)
                   VALUES ($1, NULL)"#,
            ),
            (
                "container accessor access",
                r#"INSERT INTO "ContainerAccessEvents"
                     (container_owner_participation_id, accessing_participation_id)
                   VALUES (-1, $1)"#,
            ),
            (
                "flag egress event",
                r#"INSERT INTO "FlagEgressEvents" (participation_id) VALUES ($1)"#,
            ),
            (
                "traffic capture failure",
                r#"INSERT INTO "TrafficCaptureFailures" (participation_id) VALUES ($1)"#,
            ),
        ];

        for (offset, (label, insert_sql)) in cases.into_iter().enumerate() {
            let participation_id = 100 + offset as i32;
            sqlx::query(
                r#"INSERT INTO "Participations" (id, status, division_id)
                   VALUES ($1, $2, 7)"#,
            )
            .bind(participation_id)
            .bind(ParticipationStatus::Accepted as i16)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(insert_sql)
                .bind(participation_id)
                .execute(&pool)
                .await
                .unwrap();

            let mut connection = pool.acquire().await.unwrap();
            let error = ensure_evidence_preserving_update(
                &mut connection,
                participation_id,
                ParticipationStatus::Accepted,
                ParticipationStatus::Rejected,
                Some(7),
                None,
            )
            .await
            .unwrap_err();
            assert_eq!(
                error.status(),
                axum::http::StatusCode::BAD_REQUEST,
                "{label}"
            );
            assert!(error.to_string().contains("suspend it instead"), "{label}");

            let error = ensure_evidence_preserving_update(
                &mut connection,
                participation_id,
                ParticipationStatus::Accepted,
                ParticipationStatus::Accepted,
                Some(7),
                Some(8),
            )
            .await
            .unwrap_err();
            assert_eq!(
                error.status(),
                axum::http::StatusCode::BAD_REQUEST,
                "{label}"
            );
            assert!(
                error.to_string().contains("division cannot change"),
                "{label}"
            );
            drop(connection);

            sqlx::query(r#"UPDATE "Participations" SET status = $1 WHERE id = $2"#)
                .bind(ParticipationStatus::Rejected as i16)
                .bind(participation_id)
                .execute(&pool)
                .await
                .unwrap();
            let mut connection = pool.acquire().await.unwrap();
            assert!(
                !delete_unlinked_rejected_without_evidence(&mut connection, participation_id)
                    .await
                    .unwrap(),
                "re-registration cleanup deleted {label} identity"
            );
            drop(connection);

            sqlx::query(r#"UPDATE "Participations" SET status = $1 WHERE id = $2"#)
                .bind(ParticipationStatus::Pending as i16)
                .bind(participation_id)
                .execute(&pool)
                .await
                .unwrap();
            let mut connection = pool.acquire().await.unwrap();
            assert!(
                !delete_unlinked_pending_or_rejected_without_evidence(
                    &mut connection,
                    participation_id,
                )
                .await
                .unwrap(),
                "leave cleanup deleted {label} identity"
            );
            drop(connection);

            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    r#"SELECT COUNT(*) FROM "Participations" WHERE id = $1"#,
                )
                .bind(participation_id)
                .fetch_one(&pool)
                .await
                .unwrap(),
                1,
                "{label} lost its participation identity"
            );
        }

        sqlx::query(
            r#"INSERT INTO "Participations" (id, status, division_id)
               VALUES (2000, $1, NULL)"#,
        )
        .bind(ParticipationStatus::Accepted as i16)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(r#"INSERT INTO "AdTeamServices" (id, participation_id) VALUES (20, 2000)"#)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "AdFlagDeliveryResults" (team_service_id) VALUES (20)"#)
            .execute(&pool)
            .await
            .unwrap();
        let mut connection = pool.acquire().await.unwrap();
        assert!(has_competition_evidence(&mut connection, 2000)
            .await
            .unwrap());
        let error = ensure_evidence_preserving_update(
            &mut connection,
            2000,
            ParticipationStatus::Accepted,
            ParticipationStatus::Rejected,
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
        drop(connection);

        // The audit fence must not turn cleanup into a blanket no-op.
        sqlx::query(
            r#"INSERT INTO "Participations" (id, status, division_id)
               VALUES (999, $1, NULL)"#,
        )
        .bind(ParticipationStatus::Rejected as i16)
        .execute(&pool)
        .await
        .unwrap();
        let mut connection = pool.acquire().await.unwrap();
        assert!(
            delete_unlinked_rejected_without_evidence(&mut connection, 999)
                .await
                .unwrap(),
            "an empty rejected participation should remain removable"
        );
        drop(connection);

        // An audit writer that wins the participation share lock must complete
        // before cleanup's authoritative FOR UPDATE read. Cleanup then gets a
        // fresh statement snapshot and preserves both identity and evidence.
        sqlx::query(
            r#"INSERT INTO "Participations" (id, status, division_id)
               VALUES (1000, $1, NULL)"#,
        )
        .bind(ParticipationStatus::Rejected as i16)
        .execute(&pool)
        .await
        .unwrap();
        let mut writer = pool.begin().await.unwrap();
        assert!(lock_participations_for_audit_insert(&mut writer, &[1000])
            .await
            .unwrap());
        sqlx::query(r#"INSERT INTO "HoneypotHits" (participation_id) VALUES (1000)"#)
            .execute(&mut *writer)
            .await
            .unwrap();
        let (cleanup_started_tx, cleanup_started_rx) = tokio::sync::oneshot::channel();
        let mut cleanup = tokio::spawn({
            let pool = pool.clone();
            async move {
                let mut transaction = pool.begin().await.unwrap();
                cleanup_started_tx.send(()).unwrap();
                sqlx::query(r#"SELECT id FROM "Participations" WHERE id = 1000 FOR UPDATE"#)
                    .execute(&mut *transaction)
                    .await
                    .unwrap();
                let deleted = delete_unlinked_rejected_without_evidence(&mut transaction, 1000)
                    .await
                    .unwrap();
                transaction.commit().await.unwrap();
                deleted
            }
        });
        cleanup_started_rx.await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut cleanup)
                .await
                .is_err(),
            "cleanup crossed an in-flight audit writer's participation lock"
        );
        writer.commit().await.unwrap();
        assert!(
            !tokio::time::timeout(std::time::Duration::from_secs(2), cleanup)
                .await
                .expect("cleanup remained blocked after audit commit")
                .expect("cleanup task failed"),
            "cleanup ignored evidence committed by the audit writer"
        );
        let preserved: (i64, i64) = sqlx::query_as(
            r#"SELECT
                  (SELECT COUNT(*) FROM "Participations" WHERE id = 1000),
                  (SELECT COUNT(*) FROM "HoneypotHits" WHERE participation_id = 1000)"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(preserved, (1, 1));

        // If cleanup owns FOR UPDATE first, the writer waits, observes the
        // missing identity after commit, and must not create an orphan row.
        sqlx::query(
            r#"INSERT INTO "Participations" (id, status, division_id)
               VALUES (1001, $1, NULL)"#,
        )
        .bind(ParticipationStatus::Rejected as i16)
        .execute(&pool)
        .await
        .unwrap();
        let mut cleanup = pool.begin().await.unwrap();
        sqlx::query(r#"SELECT id FROM "Participations" WHERE id = 1001 FOR UPDATE"#)
            .execute(&mut *cleanup)
            .await
            .unwrap();
        let (writer_started_tx, writer_started_rx) = tokio::sync::oneshot::channel();
        let mut late_writer = tokio::spawn({
            let pool = pool.clone();
            async move {
                let mut transaction = pool.begin().await.unwrap();
                writer_started_tx.send(()).unwrap();
                let identity_exists =
                    lock_participations_for_audit_insert(&mut transaction, &[1001])
                        .await
                        .unwrap();
                if identity_exists {
                    sqlx::query(
                        r#"INSERT INTO "FlagEgressEvents" (participation_id) VALUES (1001)"#,
                    )
                    .execute(&mut *transaction)
                    .await
                    .unwrap();
                }
                transaction.commit().await.unwrap();
                identity_exists
            }
        });
        writer_started_rx.await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut late_writer)
                .await
                .is_err(),
            "audit writer crossed cleanup's participation lock"
        );
        assert!(
            delete_unlinked_rejected_without_evidence(&mut cleanup, 1001)
                .await
                .unwrap()
        );
        cleanup.commit().await.unwrap();
        assert!(
            !tokio::time::timeout(std::time::Duration::from_secs(2), late_writer)
                .await
                .expect("audit writer remained blocked after cleanup commit")
                .expect("audit writer task failed"),
            "late audit writer did not observe the deleted identity"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"SELECT COUNT(*) FROM "FlagEgressEvents" WHERE participation_id = 1001"#,
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0,
            "late audit writer created an orphan row"
        );

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin_pool)
            .await
            .unwrap();
    }
}
