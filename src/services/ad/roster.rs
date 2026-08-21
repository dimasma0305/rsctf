//! Shared-roster eligibility for team-wide A&D bearer credentials.

use std::collections::HashSet;

use uuid::Uuid;

use crate::utils::enums::Role;
use crate::utils::error::{AppError, AppResult};

/// SQL predicate shared by every read-only admission path for a team-wide A&D
/// credential. Keeping the complete-roster rule in one macro prevents a new
/// single-statement query from silently drifting from token issuance.
macro_rules! shared_credential_team_predicate_sql {
    ($team:literal, $banned_parameter:literal) => {
        concat!(
            "NOT ",
            $team,
            r#".deletion_pending
       AND NOT EXISTS (
           SELECT 1
             FROM (
                 SELECT "#,
            $team,
            r#".captain_id AS user_id
                 UNION
                 SELECT member.user_id
                   FROM "TeamMembers" member
                  WHERE member.team_id = "#,
            $team,
            r#".id
             ) roster
             LEFT JOIN "AspNetUsers" account ON account.id = roster.user_id
            WHERE account.id IS NULL OR account.role = "#,
            $banned_parameter,
            "\n       )"
        )
    };
}

pub(crate) use shared_credential_team_predicate_sql;

const ELIGIBLE_SHARED_CREDENTIAL_TEAMS_SQL: &str = concat!(
    r#"
    SELECT team.id
      FROM "Teams" team
     WHERE team.id = ANY($1)
       AND "#,
    shared_credential_team_predicate_sql!("team", "$2"),
    "\n"
);

/// Return the requested teams whose complete roster is still eligible to use
/// shared A&D credentials. A missing/banned captain or member compromises a
/// team-wide bearer secret just as surely as the current caller being banned.
pub(crate) async fn eligible_shared_credential_teams(
    pool: &sqlx::PgPool,
    team_ids: &[i32],
) -> AppResult<HashSet<i32>> {
    if team_ids.is_empty() {
        return Ok(HashSet::new());
    }
    let ids: Vec<i32> = sqlx::query_scalar(ELIGIBLE_SHARED_CREDENTIAL_TEAMS_SQL)
        .bind(team_ids)
        .bind(Role::Banned as i16)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(ids.into_iter().collect())
}

/// Transaction-local variant used while a roster or game-control fence is
/// already held. Keeping the predicate in one constant prevents issuance and
/// request admission from drifting apart.
pub(crate) async fn eligible_shared_credential_teams_on(
    connection: &mut sqlx::PgConnection,
    team_ids: &[i32],
) -> AppResult<HashSet<i32>> {
    if team_ids.is_empty() {
        return Ok(HashSet::new());
    }
    let ids: Vec<i32> = sqlx::query_scalar(ELIGIBLE_SHARED_CREDENTIAL_TEAMS_SQL)
        .bind(team_ids)
        .bind(Role::Banned as i16)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(ids.into_iter().collect())
}

#[cfg(test)]
async fn team_allows_shared_credentials(pool: &sqlx::PgPool, team_id: i32) -> AppResult<bool> {
    Ok(eligible_shared_credential_teams(pool, &[team_id])
        .await?
        .contains(&team_id))
}

/// Lock every account that currently forms a team roster and verify that the
/// complete roster remains eligible for a shared credential. The caller owns
/// the team's roster advisory lock, so the roster id set cannot change between
/// these statements. Account role/deletion mutations conflict with the row
/// share locks and therefore cannot return while a fenced capability is live.
pub(crate) async fn lock_team_shared_credentials_on(
    connection: &mut sqlx::PgConnection,
    team_id: i32,
) -> AppResult<bool> {
    let roster_ids: Vec<Uuid> = sqlx::query_scalar(
        r#"SELECT roster.user_id
             FROM (
                   SELECT team.captain_id AS user_id
                     FROM "Teams" team
                    WHERE team.id = $1 AND NOT team.deletion_pending
                   UNION
                   SELECT member.user_id
                     FROM "TeamMembers" member
                     JOIN "Teams" team ON team.id = member.team_id
                    WHERE team.id = $1 AND NOT team.deletion_pending
             ) roster
            ORDER BY roster.user_id"#,
    )
    .bind(team_id)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if roster_ids.is_empty() {
        return Ok(false);
    }

    let accounts: Vec<(Uuid, i16)> = sqlx::query_as(
        r#"SELECT id, role
             FROM "AspNetUsers"
            WHERE id = ANY($1)
            ORDER BY id
            FOR SHARE"#,
    )
    .bind(&roster_ids)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(accounts.len() == roster_ids.len()
        && accounts
            .iter()
            .all(|(_, role)| *role != Role::Banned as i16))
}

/// Revalidate the interactive caller, participation, and complete team roster
/// on the transaction that owns the shared roster read fence.
pub(crate) async fn user_allows_shared_credentials_on(
    connection: &mut sqlx::PgConnection,
    user_id: Uuid,
    expected_security_stamp: &str,
    game_id: i32,
    team_id: i32,
    participation_id: i32,
) -> AppResult<bool> {
    // Lock the complete current roster first. Besides preventing a banned
    // teammate from using a team-wide bearer secret, the row locks keep the
    // caller's confirmed/stamp/role state stable through credential issuance.
    if !lock_team_shared_credentials_on(connection, team_id).await? {
        return Ok(false);
    }
    crate::services::live_roster::participation_caller_is_live_on(
        &mut *connection,
        user_id,
        expected_security_stamp,
        game_id,
        team_id,
        participation_id,
        true,
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use uuid::Uuid;

    use super::*;

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn shared_credentials_require_a_complete_live_roster() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_ad_roster_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin_pool)
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
            CREATE TABLE "AspNetUsers" (
              id UUID PRIMARY KEY, role SMALLINT NOT NULL,
              email_confirmed BOOLEAN NOT NULL, security_stamp TEXT
            );
            CREATE TABLE "Teams" (
              id INTEGER PRIMARY KEY,
              captain_id UUID NOT NULL,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "TeamMembers" (team_id INTEGER NOT NULL, user_id UUID NOT NULL);
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY, deletion_pending BOOLEAN NOT NULL,
              start_time_utc TIMESTAMPTZ NOT NULL, end_time_utc TIMESTAMPTZ NOT NULL
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

        let captain = Uuid::new_v4();
        let member = Uuid::new_v4();
        for id in [captain, member] {
            sqlx::query(
                r#"INSERT INTO "AspNetUsers"
                     (id, role, email_confirmed, security_stamp)
                   VALUES ($1, 1, TRUE, 'stamp')"#,
            )
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(r#"INSERT INTO "Teams" (id, captain_id) VALUES (7, $1)"#)
            .bind(captain)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "Games" VALUES
               (4, FALSE, clock_timestamp() + interval '1 hour',
                clock_timestamp() + interval '2 hours')"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES (7, $1)"#)
            .bind(member)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "Participations" (id, game_id, team_id, status)
               VALUES (17, 4, 7, $1)"#,
        )
        .bind(crate::utils::enums::ParticipationStatus::Accepted as i16)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "UserParticipations"
                 (user_id, game_id, team_id, participation_id)
               VALUES ($1, 4, 7, 17)"#,
        )
        .bind(member)
        .execute(&pool)
        .await
        .unwrap();

        assert!(team_allows_shared_credentials(&pool, 7).await.unwrap());
        {
            let mut connection = pool.acquire().await.unwrap();
            assert!(
                user_allows_shared_credentials_on(&mut connection, member, "stamp", 4, 7, 17,)
                    .await
                    .unwrap()
            );
        }
        sqlx::query(r#"UPDATE "AspNetUsers" SET security_stamp = 'rotated' WHERE id = $1"#)
            .bind(member)
            .execute(&pool)
            .await
            .unwrap();
        {
            let mut connection = pool.acquire().await.unwrap();
            assert!(
                !user_allows_shared_credentials_on(&mut connection, member, "stamp", 4, 7, 17,)
                    .await
                    .unwrap()
            );
        }
        sqlx::query(
            r#"UPDATE "AspNetUsers"
                  SET security_stamp = 'stamp', email_confirmed = FALSE
                WHERE id = $1"#,
        )
        .bind(member)
        .execute(&pool)
        .await
        .unwrap();
        {
            let mut connection = pool.acquire().await.unwrap();
            assert!(
                !user_allows_shared_credentials_on(&mut connection, member, "stamp", 4, 7, 17,)
                    .await
                    .unwrap()
            );
        }
        sqlx::query(r#"UPDATE "AspNetUsers" SET email_confirmed = TRUE WHERE id = $1"#)
            .bind(member)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"DELETE FROM "TeamMembers" WHERE team_id = 7 AND user_id = $1"#)
            .bind(member)
            .execute(&pool)
            .await
            .unwrap();
        // A legacy orphaned participation link is not current team authority.
        assert!(team_allows_shared_credentials(&pool, 7).await.unwrap());
        {
            let mut connection = pool.acquire().await.unwrap();
            assert!(
                !user_allows_shared_credentials_on(&mut connection, member, "stamp", 4, 7, 17,)
                    .await
                    .unwrap()
            );
        }
        sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES (7, $1)"#)
            .bind(member)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"UPDATE "AspNetUsers" SET role = $1 WHERE id = $2"#)
            .bind(Role::Banned as i16)
            .bind(member)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!team_allows_shared_credentials(&pool, 7).await.unwrap());
        sqlx::query(r#"UPDATE "AspNetUsers" SET role = 1 WHERE id = $1"#)
            .bind(member)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"DELETE FROM "AspNetUsers" WHERE id = $1"#)
            .bind(member)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!team_allows_shared_credentials(&pool, 7).await.unwrap());
        sqlx::query(
            r#"INSERT INTO "AspNetUsers"
                 (id, role, email_confirmed, security_stamp)
               VALUES ($1, 1, TRUE, 'stamp')"#,
        )
        .bind(member)
        .execute(&pool)
        .await
        .unwrap();
        assert!(team_allows_shared_credentials(&pool, 7).await.unwrap());
        sqlx::query(r#"UPDATE "Teams" SET deletion_pending = TRUE WHERE id = 7"#)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!team_allows_shared_credentials(&pool, 7).await.unwrap());

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin_pool)
            .await
            .unwrap();
    }
}
