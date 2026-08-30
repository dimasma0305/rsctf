//! Shared revocation leases for established proxy sessions.

use std::net::Ipv4Addr;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use uuid::Uuid;

use super::authorization::{
    exercise_lease_is_valid, game_proxy_session_is_valid, GameProxyTargetIdentity,
};
use crate::services::authorization_lease::LeaseGenerationCache;
use crate::services::live_roster::LiveParticipationIdentity;
use crate::utils::enums::Role;

#[derive(Clone)]
pub(super) struct InstanceLease {
    pub(super) pool: sqlx::PgPool,
    pub(super) user_id: Uuid,
    pub(super) security_stamp: String,
    pub(super) owner: LeaseOwner,
}

#[derive(Clone, Eq, Hash, PartialEq)]
pub(super) enum LeaseOwner {
    Game {
        game_id: i32,
        team_id: i32,
        participation_id: i32,
        challenge_id: i32,
        target_identity: GameProxyTargetIdentity,
        event_vpn_source: Option<Ipv4Addr>,
        bypass_event_vpn: bool,
    },
    Exercise {
        exercise_instance_id: i32,
        exercise_id: i32,
        container_id: Uuid,
    },
    Preview {
        container_id: Uuid,
    },
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct LeaseGenerationKey {
    user_id: Uuid,
    security_stamp: String,
    owner: LeaseOwner,
}

static LEASE_GENERATIONS: LazyLock<Arc<LeaseGenerationCache<LeaseGenerationKey>>> =
    LazyLock::new(LeaseGenerationCache::new);

pub(super) async fn wait_for_revocation(lease: InstanceLease) {
    let jitter = Duration::from_millis(u64::from(lease.user_id.as_bytes()[0]) * 4);
    let key = LeaseGenerationKey {
        user_id: lease.user_id,
        security_stamp: lease.security_stamp.clone(),
        owner: lease.owner.clone(),
    };
    let (mut subscription, owner) = LEASE_GENERATIONS.subscribe(key);
    if let Some(owner) = owner {
        drop(tokio::spawn(owner.drive(
            Duration::from_secs(5) + jitter,
            move || {
                let lease = lease.clone();
                async move { lease_is_valid(&lease).await }
            },
        )));
    }
    subscription.invalidated().await;
}

pub(super) async fn lease_is_valid(lease: &InstanceLease) -> bool {
    match &lease.owner {
        LeaseOwner::Game {
            game_id,
            team_id,
            participation_id,
            challenge_id,
            target_identity,
            event_vpn_source,
            bypass_event_vpn,
        } => {
            game_proxy_session_is_valid(
                &lease.pool,
                LiveParticipationIdentity {
                    user_id: lease.user_id,
                    expected_security_stamp: &lease.security_stamp,
                    game_id: *game_id,
                    team_id: *team_id,
                    participation_id: *participation_id,
                },
                *challenge_id,
                target_identity,
                *event_vpn_source,
                *bypass_event_vpn,
            )
            .await
        }
        LeaseOwner::Exercise {
            exercise_instance_id,
            exercise_id,
            container_id,
        } => {
            exercise_lease_is_valid(
                &lease.pool,
                lease.user_id,
                &lease.security_stamp,
                *exercise_instance_id,
                *exercise_id,
                *container_id,
            )
            .await
        }
        LeaseOwner::Preview { container_id } => {
            preview_lease_is_valid(
                &lease.pool,
                lease.user_id,
                &lease.security_stamp,
                *container_id,
            )
            .await
        }
    }
}

const PREVIEW_LEASE_SQL: &str = r#"SELECT EXISTS (
    SELECT 1
      FROM "AspNetUsers" account
      JOIN "Containers" container ON container.id = $3
     WHERE account.id = $1
       AND account.security_stamp = $2
       AND account.email_confirmed = TRUE
       AND account.role IN ($4, $5)
       AND container.is_proxy = TRUE
       AND container.game_instance_id IS NULL
       AND container.exercise_instance_id IS NULL
       AND NOT EXISTS (
           SELECT 1 FROM "ExerciseInstances" exercise
            WHERE exercise.container_id = container.id
       )
)"#;

async fn preview_lease_is_valid(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    expected_security_stamp: &str,
    container_id: Uuid,
) -> bool {
    let Some(_permit) = crate::services::authorization_lease::try_query_permit() else {
        return false;
    };
    tokio::time::timeout(
        Duration::from_secs(3),
        sqlx::query_scalar::<_, bool>(PREVIEW_LEASE_SQL)
            .bind(user_id)
            .bind(expected_security_stamp)
            .bind(container_id)
            .bind(Role::Admin as i16)
            .bind(Role::Monitor as i16)
            .fetch_one(pool),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::PREVIEW_LEASE_SQL;

    #[test]
    fn preview_lease_rechecks_identity_role_and_exact_unowned_container() {
        for gate in [
            "account.security_stamp = $2",
            "account.email_confirmed = TRUE",
            "account.role IN ($4, $5)",
            "container.id = $3",
            "container.is_proxy = TRUE",
            "container.game_instance_id IS NULL",
            "container.exercise_instance_id IS NULL",
            "exercise.container_id = container.id",
        ] {
            assert!(
                PREVIEW_LEASE_SQL.contains(gate),
                "missing preview gate: {gate}"
            );
        }
    }
}
