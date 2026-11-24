use crate::services::ad::maintenance::database_error;
use crate::utils::error::{AppError, AppResult};

/// Identity comes from the accepted participation, never from a player ZIP.
#[derive(Clone, Debug, serde::Serialize)]
pub struct TeamStorage {
    pub game_id: i32,
    pub participation_id: i32,
    pub challenge_id: i32,
    pub mount_path: String,
    pub allocation_id: uuid::Uuid,
    pub claim_uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_uid: Option<String>,
    /// A registered Service may route as soon as the kubelet marks its Pod ready.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub initialize_round_flag: bool,
}

impl super::ContainerSpec {
    pub(crate) fn set_ad_launch_flag(&mut self, flag: String, official_round: bool) {
        self.flag = Some(flag);
        if let Some(storage) = &mut self.team_storage {
            storage.initialize_round_flag = official_round;
        }
    }
}

impl TeamStorage {
    pub(crate) async fn supports_patch_activation(
        state: &crate::app_state::SharedState,
        service: i32,
        profile: Option<&str>,
        donor: Option<&str>,
    ) -> AppResult<bool> {
        if state
            .containers
            .supports_ad_patch_activation(profile, donor)
        {
            return Ok(true);
        }
        if let Some(handle) = donor.and_then(crate::services::worker::parse_worker_handle) {
            if profile.is_some() {
                return Ok(false);
            }
            // A legacy manually assigned worker may have no private listener.
            // Keep activation unavailable until its exact endpoint can be recovered.
            return sqlx::query_scalar(
                r#"SELECT EXISTS(SELECT 1 FROM "AdWorkerRelays" e
                JOIN "WorkerWorkloads" w ON w.id=e.workload_id AND w.assignment_id=$3
                JOIN "AdTeamServices" s USING(participation_id,challenge_id)
                WHERE s.id=$1 AND e.workload_id=$2 AND w.owner_kind='ad-team-service'
                AND w.owner_key=concat('ad-service:',e.participation_id,':',e.challenge_id)
                AND e.ready_until>clock_timestamp())"#,
            )
            .bind(service)
            .bind(handle.workload_id)
            .bind(handle.assignment_id)
            .fetch_one(state.pg())
            .await
            .map_err(database_error);
        }
        let Some(donor) = donor.filter(|id| !id.is_empty()) else {
            return Ok(false);
        };
        if !matches!(
            state.containers.profile_backend_kind(profile),
            Ok(super::ContainerBackendKind::Kubernetes)
        ) || !matches!(
            state.containers.runtime_backend_kind(donor),
            Ok(super::ContainerBackendKind::Kubernetes)
        ) {
            return Ok(false);
        }
        sqlx::query_scalar(
            r#"SELECT EXISTS(SELECT 1 FROM "AdTeamServices" s
               JOIN "AdTeamStorageClaims" c USING(participation_id,challenge_id)
               WHERE s.id=$1 AND c.claim_uid IS NOT NULL AND c.endpoint_uid IS NOT NULL)"#,
        )
        .bind(service)
        .fetch_one(state.pg())
        .await
        .map_err(database_error)
    }

    pub(crate) async fn configure(
        state: &crate::app_state::SharedState,
        spec: &mut super::ContainerSpec,
        game_id: i32,
        participation_id: i32,
        challenge_id: i32,
    ) -> AppResult<()> {
        if state
            .containers
            .profile_backend_kind(spec.runtime_profile.as_deref())?
            != super::ContainerBackendKind::Kubernetes
        {
            return Ok(());
        }
        super::validate_container_spec(spec)?;
        let mut tx = state.pg().begin().await.map_err(database_error)?;
        let path = sqlx::query_scalar::<_, String>(
            r#"UPDATE "AdPatchStarters" SET data_mount_claimed=TRUE
               WHERE challenge_id=$1 AND data_mount IS NOT NULL RETURNING data_mount"#,
        )
        .bind(challenge_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?;
        let Some(mount_path) = path else {
            return Ok(());
        };
        let (allocation_id, claim_uid, endpoint_uid) = sqlx::query_as::<_, (uuid::Uuid, Option<String>, Option<String>)>(
            r#"INSERT INTO "AdTeamStorageClaims" (participation_id,challenge_id,allocation_id)
               VALUES ($1,$2,$3) ON CONFLICT (participation_id,challenge_id)
               DO UPDATE SET participation_id=EXCLUDED.participation_id RETURNING allocation_id,claim_uid,endpoint_uid"#,
        )
        .bind(participation_id)
        .bind(challenge_id)
        .bind(uuid::Uuid::new_v4())
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        let mut data = Self {
            game_id,
            participation_id,
            challenge_id,
            mount_path,
            allocation_id,
            claim_uid,
            endpoint_uid,
            initialize_round_flag: false,
        };
        let (uid, endpoint) = state
            .containers
            .prepare_team_claim(
                spec.runtime_profile.as_deref(),
                &data,
                spec.storage_limit,
                spec.expose_port,
            )
            .await?;
        if data.claim_uid.is_none() || data.endpoint_uid.is_none() {
            // Concurrent launches must agree on the same API-server object.
            let pinned = sqlx::query(
                r#"UPDATE "AdTeamStorageClaims" SET claim_uid=$2,endpoint_uid=$3 WHERE allocation_id=$1
                   AND (claim_uid IS NULL OR claim_uid=$2) AND (endpoint_uid IS NULL OR endpoint_uid=$3)"#,
            )
            .bind(allocation_id)
            .bind(&uid)
            .bind(&endpoint)
            .execute(state.pg())
            .await
            .map_err(database_error)?;
            if pinned.rows_affected() != 1 {
                return Err(AppError::conflict(
                    "The registered storage claim changed during provisioning",
                ));
            }
        }
        data.claim_uid = Some(uid);
        data.endpoint_uid = Some(endpoint);
        spec.team_storage = Some(data);
        if spec.operation_id.is_none() {
            spec.operation_id = Some(format!(
                "ad-storage-provision:{game_id}:{participation_id}:{challenge_id}"
            ));
        }
        Ok(())
    }
}
