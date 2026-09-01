//! Bounded current-user team projections.
//!
//! Team mutations still load the exact row they are changing. These list reads
//! avoid one roster query per team and keep the event join selector free of
//! unrelated profile data.

use std::collections::BTreeMap;

use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::{TeamInfoModel, TeamSelectorInfoModel, TeamUserInfoModel};
use crate::utils::error::{AppError, AppResult};

const MAX_USER_TEAMS: i64 = 100;
const MAX_MEMBERS_PER_TEAM: i64 = 100;

#[derive(Debug, FromRow)]
struct TeamProjectionRow {
    team_id: i32,
    team_name: String,
    team_bio: Option<String>,
    team_avatar_hash: Option<String>,
    team_locked: bool,
    profile_revision: i64,
    member_id: Option<Uuid>,
    user_name: Option<String>,
    member_avatar_hash: Option<String>,
    captain: Option<bool>,
}

#[derive(Debug, FromRow)]
struct TeamSelectorRow {
    id: i32,
    name: String,
    captain: bool,
}

const USER_TEAMS_SQL: &str = r#"
WITH eligible_ids AS MATERIALIZED (
    SELECT team.id AS team_id
      FROM "Teams" team
     WHERE team.captain_id = $1 AND team.deletion_pending = FALSE
    UNION
    SELECT member.team_id
      FROM "TeamMembers" member
      JOIN "Teams" team ON team.id = member.team_id
     WHERE member.user_id = $1 AND team.deletion_pending = FALSE
), eligible_teams AS MATERIALIZED (
    SELECT team.id, team.name, team.bio, team.avatar_hash, team.locked,
           team.profile_revision, team.captain_id
      FROM eligible_ids
      JOIN "Teams" team ON team.id = eligible_ids.team_id
     ORDER BY team.id
     LIMIT $2
), eligible_members AS (
    SELECT team.id AS team_id, team.captain_id AS user_id, TRUE AS captain
      FROM eligible_teams team
    UNION
    SELECT member.team_id, member.user_id,
           member.user_id = team.captain_id AS captain
      FROM "TeamMembers" member
      JOIN eligible_teams team ON team.id = member.team_id
), ranked AS (
    SELECT eligible_members.*,
           ROW_NUMBER() OVER (
               PARTITION BY eligible_members.team_id
               ORDER BY eligible_members.captain DESC, eligible_members.user_id
           ) AS member_rank
      FROM eligible_members
)
SELECT team.id AS team_id, team.name AS team_name, team.bio AS team_bio,
       team.avatar_hash AS team_avatar_hash, team.locked AS team_locked,
       team.profile_revision,
       account.id AS member_id, account.user_name,
       account.avatar_hash AS member_avatar_hash, ranked.captain
  FROM eligible_teams team
  LEFT JOIN ranked
    ON ranked.team_id = team.id AND ranked.member_rank <= $3
  LEFT JOIN "AspNetUsers" account ON account.id = ranked.user_id
 ORDER BY team.id, ranked.captain DESC NULLS LAST, account.id
"#;

const TEAM_SELECTOR_SQL: &str = r#"
WITH eligible_ids AS MATERIALIZED (
    SELECT team.id AS team_id
      FROM "Teams" team
     WHERE team.captain_id = $1 AND team.deletion_pending = FALSE
    UNION
    SELECT member.team_id
      FROM "TeamMembers" member
      JOIN "Teams" team ON team.id = member.team_id
     WHERE member.user_id = $1 AND team.deletion_pending = FALSE
)
SELECT team.id, team.name, team.captain_id = $1 AS captain
  FROM eligible_ids
  JOIN "Teams" team ON team.id = eligible_ids.team_id
 ORDER BY team.id
 LIMIT $2
"#;

fn avatar_url(hash: Option<String>) -> Option<String> {
    hash.map(|hash| format!("/assets/{hash}/avatar"))
}

pub(super) async fn load_user_team_infos(
    pool: &PgPool,
    user_id: Uuid,
) -> AppResult<Vec<TeamInfoModel>> {
    let rows = sqlx::query_as::<_, TeamProjectionRow>(USER_TEAMS_SQL)
        .bind(user_id)
        .bind(MAX_USER_TEAMS)
        .bind(MAX_MEMBERS_PER_TEAM)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let mut teams = BTreeMap::<i32, TeamInfoModel>::new();
    for row in rows {
        let team = teams.entry(row.team_id).or_insert_with(|| TeamInfoModel {
            id: row.team_id,
            name: row.team_name,
            bio: row.team_bio,
            avatar: avatar_url(row.team_avatar_hash),
            locked: row.team_locked,
            profile_revision: row.profile_revision,
            members: Some(Vec::new()),
        });
        let Some(member_id) = row.member_id else {
            continue;
        };
        team.members
            .as_mut()
            .expect("current-user team projections always include a roster")
            .push(TeamUserInfoModel {
                id: member_id,
                user_name: row.user_name,
                bio: None,
                avatar: avatar_url(row.member_avatar_hash),
                captain: row.captain.unwrap_or(false),
                real_name: String::new(),
                student_number: String::new(),
            });
    }
    Ok(teams.into_values().collect())
}

pub(super) async fn load_team_selector(
    pool: &PgPool,
    user_id: Uuid,
) -> AppResult<Vec<TeamSelectorInfoModel>> {
    sqlx::query_as::<_, TeamSelectorRow>(TEAM_SELECTOR_SQL)
        .bind(user_id)
        .bind(MAX_USER_TEAMS)
        .fetch_all(pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| TeamSelectorInfoModel {
                    id: row.id,
                    name: row.name,
                    captain: row.captain,
                })
                .collect()
        })
        .map_err(|error| AppError::internal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;

    #[test]
    fn current_user_team_reads_are_fixed_query_and_response_bounded() {
        assert!(USER_TEAMS_SQL.contains("LIMIT $2"));
        assert!(USER_TEAMS_SQL.contains("ROW_NUMBER() OVER"));
        assert!(USER_TEAMS_SQL.contains("member_rank <= $3"));
        assert!(TEAM_SELECTOR_SQL.contains("LIMIT $2"));
        assert!(!TEAM_SELECTOR_SQL.contains("AspNetUsers"));
        assert_eq!(MAX_USER_TEAMS, 100);
        assert_eq!(MAX_MEMBERS_PER_TEAM, 100);
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn selector_enforces_membership_and_caps_large_team_histories() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect test database");
        let schema = format!("team_reads_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .expect("create isolated schema");
        let options = PgConnectOptions::from_str(&database_url)
            .expect("parse database URL")
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .expect("connect isolated schema");

        sqlx::raw_sql(
            r#"
            CREATE TABLE "Teams" (
              id INTEGER PRIMARY KEY, name TEXT NOT NULL, bio TEXT,
              avatar_hash TEXT, locked BOOLEAN NOT NULL,
              deletion_pending BOOLEAN NOT NULL, captain_id UUID NOT NULL,
              profile_revision BIGINT NOT NULL DEFAULT 0
            );
            CREATE TABLE "TeamMembers" (
              id SERIAL PRIMARY KEY, team_id INTEGER NOT NULL, user_id UUID NOT NULL
            );
            CREATE TABLE "AspNetUsers" (
              id UUID PRIMARY KEY, user_name TEXT, avatar_hash TEXT
            );
            "#,
        )
        .execute(&pool)
        .await
        .expect("create team fixture tables");
        let player = Uuid::new_v4();
        let other = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "AspNetUsers" VALUES ($1, 'player', NULL), ($2, 'other', NULL)"#,
        )
        .bind(player)
        .bind(other)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "Teams"
                 (id, name, bio, avatar_hash, locked, deletion_pending, captain_id)
               SELECT n, 'Owned ' || n, NULL, NULL, FALSE, FALSE, $1
                 FROM generate_series(1, 101) n"#,
        )
        .bind(player)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "Teams" VALUES
               (0, 'Member team', NULL, NULL, FALSE, FALSE, $1, 0),
               (999, 'Private team', NULL, NULL, FALSE, FALSE, $1, 0)"#,
        )
        .bind(other)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES (0, $1)"#)
            .bind(player)
            .execute(&pool)
            .await
            .unwrap();

        let selector = load_team_selector(&pool, player).await.unwrap();
        assert_eq!(selector.len(), MAX_USER_TEAMS as usize);
        assert_eq!(selector[0].id, 0);
        assert!(!selector[0].captain);
        assert!(!selector.iter().any(|team| team.id == 999));

        let teams = load_user_team_infos(&pool, player).await.unwrap();
        assert_eq!(teams.len(), MAX_USER_TEAMS as usize);
        assert!(teams.iter().all(|team| {
            team.members
                .as_ref()
                .is_some_and(|members| members.len() <= MAX_MEMBERS_PER_TEAM as usize)
        }));

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .expect("drop isolated schema");
        admin.close().await;
    }
}
