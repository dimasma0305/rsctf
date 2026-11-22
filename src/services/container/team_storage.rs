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
}

pub(crate) fn application_data_path(path: &str) -> bool {
    path == "/data"
        || path.strip_prefix("/var/lib/").is_some_and(|part| {
            !part.is_empty()
                && part.len() <= 128
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        })
}

impl TeamStorage {
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
        let (allocation_id, claim_uid) = sqlx::query_as::<_, (uuid::Uuid, Option<String>)>(
            r#"INSERT INTO "AdTeamStorageClaims" (participation_id,challenge_id,allocation_id)
               VALUES ($1,$2,$3) ON CONFLICT (participation_id,challenge_id)
               DO UPDATE SET participation_id=EXCLUDED.participation_id RETURNING allocation_id,claim_uid"#,
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
        };
        let uid = state
            .containers
            .prepare_team_claim(spec.runtime_profile.as_deref(), &data, spec.storage_limit)
            .await?;
        if data.claim_uid.is_none() {
            // Concurrent launches must agree on the same API-server object.
            let pinned = sqlx::query(
                r#"UPDATE "AdTeamStorageClaims" SET claim_uid=$2 WHERE allocation_id=$1
                   AND (claim_uid IS NULL OR claim_uid=$2)"#,
            )
            .bind(allocation_id)
            .bind(&uid)
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
        spec.team_storage = Some(data);
        if spec.operation_id.is_none() {
            spec.operation_id = Some(format!(
                "ad-storage-provision:{game_id}:{participation_id}:{challenge_id}"
            ));
        }
        Ok(())
    }
}
