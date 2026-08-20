//! Canonical live team-roster authorization.
//!
//! `UserParticipations` is intentionally retained after a user leaves a team so
//! historical submissions and anti-cheat evidence keep their actor. It is not a
//! live membership table. Every interactive game authorization must additionally
//! require the user to be the current captain or have a current `TeamMembers` row.

use uuid::Uuid;

use crate::utils::enums::{ParticipationStatus, Role};
use crate::utils::error::{AppError, AppResult};

/// Stable identity of one authenticated caller's historical participation.
///
/// Keeping these fields together prevents sensitive final-boundary helpers from
/// accepting mismatched game, team, participation, or session-stamp arguments.
#[derive(Clone, Copy)]
pub(crate) struct LiveParticipationIdentity<'a> {
    pub(crate) user_id: Uuid,
    pub(crate) expected_security_stamp: &'a str,
    pub(crate) game_id: i32,
    pub(crate) team_id: i32,
    pub(crate) participation_id: i32,
}

pub(crate) fn lock_key(team_id: i32) -> String {
    format!("team-roster:{team_id}")
}

/// Take a non-blocking shared roster fence and validate the exact caller while
/// retaining that transaction for a sensitive read or write. `None` is a
/// fail-closed denial, including an in-progress exclusive roster mutation.
pub(crate) async fn try_acquire_participation_fence(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    expected_security_stamp: &str,
    game_id: i32,
    team_id: i32,
    participation_id: i32,
    accepted_only: bool,
) -> AppResult<Option<crate::utils::single_flight::PgAdvisoryLock>> {
    let Some(mut roster) =
        crate::utils::single_flight::PgAdvisoryLock::try_acquire_shared(pool, &lock_key(team_id))
            .await?
    else {
        return Ok(None);
    };
    if !participation_caller_is_live_on(
        &mut **roster.transaction_mut(),
        user_id,
        expected_security_stamp,
        game_id,
        team_id,
        participation_id,
        accepted_only,
    )
    .await?
    {
        roster.release().await?;
        return Ok(None);
    }
    Ok(Some(roster))
}

/// Revalidate an exact historical participation link against the current roster.
///
/// A single direct call is the authoritative point-in-time predicate for ordinary
/// reads. Sensitive reads and writes must pass a transaction that already holds
/// `team-roster:{team_id}` in shared mode (normally via
/// [`try_acquire_participation_fence`]); roster mutations take its exclusive form,
/// keeping the decision valid through the protected operation.
pub(crate) async fn participation_caller_is_live_on<'e, E>(
    executor: E,
    user_id: Uuid,
    expected_security_stamp: &str,
    game_id: i32,
    team_id: i32,
    participation_id: i32,
    accepted_only: bool,
) -> AppResult<bool>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    // `2` is deliberately distinct from an ordinary roster denial: an
    // authenticated player who crossed into the live window without a scoped
    // login/join observation must be sent through authentication again, not
    // told that their durable participation disappeared.
    let decision: i16 = sqlx::query_scalar(
        r#"WITH observed_now AS (
               SELECT clock_timestamp() AS at
           )
           SELECT COALESCE((
               SELECT CASE
                        WHEN participation.status IN ($8, $9)
                         AND observed_now.at >= game.start_time_utc
                         AND observed_now.at < game.end_time_utc
                         AND NOT EXISTS (
                             SELECT 1
                               FROM "IdentityObservations" identity
                              WHERE identity.user_id = historical.user_id
                                AND identity.game_id = historical.game_id
                                AND identity.team_id = historical.team_id
                                AND identity.participation_id = historical.participation_id
                                AND identity.observed_at_utc >= game.start_time_utc
                                AND identity.observed_at_utc < game.end_time_utc
                                AND identity.observed_at_utc <= observed_now.at
                         )
                        THEN 2
                        ELSE 1
                      END::SMALLINT
                 FROM "UserParticipations" historical
                 JOIN "Participations" participation
                   ON participation.id = historical.participation_id
                  AND participation.game_id = historical.game_id
                  AND participation.team_id = historical.team_id
                 JOIN "Teams" team ON team.id = participation.team_id
                 JOIN "Games" game ON game.id = participation.game_id
                 JOIN "AspNetUsers" account ON account.id = historical.user_id
                CROSS JOIN observed_now
                WHERE historical.user_id = $1
                  AND historical.game_id = $2
                  AND historical.team_id = $3
                  AND historical.participation_id = $4
                  AND game.deletion_pending = FALSE
                  AND team.deletion_pending = FALSE
                  AND account.role <> $5
                  AND account.email_confirmed = TRUE
                  AND account.security_stamp = $6
                  AND (NOT $7 OR participation.status = $8)
                  AND (
                       team.captain_id = historical.user_id
                       OR EXISTS (
                           SELECT 1
                             FROM "TeamMembers" live_member
                            WHERE live_member.team_id = team.id
                              AND live_member.user_id = historical.user_id
                       )
                  )
                  FOR SHARE OF historical, participation, team, game, account
           ), 0::SMALLINT)"#,
    )
    .bind(user_id)
    .bind(game_id)
    .bind(team_id)
    .bind(participation_id)
    .bind(Role::Banned as i16)
    .bind(expected_security_stamp)
    .bind(accepted_only)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(ParticipationStatus::Suspended as i16)
    .fetch_one(executor)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    match decision {
        1 => Ok(true),
        2 => Err(AppError::Unauthorized),
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use uuid::Uuid;

    use super::*;

    #[test]
    fn retained_history_is_never_the_live_roster_predicate() {
        let source = include_str!("live_roster.rs");
        assert!(source.contains("team.captain_id = historical.user_id"));
        assert!(source.contains("FROM \"TeamMembers\" live_member"));
        assert!(source.contains("FROM \"UserParticipations\" historical"));
        assert!(source.contains("account.email_confirmed = TRUE"));
        assert!(source.contains("account.security_stamp = $6"));
        assert!(source.contains("participation.status IN ($8, $9)"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn active_window_requires_an_exact_scoped_identity_observation() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("live_roster_identity_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY, deletion_pending BOOLEAN NOT NULL,
              start_time_utc TIMESTAMPTZ NOT NULL, end_time_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "Teams" (
              id INTEGER PRIMARY KEY, captain_id UUID NOT NULL,
              deletion_pending BOOLEAN NOT NULL
            );
            CREATE TABLE "TeamMembers" (team_id INTEGER NOT NULL, user_id UUID NOT NULL);
            CREATE TABLE "AspNetUsers" (
              id UUID PRIMARY KEY, role SMALLINT NOT NULL,
              email_confirmed BOOLEAN NOT NULL, security_stamp TEXT
            );
            CREATE TABLE "Participations" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL, status SMALLINT NOT NULL
            );
            CREATE TABLE "UserParticipations" (
              user_id UUID NOT NULL, game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL, participation_id INTEGER NOT NULL
            );
            CREATE TABLE "IdentityObservations" (
              user_id UUID NOT NULL, game_id INTEGER,
              team_id INTEGER, participation_id INTEGER,
              observed_at_utc TIMESTAMPTZ NOT NULL
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let user_id = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "Games" VALUES
               (1, FALSE, clock_timestamp() + interval '1 hour',
                clock_timestamp() + interval '2 hours')"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(r#"INSERT INTO "Teams" VALUES (2, $1, FALSE)"#)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "TeamMembers" VALUES (2, $1)"#)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "AspNetUsers" VALUES ($1, 1, TRUE, 'stamp')"#)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "Participations" VALUES (3, 1, 2, $1)"#)
            .bind(ParticipationStatus::Accepted as i16)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "UserParticipations" VALUES ($1, 1, 2, 3)"#)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();

        assert!(
            participation_caller_is_live_on(&pool, user_id, "stamp", 1, 2, 3, true,)
                .await
                .unwrap()
        );
        sqlx::query(
            r#"UPDATE "Games"
                  SET start_time_utc = clock_timestamp() - interval '1 minute',
                      end_time_utc = clock_timestamp() + interval '1 hour'
                WHERE id = 1"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(matches!(
            participation_caller_is_live_on(&pool, user_id, "stamp", 1, 2, 3, true).await,
            Err(AppError::Unauthorized)
        ));

        // Pending and rejected registrations are roster mutations, not live
        // play identities. They may be withdrawn during an active review
        // window even though observations are only scoped to admitted players.
        for status in [ParticipationStatus::Pending, ParticipationStatus::Rejected] {
            sqlx::query(r#"UPDATE "Participations" SET status = $1 WHERE id = 3"#)
                .bind(status as i16)
                .execute(&pool)
                .await
                .unwrap();
            assert!(
                participation_caller_is_live_on(&pool, user_id, "stamp", 1, 2, 3, false)
                    .await
                    .unwrap()
            );
        }
        sqlx::query(r#"UPDATE "Participations" SET status = $1 WHERE id = 3"#)
            .bind(ParticipationStatus::Accepted as i16)
            .execute(&pool)
            .await
            .unwrap();

        // A global or wrong-participation observation cannot unlock gameplay.
        sqlx::query(
            r#"INSERT INTO "IdentityObservations"
                 (user_id, game_id, team_id, participation_id, observed_at_utc)
               VALUES ($1, NULL, NULL, NULL, clock_timestamp())"#,
        )
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
        assert!(matches!(
            participation_caller_is_live_on(&pool, user_id, "stamp", 1, 2, 3, true).await,
            Err(AppError::Unauthorized)
        ));
        sqlx::query(
            r#"INSERT INTO "IdentityObservations"
                 (user_id, game_id, team_id, participation_id, observed_at_utc)
               VALUES ($1, 1, 2, 3, clock_timestamp())"#,
        )
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            participation_caller_is_live_on(&pool, user_id, "stamp", 1, 2, 3, true,)
                .await
                .unwrap()
        );

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
