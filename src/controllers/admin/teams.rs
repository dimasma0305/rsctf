//! Team listing / search / CRUD.

use super::*;
use std::collections::HashMap;

use sea_orm::sea_query::{Expr, Func};

/// RSCTF `TeamModel` — compact nested team reference.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamModel {
    pub id: i32,
    pub name: String,
    pub avatar: Option<String>,
}

/// RSCTF `TeamInfoModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamInfoModel {
    pub id: i32,
    pub name: String,
    pub bio: Option<String>,
    pub avatar: Option<String>,
    pub locked: bool,
    pub members: Vec<Value>,
}

impl From<team::Model> for TeamInfoModel {
    fn from(t: team::Model) -> Self {
        Self {
            id: t.id,
            avatar: t.avatar_url(),
            name: t.name,
            bio: t.bio,
            locked: t.locked,
            members: Vec::new(),
        }
    }
}

/// Admin team-mutation body (RSCTF `AdminTeamModel`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminTeamModel {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub bio: Option<String>,
    #[serde(default)]
    pub locked: Option<bool>,
}

/// `GET /api/admin/teams` — paginated team listing.
pub async fn teams(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(q): Query<ListQuery>,
) -> AppResult<ArrayResponse<TeamInfoModel>> {
    let count = q.count.clamp(0, 500);
    let total = team::Entity::find().count(&st.db).await? as i64;
    let rows = team::Entity::find()
        .order_by_asc(team::Column::Id)
        .offset(q.skip)
        .limit(count)
        .all(&st.db)
        .await?;

    let data = teams_with_members(st.pg(), rows).await?;
    Ok(ArrayResponse::new(data, total))
}

/// `POST /api/admin/teams/search` — case-insensitive substring search over the
/// team name, plus an exact match on the numeric id when the hint parses as an
/// integer. Mirrors RSCTF `TeamRepository.SearchTeams` (`Name.ToLower().Contains`
/// OR `Id == id`).
pub async fn search_teams(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(model): Query<SearchModel>,
) -> AppResult<ArrayResponse<TeamInfoModel>> {
    let hint = model.hint;
    let hint = hint.trim();
    let pat = format!("%{}%", hint.to_lowercase());
    let mut cond = Condition::any()
        .add(Expr::expr(Func::lower(team::Column::Name.into_expr())).like(pat.as_str()));
    if let Ok(id) = hint.parse::<i32>() {
        cond = cond.add(team::Column::Id.eq(id));
    }
    let rows = team::Entity::find()
        .filter(cond)
        .order_by_asc(team::Column::Id)
        .limit(30)
        .all(&st.db)
        .await?;

    let data = teams_with_members(st.pg(), rows).await?;
    let total = data.len() as i64;
    Ok(ArrayResponse::new(data, total))
}

/// `PUT /api/admin/teams/{id}` — mutate name / bio / locked.
pub async fn update_team(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
    Json(model): Json<AdminTeamModel>,
) -> AppResult<MessageResponse> {
    let mut roster = crate::controllers::team::acquire_roster_mutation(st.pg(), id).await?;
    crate::controllers::team::require_team_mutable(roster.transaction_mut(), id).await?;
    let t = team::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Team not found"))?;
    let old_name = t.name.clone();

    let mut am: team::ActiveModel = t.into();
    if let Some(name) = model.name {
        let name = name.trim().to_string();
        if !name.is_empty() {
            am.name = Set(name);
        }
    }
    if let Some(bio) = model.bio {
        am.bio = Set(Some(bio));
    }
    if let Some(locked) = model.locked {
        am.locked = Set(locked);
    }
    let updated = am.update(&st.db).await?;
    roster.release().await?;
    if updated.name != old_name {
        crate::controllers::team::flush_scoreboard_for_team(&st, updated.id).await?;
    }
    Ok(MessageResponse::ok(""))
}

/// `DELETE /api/admin/teams/{id}` — remove a team (best-effort), returning the
/// team id as a string (RSCTF `string` success).
pub async fn delete_team(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<String>> {
    let roster_key = format!("team-roster:{id}");
    let mut initial = crate::controllers::team::acquire_roster_mutation(st.pg(), id).await?;
    let team = team::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Team not found"))?;
    let affected_game_ids = crate::controllers::team::team_game_ids(&st, team.id).await?;

    crate::controllers::team::mark_team_participations_revoked(initial.advisory_mut(), team.id)
        .await?;
    // The suspension is the durable authorization fence. Commit it before
    // capability teardown takes independent game/VPN locks.
    let _roster_guard = initial.release_for_external().await?;
    let Some(deletion_lease) =
        crate::controllers::team::TeamDeletionLease::acquire(st.pg(), &roster_key, team.id).await?
    else {
        return Ok(RequestResponse::ok(id.to_string()));
    };
    // Make the durable suspension visible to cached session reads before the
    // slower external teardown begins.
    crate::controllers::game::ad::flush_team_participation_cache(&st, team.id).await;
    // Revoke team-shared API/SSH/VPN/BYOC capabilities before their ownership
    // rows disappear. These tables are not all FK-cascaded, and live network
    // sessions otherwise outlive an admin deletion.
    crate::controllers::team::revoke_team_shared_capabilities(&st, team.id).await?;

    // RSCTF `TeamRepository.DeleteTeam` (both admin and non-admin delete route
    // through it): reap the team's live per-team containers and evict the affected
    // games' scoreboard caches BEFORE the cascade drops the participation/instance
    // rows the teardown keys off — otherwise the containers leak until the reaper
    // and the deleted team lingers on the cached board. Reuses the team-controller
    // helpers so the two delete paths stay in step. Runtime teardown is fail-closed;
    // cache eviction remains best-effort because the cache is not authoritative.
    crate::controllers::team::destroy_team_containers(&st, team.id).await?;
    crate::controllers::team::flush_scoreboard_for_team(&st, team.id).await?;

    deletion_lease.finalize(team.id).await?;
    crate::controllers::team::flush_scoreboards_for_games(&st, &affected_game_ids).await;
    Ok(RequestResponse::ok(id.to_string()))
}

#[derive(Debug, sqlx::FromRow)]
struct TeamMemberProjection {
    team_id: i32,
    id: Uuid,
    user_name: Option<String>,
    bio: String,
    avatar_hash: Option<String>,
    captain: bool,
}

const TEAM_MEMBER_PROJECTION_SQL: &str = r#"
    WITH roster AS (
        SELECT team.id AS team_id,
               team.captain_id,
               team.captain_id AS user_id
          FROM "Teams" team
         WHERE team.id = ANY($1)
        UNION
        SELECT team.id AS team_id,
               team.captain_id,
               member.user_id
          FROM "Teams" team
          JOIN "TeamMembers" member ON member.team_id = team.id
         WHERE team.id = ANY($1)
    )
    SELECT roster.team_id,
           account.id,
           account.user_name,
           account.bio,
           account.avatar_hash,
           account.id = roster.captain_id AS captain
      FROM roster
      JOIN "AspNetUsers" account ON account.id = roster.user_id
     ORDER BY roster.team_id, account.id
"#;

/// Resolve a page of team rosters in the client `TeamUserInfoModel` shape
/// (`id`/`userName`/`bio`/`avatar`/`captain`). Mirrors RSCTF
/// `TeamInfoModel.FromTeam`'s `Members` projection — which includes the captain
/// (seeded into `Team.Members` on create), so we union the `team_member` rows with
/// `captain_id`. `realName`/`studentNumber` are `[JsonIgnore]` in RSCTF and are
/// intentionally omitted here too. The narrow projection is deliberate: a
/// roster render must not decode unrelated identity timestamps (legacy
/// PostgreSQL `infinity` values cannot be represented by Chrono). Loading the
/// whole page in one query also avoids two sequential queries per team.
async fn teams_with_members(
    pool: &sqlx::PgPool,
    teams: Vec<team::Model>,
) -> AppResult<Vec<TeamInfoModel>> {
    let team_ids = teams.iter().map(|team| team.id).collect::<Vec<_>>();
    let rows = if team_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as::<_, TeamMemberProjection>(TEAM_MEMBER_PROJECTION_SQL)
            .bind(&team_ids)
            .fetch_all(pool)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
    };
    let mut members = HashMap::<i32, Vec<Value>>::with_capacity(team_ids.len());
    for row in rows {
        let avatar = row.avatar_hash.map(|hash| format!("/assets/{hash}/avatar"));
        members
            .entry(row.team_id)
            .or_default()
            .push(serde_json::json!({
                "id": row.id,
                "userName": row.user_name,
                "bio": row.bio,
                "avatar": avatar,
                "captain": row.captain,
            }));
    }

    Ok(teams
        .into_iter()
        .map(|team| {
            let team_id = team.id;
            let mut info = TeamInfoModel::from(team);
            info.members = members.remove(&team_id).unwrap_or_default();
            info
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    #[test]
    fn roster_projection_never_decodes_unrelated_identity_columns() {
        let sql = TEAM_MEMBER_PROJECTION_SQL.to_ascii_lowercase();
        assert!(!sql.contains("lockout_end"));
        assert!(!sql.contains("last_signed_in_utc"));
        assert!(!sql.contains("register_time_utc"));
        assert!(sql.contains("account.avatar_hash"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn roster_projection_tolerates_legacy_infinite_lockouts() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("admin_team_roster_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
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
            CREATE TABLE "AspNetUsers" (
              id UUID PRIMARY KEY,
              user_name TEXT,
              bio TEXT NOT NULL DEFAULT '',
              avatar_hash TEXT,
              lockout_end TIMESTAMPTZ
            );
            CREATE TABLE "Teams" (
              id INTEGER PRIMARY KEY,
              name TEXT NOT NULL,
              bio TEXT,
              avatar_hash TEXT,
              locked BOOLEAN NOT NULL DEFAULT FALSE,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE,
              invite_token TEXT NOT NULL,
              captain_id UUID NOT NULL
            );
            CREATE TABLE "TeamMembers" (
              id INTEGER GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
              team_id INTEGER NOT NULL,
              user_id UUID NOT NULL,
              UNIQUE (team_id, user_id)
            );
            INSERT INTO "AspNetUsers"
                (id, user_name, bio, avatar_hash, lockout_end)
            VALUES
                ('00000000-0000-0000-0000-000000000001',
                 'captain', 'captain bio', 'captain-hash', 'infinity'),
                ('00000000-0000-0000-0000-000000000002',
                 'member', 'member bio', NULL, '-infinity');
            INSERT INTO "Teams"
                (id, name, bio, avatar_hash, locked, deletion_pending,
                 invite_token, captain_id)
            VALUES
                (7, 'Infinite Lockouts', NULL, NULL, FALSE, FALSE, 'invite',
                 '00000000-0000-0000-0000-000000000001');
            INSERT INTO "TeamMembers" (team_id, user_id)
            VALUES
                (7, '00000000-0000-0000-0000-000000000001'),
                (7, '00000000-0000-0000-0000-000000000002');
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let legacy_values = sqlx::query_scalar::<_, String>(
            r#"SELECT lockout_end::TEXT FROM "AspNetUsers" ORDER BY id"#,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(legacy_values, vec!["infinity", "-infinity"]);

        let team = team::Model {
            id: 7,
            name: "Infinite Lockouts".to_owned(),
            bio: None,
            avatar_hash: None,
            locked: false,
            deletion_pending: false,
            invite_token: "invite".to_owned(),
            captain_id: Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
        };
        let result = teams_with_members(&pool, vec![team]).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].members.len(), 2);
        assert_eq!(result[0].members[0]["userName"], "captain");
        assert_eq!(result[0].members[0]["captain"], true);
        assert_eq!(
            result[0].members[0]["avatar"],
            "/assets/captain-hash/avatar"
        );
        assert_eq!(result[0].members[1]["userName"], "member");
        assert_eq!(result[0].members[1]["captain"], false);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
