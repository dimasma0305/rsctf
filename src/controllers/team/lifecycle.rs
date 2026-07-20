//! Team-owned A&D backend lifecycle helpers.

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

use crate::app_state::SharedState;
use crate::models::data::ad_team_service;
use crate::utils::error::AppResult;

/// Stop every A&D backend owned by a participation under its provisioning lock.
pub(crate) async fn destroy_participation_ad_services(
    st: &SharedState,
    participation_id: i32,
) -> AppResult<()> {
    let game_ids = sqlx::query_scalar::<_, i32>(
        r#"SELECT game_id FROM "Participations" WHERE id = $1
           UNION
           SELECT game_id FROM "AdTeamServices" WHERE participation_id = $1"#,
    )
    .bind(participation_id)
    .fetch_all(st.pg())
    .await
    .map_err(|error| crate::utils::error::AppError::internal(error.to_string()))?;
    crate::services::ad::service_lifecycle::drain_publications(st.pg(), game_ids).await?;
    let services = ad_team_service::Entity::find()
        .filter(ad_team_service::Column::ParticipationId.eq(participation_id))
        .order_by_asc(ad_team_service::Column::Id)
        .all(&st.db)
        .await?;
    for service in services {
        let lock_key = crate::services::ad::service_lifecycle::service_lock_key(
            service.participation_id,
            service.challenge_id,
        );
        let _local = crate::utils::single_flight::coalesce(&lock_key).await;
        let distributed =
            crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &lock_key)
                .await?;
        let teardown =
            crate::services::ad::service_lifecycle::destroy_persisted_service(st, service.id).await;
        let release = distributed.release().await;
        teardown?;
        release?;
    }
    Ok(())
}
