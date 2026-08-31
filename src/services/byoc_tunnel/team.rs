use std::collections::HashSet;

use futures::{future, StreamExt};
use sea_orm::DatabaseConnection;

use super::{wait_for_endpoint_shutdown, wait_for_tunnel_shutdown, Registry};
use crate::utils::error::{AppError, AppResult};

const DISCONNECT_TEAM_SERVICES_SQL: &str = r#"
    UPDATE "AdTeamServices" service
       SET host = '', port = 0, status = 2
      FROM "Participations" participation,
           "GameChallenges" challenge
     WHERE participation.id = service.participation_id
       AND participation.team_id = $1
       AND challenge.id = service.challenge_id
       AND challenge.ad_self_hosted = TRUE
       AND service.container_id IS NULL
       AND (service.host <> '' OR service.port <> 0 OR service.status <> 2)
"#;

impl Registry {
    /// Revoke every BYOC endpoint owned by a team as one bounded transition.
    /// The database update and VPN synchronization are set-wise, while local
    /// endpoint shutdown is capped so a long participation history cannot
    /// create one serial timeout per row.
    pub async fn disconnect_team(&self, db: &DatabaseConnection, team_id: i32) -> AppResult<()> {
        self.disconnect_team_inner(db, team_id, true).await
    }

    pub(super) async fn disconnect_team_inner(
        &self,
        db: &DatabaseConnection,
        team_id: i32,
        propagate: bool,
    ) -> AppResult<()> {
        // Only resolve participations represented by a live local endpoint.
        // Team history is intentionally unbounded, while this registry is the
        // finite runtime set that this replica can actually revoke.
        let endpoint_candidates = {
            let registry = self.endpoints.lock().await;
            registry
                .keys()
                .map(|(participation_id, _)| *participation_id)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
        };
        let participation_ids = if endpoint_candidates.is_empty() {
            Vec::new()
        } else {
            sqlx::query_scalar::<_, i32>(
                r#"SELECT id FROM "Participations"
                    WHERE team_id = $1 AND id = ANY($2)
                    ORDER BY id"#,
            )
            .bind(team_id)
            .bind(&endpoint_candidates)
            .fetch_all(db.get_postgres_connection_pool())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
        };
        let participation_set = participation_ids.iter().copied().collect::<HashSet<_>>();
        {
            let mut generations = self.authorization_generations.write().await;
            for &participation_id in &participation_ids {
                let generation = generations
                    .participations
                    .entry(participation_id)
                    .or_default();
                *generation = generation.saturating_add(1);
            }
        }
        let endpoints = {
            let mut registry = self.endpoints.lock().await;
            let keys = registry
                .keys()
                .filter(|(participation_id, _)| participation_set.contains(participation_id))
                .copied()
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| registry.remove(&key))
                .collect::<Vec<_>>()
        };
        let handles = futures::stream::iter(endpoints.iter().cloned())
            .map(|endpoint| async move { endpoint.revoke().await })
            .buffer_unordered(16)
            .filter_map(future::ready)
            .collect::<Vec<_>>()
            .await;
        let revocation = async {
            sqlx::query(DISCONNECT_TEAM_SERVICES_SQL)
                .bind(team_id)
                .execute(db.get_postgres_connection_pool())
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            crate::services::ad_vpn::ensure_hub_and_sync(db).await
        }
        .await;
        let mut handles = handles;
        wait_for_tunnel_shutdown(&mut handles).await;
        wait_for_endpoint_shutdown(&endpoints).await;
        if propagate
            && revocation.is_ok()
            && self.events.is_distributed()
            && !crate::services::ad_vpn::owns_instance_lease()
        {
            self.events.publish(crate::app_state::HubEvent {
                target: "InternalByocRevokeTeam",
                game_id: None,
                payload: team_id.to_string(),
            });
        }
        revocation
    }
}

#[cfg(test)]
mod tests {
    use super::DISCONNECT_TEAM_SERVICES_SQL;

    #[test]
    fn team_revocation_is_one_set_based_active_service_update() {
        assert!(DISCONNECT_TEAM_SERVICES_SQL.contains("participation.team_id = $1"));
        assert!(DISCONNECT_TEAM_SERVICES_SQL.contains("service.status <> 2"));
        assert!(!DISCONNECT_TEAM_SERVICES_SQL.contains("ANY("));
    }
}
