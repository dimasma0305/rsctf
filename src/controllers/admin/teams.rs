//! Team listing / search / CRUD.

use super::*;
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

    // RSCTF `TeamRepository.GetTeams` eager-loads `Team.Members` and
    // `TeamInfoModel.FromTeam` emits them; resolve each team's roster so the admin
    // list isn't stuck with empty members arrays.
    let mut data = Vec::with_capacity(rows.len());
    for t in rows {
        let members = team_members(&st, &t).await?;
        let mut info = TeamInfoModel::from(t);
        info.members = members;
        data.push(info);
    }
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

    // Same eager members resolution as the list endpoint (RSCTF `SearchTeams`
    // also `.Include(t => t.Members)` before `TeamInfoModel.FromTeam`).
    let mut data = Vec::with_capacity(rows.len());
    for t in rows {
        let members = team_members(&st, &t).await?;
        let mut info = TeamInfoModel::from(t);
        info.members = members;
        data.push(info);
    }
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
    // helpers so the two delete paths stay in step. Both are best-effort.
    crate::controllers::team::destroy_team_containers(&st, team.id).await?;
    crate::controllers::team::flush_scoreboard_for_team(&st, team.id).await?;

    deletion_lease.finalize(team.id).await?;
    crate::controllers::team::flush_scoreboards_for_games(&st, &affected_game_ids).await;
    Ok(RequestResponse::ok(id.to_string()))
}

/// Resolve a team's roster in the client `TeamUserInfoModel` shape
/// (`id`/`userName`/`bio`/`avatar`/`captain`). Mirrors RSCTF
/// `TeamInfoModel.FromTeam`'s `Members` projection — which includes the captain
/// (seeded into `Team.Members` on create), so we union the `team_member` rows with
/// `captain_id`. `realName`/`studentNumber` are `[JsonIgnore]` in RSCTF and are
/// intentionally omitted here too.
async fn team_members(st: &SharedState, team: &team::Model) -> AppResult<Vec<Value>> {
    let mut ids: Vec<Uuid> = team_member::Entity::find()
        .filter(team_member::Column::TeamId.eq(team.id))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|r| r.user_id)
        .collect();
    ids.push(team.captain_id);
    ids.sort_unstable();
    ids.dedup();

    let users = user::Entity::find()
        .filter(user::Column::Id.is_in(ids))
        .all(&st.db)
        .await?;
    Ok(users
        .into_iter()
        .map(|u| {
            serde_json::json!({
                "id": u.id,
                "userName": u.user_name,
                "bio": u.bio,
                "avatar": u.avatar_url(),
                "captain": u.id == team.captain_id,
            })
        })
        .collect())
}
