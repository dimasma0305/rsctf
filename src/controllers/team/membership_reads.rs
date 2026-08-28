use super::*;

const MAX_USER_TEAMS: i64 = 100;

#[derive(sqlx::FromRow)]
struct UserTeamRow {
    id: i32,
    name: String,
    bio: Option<String>,
    avatar_hash: Option<String>,
    locked: bool,
    profile_revision: i64,
}

#[derive(sqlx::FromRow)]
struct UserTeamRosterRow {
    team_id: i32,
    id: Uuid,
    user_name: Option<String>,
    bio: String,
    avatar_hash: Option<String>,
    captain: bool,
    real_name: String,
    student_number: String,
}

const USER_TEAMS_SQL: &str = r#"SELECT team.id, team.name, team.bio, team.avatar_hash, team.locked,
                                      team.profile_revision
       FROM "Teams" team
      WHERE team.deletion_pending = FALSE
        AND (
            team.captain_id = $1
            OR EXISTS (
                SELECT 1 FROM "TeamMembers" member
                 WHERE member.team_id = team.id AND member.user_id = $1
            )
        )
      ORDER BY team.id
      LIMIT $2"#;

const USER_TEAM_ROSTERS_SQL: &str = r#"WITH roster_ids AS (
        SELECT team.id AS team_id, team.captain_id AS user_id, TRUE AS captain
          FROM "Teams" team
         WHERE team.id = ANY($1) AND team.deletion_pending = FALSE
        UNION ALL
        SELECT member.team_id, member.user_id, FALSE AS captain
          FROM "TeamMembers" member
          JOIN "Teams" team ON team.id = member.team_id
         WHERE member.team_id = ANY($1)
           AND member.user_id <> team.captain_id
           AND team.deletion_pending = FALSE
    ), bounded AS (
        SELECT roster_ids.*,
               ROW_NUMBER() OVER (
                   PARTITION BY roster_ids.team_id
                   ORDER BY roster_ids.captain DESC, roster_ids.user_id
               ) AS ordinal
          FROM roster_ids
    )
    SELECT bounded.team_id, account.id, account.user_name, account.bio,
           account.avatar_hash, bounded.captain, account.real_name,
           account.std_number AS student_number
      FROM bounded
      JOIN "AspNetUsers" account ON account.id = bounded.user_id
     WHERE bounded.ordinal <= $2
     ORDER BY bounded.team_id, bounded.captain DESC, account.id"#;

/// Full roster detail for the dedicated team workspace; two bounded queries.
pub async fn get_teams_info(
    State(st): State<SharedState>,
    user: CurrentUser,
) -> AppResult<RequestResponse<Vec<TeamInfoModel>>> {
    let teams = sqlx::query_as::<_, UserTeamRow>(USER_TEAMS_SQL)
        .bind(user.id)
        .bind(MAX_USER_TEAMS)
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let team_ids = teams.iter().map(|team| team.id).collect::<Vec<_>>();
    let roster_rows = if team_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as::<_, UserTeamRosterRow>(USER_TEAM_ROSTERS_SQL)
            .bind(&team_ids)
            .bind(MAX_TEAM_MEMBERS as i64)
            .fetch_all(st.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
    };
    let mut rosters = std::collections::BTreeMap::<i32, Vec<TeamUserInfoModel>>::new();
    for row in roster_rows {
        rosters
            .entry(row.team_id)
            .or_default()
            .push(TeamUserInfoModel {
                id: row.id,
                user_name: row.user_name,
                bio: Some(row.bio),
                avatar: row.avatar_hash.map(|hash| format!("/assets/{hash}/avatar")),
                captain: row.captain,
                real_name: row.real_name,
                student_number: row.student_number,
            });
    }
    let out = teams
        .into_iter()
        .map(|team| TeamInfoModel {
            id: team.id,
            name: team.name,
            bio: team.bio,
            avatar: team
                .avatar_hash
                .map(|hash| format!("/assets/{hash}/avatar")),
            locked: team.locked,
            profile_revision: team.profile_revision,
            members: Some(rosters.remove(&team.id).unwrap_or_default()),
        })
        .collect();
    Ok(RequestResponse::ok(out))
}

/// Compact selector identities; never loads member profiles.
pub async fn get_team_selector(
    State(st): State<SharedState>,
    user: CurrentUser,
) -> AppResult<RequestResponse<Vec<TeamSelectorModel>>> {
    let teams = sqlx::query_as::<_, UserTeamRow>(USER_TEAMS_SQL)
        .bind(user.id)
        .bind(MAX_USER_TEAMS)
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .into_iter()
        .map(|team| TeamSelectorModel {
            id: team.id,
            name: team.name,
        })
        .collect();
    Ok(RequestResponse::ok(teams))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_are_bounded_and_rosters_are_batched() {
        assert!(USER_TEAMS_SQL.contains("LIMIT $2"));
        assert!(USER_TEAMS_SQL.contains("EXISTS ("));
        assert!(USER_TEAM_ROSTERS_SQL.contains("team.id = ANY($1)"));
        assert!(USER_TEAM_ROSTERS_SQL.contains("ROW_NUMBER() OVER"));
        assert!(USER_TEAM_ROSTERS_SQL.contains("bounded.ordinal <= $2"));
        assert_eq!(MAX_USER_TEAMS, 100);
        assert_eq!(MAX_TEAM_MEMBERS, 100);
    }
}
