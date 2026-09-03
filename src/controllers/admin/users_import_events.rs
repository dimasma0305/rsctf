//! Transactional event enrollment for teams created or updated by CSV import.

use std::collections::BTreeSet;

use uuid::Uuid;

use super::users::{ImportEventAssignment, ImportUserResult};
use super::*;
use crate::controllers::game::membership::{
    participation_status, persist_game_join_locked, JoinMutation,
};

#[derive(sqlx::FromRow)]
struct ImportEventPolicy {
    title: String,
    private_key: String,
    join_open: bool,
    member_limit: i32,
    scoring_started: bool,
    has_divisions: bool,
    selected_division_exists: bool,
}

async fn load_event_policy(
    connection: &mut sqlx::PgConnection,
    assignment: &ImportEventAssignment,
) -> AppResult<ImportEventPolicy> {
    sqlx::query_as::<_, ImportEventPolicy>(
        r#"SELECT game.title,
                  game.private_key,
                  game.practice_mode OR game.end_time_utc >= clock_timestamp() AS join_open,
                  game.team_member_count_limit AS member_limit,
                  game.ad_scoring_start_round IS NOT NULL
                    OR game.koth_scoring_start_round IS NOT NULL AS scoring_started,
                  EXISTS(SELECT 1 FROM "Divisions" candidate
                          WHERE candidate.game_id = game.id) AS has_divisions,
                  $2::INTEGER IS NOT NULL AND EXISTS(
                      SELECT 1 FROM "Divisions" selected
                       WHERE selected.game_id = game.id AND selected.id = $2
                  ) AS selected_division_exists
             FROM "Games" game
            WHERE game.id = $1 AND game.deletion_pending = FALSE
            FOR SHARE OF game"#,
    )
    .bind(assignment.game_id)
    .bind(assignment.division_id)
    .fetch_optional(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found(format!("Event #{} not found", assignment.game_id)))
}

fn validate_event_policy(
    assignment: &ImportEventAssignment,
    policy: &ImportEventPolicy,
) -> AppResult<()> {
    if !policy.join_open {
        return Err(AppError::bad_request(format!(
            "{} is no longer open for team enrollment",
            policy.title
        )));
    }
    match (
        policy.has_divisions,
        assignment.division_id,
        policy.selected_division_exists,
    ) {
        (true, None, _) => Err(AppError::bad_request(format!(
            "A division must be selected for {}",
            policy.title
        ))),
        (true, Some(_), false) | (false, Some(_), _) => Err(AppError::bad_request(format!(
            "The selected division is invalid for {}",
            policy.title
        ))),
        _ => Ok(()),
    }
}

/// Reject stale/deleted event or division selections before an import acquires
/// email leases or starts expensive password hashing.
pub(super) async fn validate_import_event_assignments(
    pool: &sqlx::PgPool,
    assignments: &[ImportEventAssignment],
) -> AppResult<()> {
    if assignments.is_empty() {
        return Ok(());
    }
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    for assignment in assignments {
        let policy = load_event_policy(&mut transaction, assignment).await?;
        validate_event_policy(assignment, &policy)?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

async fn imported_team_ids(pool: &sqlx::PgPool, rows: &[ImportUserResult]) -> AppResult<Vec<i32>> {
    let pairs = rows
        .iter()
        .filter(|row| row.status != "skipped")
        .filter_map(|row| row.user_id.zip(row.team_name.as_deref()))
        .collect::<Vec<_>>();
    if pairs.is_empty() {
        return Ok(Vec::new());
    }
    let user_ids = pairs
        .iter()
        .map(|(user_id, _)| *user_id)
        .collect::<Vec<_>>();
    let team_names = pairs
        .iter()
        .map(|(_, team_name)| (*team_name).to_string())
        .collect::<Vec<_>>();
    let resolved: Vec<(i64, i64, Option<i32>)> = sqlx::query_as(
        r#"SELECT imported.ordinality::BIGINT,
                  COUNT(team.id)::BIGINT,
                  MIN(team.id)
             FROM UNNEST($1::UUID[], $2::TEXT[]) WITH ORDINALITY
                    AS imported(user_id, team_name, ordinality)
        LEFT JOIN "Teams" team
               ON team.name = imported.team_name
              AND team.deletion_pending = FALSE
              AND (
                    team.captain_id = imported.user_id
                    OR EXISTS (
                        SELECT 1 FROM "TeamMembers" member
                         WHERE member.team_id = team.id
                           AND member.user_id = imported.user_id
                    )
              )
         GROUP BY imported.ordinality
         ORDER BY imported.ordinality"#,
    )
    .bind(&user_ids)
    .bind(&team_names)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if resolved.len() != pairs.len() {
        return Err(AppError::conflict(
            "An imported team assignment changed; retry the import",
        ));
    }
    let mut team_ids = BTreeSet::new();
    for (_, count, team_id) in resolved {
        if count != 1 {
            return Err(AppError::conflict(
                "An imported user does not have one unambiguous team assignment",
            ));
        }
        team_ids.insert(team_id.expect("one matching team has an id"));
    }
    Ok(team_ids.into_iter().collect())
}

async fn load_team_roster(
    connection: &mut sqlx::PgConnection,
    team_id: i32,
) -> AppResult<Vec<Uuid>> {
    sqlx::query_scalar(
        r#"SELECT roster.user_id
             FROM (
                   SELECT captain_id AS user_id FROM "Teams" WHERE id = $1
                   UNION
                   SELECT user_id FROM "TeamMembers" WHERE team_id = $1
                  ) roster
         ORDER BY roster.user_id"#,
    )
    .bind(team_id)
    .fetch_all(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

async fn enroll_team_in_event(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    team_id: i32,
    roster: &[Uuid],
    assignment: &ImportEventAssignment,
) -> AppResult<i32> {
    let policy = load_event_policy(transaction, assignment).await?;
    validate_event_policy(assignment, &policy)?;
    if policy.member_limit > 0
        && roster.len() > usize::try_from(policy.member_limit).unwrap_or_default()
    {
        return Err(AppError::bad_request(format!(
            "{} allows at most {} team members",
            policy.title, policy.member_limit
        )));
    }

    let existing: Option<(i32, i16, Option<i32>)> = sqlx::query_as(
        r#"SELECT id, status, division_id
             FROM "Participations"
            WHERE game_id = $1 AND team_id = $2
         ORDER BY id
            LIMIT 1
              FOR UPDATE"#,
    )
    .bind(assignment.game_id)
    .bind(team_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let mut target_participation_id = existing.map(|(participation_id, _, _)| participation_id);
    if let Some((participation_id, status, current_division)) = existing {
        let current_status = participation_status(status)?;
        if current_division != assignment.division_id {
            if policy.scoring_started
                || !matches!(
                    current_status,
                    ParticipationStatus::Pending | ParticipationStatus::Rejected
                )
            {
                return Err(AppError::bad_request(format!(
                    "{} already has this team in another division",
                    policy.title
                )));
            }
            crate::services::participation_evidence::ensure_evidence_preserving_update(
                &mut **transaction,
                participation_id,
                current_status,
                current_status,
                current_division,
                assignment.division_id,
            )
            .await?;
            sqlx::query(r#"UPDATE "Participations" SET division_id = $1 WHERE id = $2"#)
                .bind(assignment.division_id)
                .bind(participation_id)
                .execute(&mut **transaction)
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
        }
    }

    let token =
        crate::controllers::game::participation_token_from_key(&policy.private_key, team_id)?;
    for user_id in roster {
        let current: Option<i32> = sqlx::query_scalar(
            r#"SELECT participation_id
                 FROM "UserParticipations"
                WHERE user_id = $1 AND game_id = $2
                FOR UPDATE"#,
        )
        .bind(user_id)
        .bind(assignment.game_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if current.is_some() && current == target_participation_id {
            continue;
        }
        let persisted = persist_game_join_locked(
            transaction,
            JoinMutation {
                user_id: *user_id,
                game_id: assignment.game_id,
                team_id,
                division_id: assignment.division_id,
                target_status: ParticipationStatus::Accepted,
                token: &token,
                member_limit: policy.member_limit,
                scoring_started: policy.scoring_started,
            },
        )
        .await?;
        target_participation_id = Some(persisted.participation_id);
    }

    let (participation_id, status, division_id): (i32, i16, Option<i32>) = sqlx::query_as(
        r#"SELECT id, status, division_id
             FROM "Participations"
            WHERE game_id = $1 AND team_id = $2
         ORDER BY id
            LIMIT 1
              FOR UPDATE"#,
    )
    .bind(assignment.game_id)
    .bind(team_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let current_status = participation_status(status)?;
    if matches!(
        current_status,
        ParticipationStatus::Pending | ParticipationStatus::Rejected
    ) {
        crate::services::participation_evidence::ensure_evidence_preserving_update(
            &mut **transaction,
            participation_id,
            current_status,
            ParticipationStatus::Accepted,
            division_id,
            assignment.division_id,
        )
        .await?;
        crate::controllers::edit::ensure_ad_roster_status_mutable(
            policy.scoring_started,
            Some(current_status),
            ParticipationStatus::Accepted,
        )?;
        crate::services::ad::koth_api_capability::reconcile_pending_event_capabilities(
            transaction,
            assignment.game_id,
            None,
            Some(&[participation_id]),
        )
        .await?;
        sqlx::query(
            r#"UPDATE "Participations"
                  SET status = $1, division_id = $2
                WHERE id = $3 AND game_id = $4"#,
        )
        .bind(ParticipationStatus::Accepted as i16)
        .bind(assignment.division_id)
        .bind(participation_id)
        .bind(assignment.game_id)
        .execute(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }

    let accepted: bool =
        sqlx::query_scalar(r#"SELECT status = $2 FROM "Participations" WHERE id = $1"#)
            .bind(participation_id)
            .bind(ParticipationStatus::Accepted as i16)
            .fetch_one(&mut **transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    if accepted {
        sqlx::query(r#"UPDATE "Teams" SET locked = TRUE WHERE id = $1"#)
            .bind(team_id)
            .execute(&mut **transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        for user_id in roster {
            crate::services::anti_cheat::snapshot_recent_global_observations_for_game(
                transaction,
                *user_id,
                assignment.game_id,
                team_id,
                participation_id,
            )
            .await?;
        }
        crate::controllers::edit::enqueue_accepted_provisioning(
            &mut **transaction,
            assignment.game_id,
            participation_id,
        )
        .await?;
    }
    Ok(participation_id)
}

async fn enroll_team(
    st: &SharedState,
    team_id: i32,
    assignments: &[ImportEventAssignment],
) -> AppResult<Vec<(i32, i32)>> {
    let team_key = format!("team-roster:{team_id}");
    let _team_local = crate::utils::single_flight::coalesce(&team_key).await;
    let expected_roster = {
        let mut connection = st
            .pg()
            .acquire()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        load_team_roster(&mut connection, team_id).await?
    };
    if expected_roster.is_empty() {
        return Err(AppError::conflict("Imported team has no members"));
    }
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    for user_id in &expected_roster {
        crate::services::anti_cheat::lock_identity_user_scope(&mut transaction, *user_id).await?;
    }
    crate::utils::single_flight::acquire_transaction_advisory_lock(&mut transaction, &team_key)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let deletion_pending: bool =
        sqlx::query_scalar(r#"SELECT deletion_pending FROM "Teams" WHERE id = $1 FOR UPDATE"#)
            .bind(team_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
            .ok_or_else(|| AppError::conflict("Imported team no longer exists"))?;
    if deletion_pending {
        return Err(AppError::conflict("Imported team is being deleted"));
    }
    let live_roster = load_team_roster(&mut transaction, team_id).await?;
    if live_roster != expected_roster {
        return Err(AppError::conflict(
            "Imported team roster changed; retry the import",
        ));
    }

    for assignment in assignments {
        crate::utils::single_flight::acquire_transaction_advisory_lock(
            &mut transaction,
            &crate::services::ad_engine::game_lock_key(assignment.game_id),
        )
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    let mut enrolled = Vec::with_capacity(assignments.len());
    for assignment in assignments {
        let participation_id =
            enroll_team_in_event(&mut transaction, team_id, &live_roster, assignment).await?;
        enrolled.push((assignment.game_id, participation_id));
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(enrolled)
}

/// Enroll each successfully imported team in every selected event before the
/// durable import job is marked complete. Team/event pairs are idempotent, so a
/// crashed operation resumes any unfinished assignments without duplication.
pub(super) async fn enroll_imported_teams(
    st: &SharedState,
    assignments: &[ImportEventAssignment],
    rows: &[ImportUserResult],
) -> AppResult<usize> {
    if assignments.is_empty() {
        return Ok(0);
    }
    let mut assignments = assignments.to_vec();
    assignments.sort_by_key(|assignment| assignment.game_id);
    let team_ids = imported_team_ids(st.pg(), rows).await?;
    let mut enrolled = 0_usize;
    for team_id in team_ids {
        let participations = enroll_team(st, team_id, &assignments).await?;
        enrolled = enrolled.saturating_add(participations.len());
        let game_ids = participations
            .iter()
            .map(|(game_id, _)| *game_id)
            .collect::<Vec<_>>();
        crate::controllers::team::flush_scoreboards_for_games(st, &game_ids).await;
        for (game_id, participation_id) in participations {
            crate::controllers::game::ad::flush_participation_cache(st, game_id, participation_id)
                .await;
        }
    }
    Ok(enrolled)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use sqlx::{Connection, PgConnection};

    use super::*;

    #[test]
    fn divisions_are_required_only_for_events_that_define_them() {
        let base = ImportEventPolicy {
            title: "Qualifier".to_string(),
            private_key: "unused".to_string(),
            join_open: true,
            member_limit: 0,
            scoring_started: false,
            has_divisions: false,
            selected_division_exists: false,
        };
        let without_division = ImportEventAssignment {
            game_id: 7,
            division_id: None,
        };
        assert!(validate_event_policy(&without_division, &base).is_ok());

        let mut divided = base;
        divided.has_divisions = true;
        assert!(validate_event_policy(&without_division, &divided).is_err());
        let with_division = ImportEventAssignment {
            game_id: 7,
            division_id: Some(9),
        };
        divided.selected_division_exists = true;
        assert!(validate_event_policy(&with_division, &divided).is_ok());
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn event_enrollment_is_accepted_complete_and_idempotent() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let options = PgConnectOptions::from_str(&database_url).unwrap();
        let mut connection = PgConnection::connect_with(&options).await.unwrap();
        let schema = format!("rsctf_import_events_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(&format!(r#"SET search_path TO "{schema}""#))
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Teams" (
              id INTEGER PRIMARY KEY, name TEXT NOT NULL, captain_id UUID NOT NULL,
              locked BOOLEAN NOT NULL DEFAULT FALSE,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "TeamMembers" (
              team_id INTEGER NOT NULL, user_id UUID NOT NULL,
              PRIMARY KEY (team_id, user_id)
            );
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY, title TEXT NOT NULL, private_key TEXT NOT NULL,
              practice_mode BOOLEAN NOT NULL DEFAULT FALSE,
              start_time_utc TIMESTAMPTZ NOT NULL,
              end_time_utc TIMESTAMPTZ NOT NULL,
              team_member_count_limit INTEGER NOT NULL DEFAULT 0,
              ad_scoring_start_round INTEGER, koth_scoring_start_round INTEGER,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "Divisions" (id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL);
            CREATE TABLE "Participations" (
              id INTEGER GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
              status SMALLINT NOT NULL, token TEXT NOT NULL, writeup_id INTEGER,
              game_id INTEGER NOT NULL, team_id INTEGER NOT NULL, division_id INTEGER,
              suspicion_score INTEGER NOT NULL DEFAULT 0,
              competitive_admitted_at_utc TIMESTAMPTZ
            );
            CREATE TABLE "UserParticipations" (
              user_id UUID NOT NULL, game_id INTEGER NOT NULL, team_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL, PRIMARY KEY (user_id, game_id)
            );
            CREATE TABLE "ParticipationProvisionJobs" (
              participation_id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              attempts INTEGER NOT NULL, next_attempt_at TIMESTAMPTZ NOT NULL,
              lease_owner UUID, lease_until TIMESTAMPTZ, last_error TEXT,
              updated_at_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "SuspicionReconciliationState" (
              game_id INTEGER PRIMARY KEY, evidence_closed_at_utc TIMESTAMPTZ
            );
            CREATE TABLE "IdentityObservations" (
              id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
              user_id UUID NOT NULL, team_id INTEGER, game_id INTEGER,
              participation_id INTEGER, kind TEXT NOT NULL, value_hash BYTEA NOT NULL,
              subnet_group_hash BYTEA, broad_network_hash BYTEA, value_hint TEXT,
              source TEXT NOT NULL, observed_at_utc TIMESTAMPTZ NOT NULL
            );
            "#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        let captain = Uuid::new_v4();
        let member = Uuid::new_v4();
        let (_, private_key) = crate::utils::crypto_utils::generate_game_keypair();
        sqlx::query(r#"INSERT INTO "Teams" (id, name, captain_id) VALUES (10, 'Imported', $1)"#)
            .bind(captain)
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES (10, $1)"#)
            .bind(member)
            .execute(&mut connection)
            .await
            .unwrap();
        let scoped_options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let scoped_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(scoped_options)
            .await
            .unwrap();
        let imported_rows = [captain, member].map(|user_id| ImportUserResult {
            user_id: Some(user_id),
            email: format!("{user_id}@example.test"),
            real_name: "Imported Player".to_string(),
            user_name: user_id.simple().to_string(),
            password: "temporary".to_string(),
            team_name: Some("Imported".to_string()),
            status: "created".to_string(),
            error: None,
        });
        assert_eq!(
            imported_team_ids(&scoped_pool, &imported_rows)
                .await
                .unwrap(),
            vec![10]
        );
        scoped_pool.close().await;
        sqlx::query(
            r#"INSERT INTO "Games"
                 (id, title, private_key, start_time_utc, end_time_utc)
               VALUES (20, 'Warmup', $1, clock_timestamp() - INTERVAL '1 hour',
                       clock_timestamp() + INTERVAL '1 hour')"#,
        )
        .bind(private_key)
        .execute(&mut connection)
        .await
        .unwrap();
        let assignment = ImportEventAssignment {
            game_id: 20,
            division_id: None,
        };
        for _ in 0..2 {
            let mut transaction = connection.begin().await.unwrap();
            let participation_id =
                enroll_team_in_event(&mut transaction, 10, &[captain, member], &assignment)
                    .await
                    .unwrap();
            assert!(participation_id > 0);
            transaction.commit().await.unwrap();
        }
        let participation_count: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*)::BIGINT FROM "Participations"
                WHERE game_id = 20 AND team_id = 10 AND status = 1"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        let member_count: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*)::BIGINT FROM "UserParticipations" WHERE game_id = 20"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        let team_locked: bool = sqlx::query_scalar(r#"SELECT locked FROM "Teams" WHERE id = 10"#)
            .fetch_one(&mut connection)
            .await
            .unwrap();
        let job_count: i64 =
            sqlx::query_scalar(r#"SELECT COUNT(*)::BIGINT FROM "ParticipationProvisionJobs""#)
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!(participation_count, 1);
        assert_eq!(member_count, 2);
        assert!(team_locked);
        assert_eq!(job_count, 1);

        sqlx::query("SET search_path TO public")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&mut connection)
            .await
            .unwrap();
    }
}
