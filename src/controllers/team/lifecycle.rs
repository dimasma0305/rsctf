//! Team-owned A&D backend lifecycle helpers.

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

use crate::app_state::SharedState;
use crate::models::data::ad_team_service;
use crate::utils::error::AppResult;

/// Stop every A&D backend owned by a participation under its provisioning lock.
pub(super) async fn destroy_participation_ad_services(
    st: &SharedState,
    participation_id: i32,
) -> AppResult<()> {
    let services = ad_team_service::Entity::find()
        .filter(ad_team_service::Column::ParticipationId.eq(participation_id))
        .order_by_asc(ad_team_service::Column::Id)
        .all(&st.db)
        .await?;
    for service in services {
        let lock_key = format!(
            "ad-service:{}:{}",
            service.participation_id, service.challenge_id
        );
        let _local = crate::utils::single_flight::coalesce(&lock_key).await;
        let distributed =
            crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &lock_key)
                .await?;
        let current = ad_team_service::Entity::find_by_id(service.id)
            .one(&st.db)
            .await?
            .filter(|row| row.participation_id == participation_id);
        if let Some(current) = current {
            let backend_id = current.container_id.clone();
            crate::services::ad_vpn::deactivate_team_service(&st.db, current.id).await?;
            if let Some(backend_id) = backend_id {
                crate::services::traffic::stop_container_capture(st, &backend_id).await?;
                let _ = st.containers.destroy(&backend_id).await;
            }
        }
        distributed.release().await?;
    }
    Ok(())
}
