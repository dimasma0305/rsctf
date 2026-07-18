//! Small batched display-name lookups shared by scoreboard and audit views.

use super::*;

pub(super) async fn team_name_map(
    st: &SharedState,
    ids: impl Iterator<Item = i32>,
) -> AppResult<HashMap<i32, String>> {
    let ids: Vec<i32> = ids.collect::<HashSet<_>>().into_iter().collect();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let teams = team::Entity::find()
        .filter(team::Column::Id.is_in(ids))
        .all(&st.db)
        .await?;
    Ok(teams.into_iter().map(|team| (team.id, team.name)).collect())
}

pub(super) async fn team_avatar_map(
    st: &SharedState,
    ids: impl Iterator<Item = i32>,
) -> AppResult<HashMap<i32, Option<String>>> {
    let ids: Vec<i32> = ids.collect::<HashSet<_>>().into_iter().collect();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let teams = team::Entity::find()
        .filter(team::Column::Id.is_in(ids))
        .all(&st.db)
        .await?;
    Ok(teams
        .into_iter()
        .map(|team| (team.id, team.avatar_url()))
        .collect())
}

pub(super) async fn user_name_map(
    st: &SharedState,
    ids: impl Iterator<Item = Uuid>,
) -> AppResult<HashMap<Uuid, String>> {
    let ids: Vec<Uuid> = ids.collect::<HashSet<_>>().into_iter().collect();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let users = user::Entity::find()
        .filter(user::Column::Id.is_in(ids))
        .all(&st.db)
        .await?;
    Ok(users
        .into_iter()
        .map(|user| (user.id, user.user_name.unwrap_or_default()))
        .collect())
}

pub(super) async fn challenge_title_map(
    st: &SharedState,
    ids: impl Iterator<Item = i32>,
) -> AppResult<HashMap<i32, String>> {
    let ids: Vec<i32> = ids.collect::<HashSet<_>>().into_iter().collect();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = game_challenge::Entity::find()
        .filter(game_challenge::Column::Id.is_in(ids))
        .all(&st.db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|challenge| (challenge.id, challenge.title))
        .collect())
}
