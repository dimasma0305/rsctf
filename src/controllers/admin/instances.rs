//! Admin container-instance listing + destroy + stats — split from admin/mod.rs.
use super::*;
use crate::models::data::{
    ad_team_service, container, game_challenge, game_instance, participation, team,
};

/// RSCTF `ChallengeModel` (nested challenge reference).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeModel {
    pub id: i32,
    pub title: String,
    pub category: ChallengeCategory,
}

/// RSCTF `ContainerInstanceModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerInstanceModel {
    pub team: Option<TeamModel>,
    pub challenge: Option<ChallengeModel>,
    pub image: String,
    pub container_guid: Uuid,
    pub container_id: String,
    #[serde(with = "crate::utils::datetime::millis")]
    pub started_at: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub expect_stop_at: DateTime<Utc>,
    pub ip: String,
    pub port: i32,
}

/// `GET /api/admin/instances` — paginated list of live container rows, joined to
/// their GameInstance → challenge / participation → team where possible.
pub async fn instances(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(q): Query<ListQuery>,
) -> AppResult<ArrayResponse<ContainerInstanceModel>> {
    let count = q.count.clamp(0, 500);
    let total = container::Entity::find().count(&st.db).await? as i64;
    let rows = container::Entity::find()
        .order_by_asc(container::Column::StartedAt)
        .offset(q.skip)
        .limit(count)
        .all(&st.db)
        .await?;

    let mut data = Vec::with_capacity(rows.len());
    for c in rows {
        let mut team = None;
        let mut challenge = None;

        if let Some(gi_id) = c.game_instance_id {
            if let Some(inst) = game_instance::Entity::find_by_id(gi_id).one(&st.db).await? {
                if let Some(ch) = game_challenge::Entity::find_by_id(inst.challenge_id)
                    .one(&st.db)
                    .await?
                {
                    challenge = Some(ChallengeModel {
                        id: ch.id,
                        title: ch.title,
                        category: ch.category,
                    });
                }
                if let Some(part) = participation::Entity::find_by_id(inst.participation_id)
                    .one(&st.db)
                    .await?
                {
                    if let Some(t) = team::Entity::find_by_id(part.team_id).one(&st.db).await? {
                        team = Some(TeamModel {
                            id: t.id,
                            name: t.name.clone(),
                            avatar: t.avatar_url(),
                        });
                    }
                }
            }
        }

        // A&D service container: linked via ad_team_service.container_id (no
        // game_instance). RSCTF merges these in — resolve team + challenge.
        if challenge.is_none() && !c.container_id.is_empty() {
            if let Some(svc) = ad_team_service::Entity::find()
                .filter(ad_team_service::Column::ContainerId.eq(c.container_id.clone()))
                .one(&st.db)
                .await?
            {
                if let Some(ch) = game_challenge::Entity::find_by_id(svc.challenge_id)
                    .one(&st.db)
                    .await?
                {
                    challenge = Some(ChallengeModel {
                        id: ch.id,
                        title: ch.title,
                        category: ch.category,
                    });
                }
                if let Some(part) = participation::Entity::find_by_id(svc.participation_id)
                    .one(&st.db)
                    .await?
                {
                    if let Some(t) = team::Entity::find_by_id(part.team_id).one(&st.db).await? {
                        team = Some(TeamModel {
                            id: t.id,
                            name: t.name.clone(),
                            avatar: t.avatar_url(),
                        });
                    }
                }
            }
        }

        // Admin TEST container: linked via game_challenge.test_container_id. Label
        // it with the challenge + a "(test)" pseudo-team so the table shows real
        // names instead of blank placeholder columns.
        if challenge.is_none() {
            if let Some(ch) = game_challenge::Entity::find()
                .filter(game_challenge::Column::TestContainerId.eq(c.id))
                .one(&st.db)
                .await?
            {
                challenge = Some(ChallengeModel {
                    id: ch.id,
                    title: ch.title,
                    category: ch.category,
                });
                team = Some(TeamModel {
                    id: 0,
                    name: "(test container)".to_string(),
                    avatar: None,
                });
            }
        }

        let ip = c.public_ip.clone().unwrap_or_else(|| c.ip.clone());
        let port = c.public_port.unwrap_or(c.port);

        data.push(ContainerInstanceModel {
            team,
            challenge,
            image: c.image,
            container_guid: c.id,
            container_id: c.container_id,
            started_at: c.started_at,
            expect_stop_at: c.expect_stop_at,
            ip,
            port,
        });
    }

    Ok(ArrayResponse::new(data, total))
}

/// `DELETE /api/admin/instances/{id}` — forcibly destroy a container.
pub async fn destroy_instance(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<Uuid>,
) -> AppResult<MessageResponse> {
    let c = container::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Container instance not found"))?;

    crate::controllers::game::destroy_managed_container_row(&st, &c, false).await?;
    Ok(MessageResponse::ok(""))
}

/// RSCTF `ContainerStatsModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerStatsModel {
    pub cpu_percent: f64,
    pub memory_used_bytes: i64,
    pub memory_limit_bytes: i64,
    pub net_rx_bytes: i64,
    pub net_tx_bytes: i64,
    #[serde(with = "crate::utils::datetime::millis")]
    pub sampled_at: DateTime<Utc>,
}

/// `GET /api/admin/instances/{id}/stats` — point-in-time container stats.
///
/// Mirrors RSCTF `AdminController.GetInstanceStats`: look up the container row by
/// its database GUID, then sample the live runtime via `st.containers.query`,
/// which reads the Docker stats API and returns a `ContainerStatus` with
/// `memory_bytes` / `cpu_usage` populated. The coarse `ContainerStatus` sample
/// carries CPU (as a fraction of one core) and memory (bytes); it does not expose
/// a memory limit or per-interface network counters, so those DTO fields stay `0`
/// (matching the "stats the backend can provide" contract). `cpu_usage` is scaled
/// ×100 to the `cpuPercent` (0–100 × cores) the client renders.
///
/// When the runtime can't provide a sample — no Docker backend configured, the
/// daemon is unreachable, or the container is already gone — `query` errors; we
/// degrade to a 404 with a null payload, exactly like RSCTF returns when
/// `GetStatsAsync` yields `null`.
pub async fn instance_stats(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<Uuid>,
) -> AppResult<RequestResponse<ContainerStatsModel>> {
    let c = container::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Container instance not found"))?;

    // Sample the live runtime. A backend error (Docker unreachable / no backend /
    // container gone) degrades to a 404 "stats unavailable" rather than a 500,
    // so the admin UI just shows the row without a stats overlay.
    let status = st
        .containers
        .query(&c.container_id)
        .await
        .map_err(|_| AppError::not_found("Stats unavailable for this container."))?;

    Ok(RequestResponse::ok(ContainerStatsModel {
        cpu_percent: status.cpu_usage.map(|v| v * 100.0).unwrap_or(0.0),
        memory_used_bytes: status.memory_bytes.map(|m| m as i64).unwrap_or(0),
        // The coarse ContainerStatus sample carries no memory limit or network
        // counters; leave them zero until the backend surfaces them.
        memory_limit_bytes: 0,
        net_rx_bytes: 0,
        net_tx_bytes: 0,
        sampled_at: Utc::now(),
    }))
}

// ─── Files ─────────────────────────────────────────────────────────────────────

/// RSCTF `LocalFile`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalFileModel {
    pub hash: String,
    pub name: String,
}
