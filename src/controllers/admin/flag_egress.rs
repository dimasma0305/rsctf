//! Admin Flag-Egress feed — `GET /api/admin/Games/{id}/FlagEgress`.
//!
//! Lists the windowed flag-egress events for a game (a team's own flag bytes seen
//! leaving its proxied container). Populated best-effort by the proxy tunnel
//! ([`crate::controllers::proxy`]) scanning pumped bytes for the container's flag.
//! Admin-monitoring only — never contributes to a suspicion score (matching
//! RSCTF, which deliberately does not penalize a team for its own flag).
use super::*;

use crate::models::data::{flag_egress_event, game_challenge, participation, team};

/// Client `FlagEgressEventModel` (`web/src/pages/admin/games/[id]/FlagEgress.tsx`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlagEgressEventModel {
    pub id: i32,
    pub game_id: i32,
    pub participation_id: i32,
    pub challenge_id: i32,
    pub container_id: Option<String>,
    pub team_name: String,
    pub challenge_title: String,
    pub remote_ip: String,
    pub remote_port: i32,
    pub hit_count: i32,
    #[serde(with = "crate::utils::datetime::millis")]
    pub first_seen_utc: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub last_seen_utc: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlagEgressQuery {
    #[serde(default = "default_count")]
    pub count: u64,
    #[serde(default)]
    pub skip: u64,
}
fn default_count() -> u64 {
    100
}

/// `GET /api/admin/Games/{id}/FlagEgress?count=&skip=` — newest first.
pub async fn get_flag_egress(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(game_id): Path<i32>,
    Query(q): Query<FlagEgressQuery>,
) -> AppResult<ArrayResponse<FlagEgressEventModel>> {
    let total = flag_egress_event::Entity::find()
        .filter(flag_egress_event::Column::GameId.eq(game_id))
        .count(&st.db)
        .await? as i64;

    let rows = flag_egress_event::Entity::find()
        .filter(flag_egress_event::Column::GameId.eq(game_id))
        .order_by_desc(flag_egress_event::Column::LastSeenUtc)
        .offset(q.skip)
        .limit(q.count.clamp(1, 500))
        .all(&st.db)
        .await?;

    // Resolve team names (participation -> team) + challenge titles for the page.
    let part_ids: Vec<i32> = rows.iter().map(|r| r.participation_id).collect();
    let team_by_part: std::collections::HashMap<i32, String> = if part_ids.is_empty() {
        std::collections::HashMap::new()
    } else {
        let parts = participation::Entity::find()
            .filter(participation::Column::Id.is_in(part_ids))
            .all(&st.db)
            .await?;
        let team_ids: Vec<i32> = parts.iter().map(|p| p.team_id).collect();
        let team_names: std::collections::HashMap<i32, String> = team::Entity::find()
            .filter(team::Column::Id.is_in(team_ids))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|t| (t.id, t.name))
            .collect();
        parts
            .into_iter()
            .map(|p| {
                (
                    p.id,
                    team_names.get(&p.team_id).cloned().unwrap_or_default(),
                )
            })
            .collect()
    };

    let chall_ids: Vec<i32> = rows.iter().map(|r| r.challenge_id).collect();
    let title_by_chall: std::collections::HashMap<i32, String> = if chall_ids.is_empty() {
        std::collections::HashMap::new()
    } else {
        game_challenge::Entity::find()
            .filter(game_challenge::Column::Id.is_in(chall_ids))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|c| (c.id, c.title))
            .collect()
    };

    let data: Vec<FlagEgressEventModel> = rows
        .into_iter()
        .map(|e| FlagEgressEventModel {
            id: e.id,
            game_id: e.game_id,
            participation_id: e.participation_id,
            challenge_id: e.challenge_id,
            container_id: e.container_id.map(|c| c.to_string()),
            team_name: team_by_part
                .get(&e.participation_id)
                .cloned()
                .unwrap_or_default(),
            challenge_title: title_by_chall
                .get(&e.challenge_id)
                .cloned()
                .unwrap_or_default(),
            remote_ip: e.remote_ip,
            remote_port: e.remote_port,
            hit_count: e.hit_count,
            first_seen_utc: e.first_seen_utc,
            last_seen_utc: e.last_seen_utc,
        })
        .collect();

    Ok(ArrayResponse::new(data, total))
}
