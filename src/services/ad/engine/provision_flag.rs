use crate::services::ad::service_lifecycle::{
    publish_managed_backend_if_eligible, ManagedBackendPublication,
};
use crate::utils::error::{AppError, AppResult};

pub(crate) fn managed_flag_command(flag: &str) -> Vec<String> {
    // The flag is passed as argv[1], never interpolated into the shell
    // program. Existing containers without RSCTF_FLAG_FILE use /flag.
    vec![
        "sh".to_string(),
        "-c".to_string(),
        "umask 022 && printf '%s\\n' \"$1\" > \"${RSCTF_FLAG_FILE:-/flag}\" && cat \"${RSCTF_FLAG_FILE:-/flag}\"".to_string(),
        "rsctf-flag".to_string(),
        flag.to_string(),
    ]
}

/// A newly created workload receives a compatibility environment value. File-based
/// services also need the current official flag before their endpoint goes live.
/// This readback does not issue or amend a scored round receipt.
pub(crate) async fn initialize_official_flag_file(
    state: &crate::app_state::SharedState,
    backend_id: &str,
    flag: Option<&str>,
) -> AppResult<()> {
    let Some(flag) = flag else {
        return Ok(());
    };
    let policy = super::FlagDeliveryPolicy::from_env().map_err(AppError::internal)?;
    for attempt in 1..=policy.attempts() {
        let write = state
            .containers
            .exec(backend_id, managed_flag_command(flag));
        match tokio::time::timeout(policy.attempt_timeout(), write).await {
            Ok(Ok(output)) if output.trim_end_matches(['\r', '\n']) == flag => return Ok(()),
            // A timed-out exec may still write later. Discard this unpublished
            // candidate before the next round can reuse its flag file.
            Err(_) => break,
            _ => {}
        }
        if attempt < policy.attempts() {
            tokio::time::sleep(policy.retry_backoff(attempt)).await;
        }
    }
    Err(AppError::unavailable(
        "The replacement service did not confirm its current flag file",
    ))
}

/// Warmup, a late arrival without a flag, and the official flag for a round
/// are distinct states. Never publish a workload prepared for a stale state.
#[derive(Default, PartialEq)]
pub(crate) struct ProvisionFlag {
    round_id: Option<i32>,
    pub(crate) official: Option<String>,
}

impl ProvisionFlag {
    /// A reset is tied to the prepared round. Only a warmup reset can use a
    /// bootstrap flag; a service joining mid-round has a separate provision path.
    pub(crate) fn into_reset_flag(
        self,
        prepared_round_id: Option<i32>,
    ) -> AppResult<Option<String>> {
        if self.round_id != prepared_round_id {
            return Err(AppError::conflict(
                "The A&D round changed after reset preparation",
            ));
        }
        if self.round_id.is_some() && self.official.is_none() {
            return Err(AppError::conflict(
                "The prepared round flag is missing for this service",
            ));
        }
        Ok(self.official)
    }

    pub(crate) async fn read<'e>(
        executor: impl sqlx::Executor<'e, Database = sqlx::Postgres>,
        game_id: i32,
        participation_id: i32,
        challenge_id: i32,
    ) -> AppResult<Self> {
        let latest = sqlx::query_as::<_, (i32, Option<String>)>(
            r#"SELECT round.id, CASE WHEN octet_length(flag.flag)>38 THEN '' ELSE flag.flag END
               FROM (SELECT id FROM "AdRounds" WHERE game_id=$1 AND NOT finalized
                     ORDER BY number DESC, id DESC LIMIT 1) round
               LEFT JOIN "AdTeamServices" service ON service.game_id=$1
                    AND service.participation_id=$2 AND service.challenge_id=$3
               LEFT JOIN "AdFlags" flag ON flag.round_id=round.id AND flag.team_service_id=service.id"#,
        )
        .bind(game_id)
        .bind(participation_id)
        .bind(challenge_id)
        .fetch_optional(executor)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        let Some((round_id, flag)) = latest else {
            return Ok(Self::default());
        };
        Ok(Self {
            round_id: Some(round_id),
            official: flag
                .map(crate::utils::flag_generator::validate_stored_ad_flag)
                .transpose()?,
        })
    }

    pub(crate) async fn publish(
        &self,
        pool: &sqlx::PgPool,
        endpoint: ManagedBackendPublication<'_>,
    ) -> AppResult<bool> {
        self.publish_over(pool, endpoint, None).await
    }

    /// Called under the team/service publication lock. Keep the stopped donor
    /// attached until its verified successor is committed.
    pub(crate) async fn repair_data_volumes(
        &self,
        state: &crate::app_state::SharedState,
        service: &crate::models::data::ad_team_service::Model,
        spec: &crate::services::container::ContainerSpec,
        donor: &str,
    ) -> AppResult<Option<String>> {
        // Returning None lets the caller remove a stateless donor. Its
        // writable layer still belongs to the team if the next launch is invalid.
        crate::services::container::validate_container_spec(spec)?;
        if !state.containers.has_ad_data_volumes(donor).await? {
            return Ok(None);
        }
        crate::services::traffic::fence_container_capture_stop(state, donor).await?;
        let info = state
            .containers
            .repair_ad_volumes(
                donor,
                spec,
                crate::services::container::AdVolumeChange::RepairStopped,
            )
            .await?
            .ok_or_else(|| {
                AppError::conflict("The service's data volumes changed before repair")
            })?;
        if info.ip.is_empty() || info.status != "running" {
            state.containers.destroy(&info.id).await?;
            return Err(AppError::unavailable(
                "The replacement service has no running endpoint",
            ));
        }
        let endpoint = ManagedBackendPublication {
            game_id: service.game_id,
            participation_id: service.participation_id,
            challenge_id: service.challenge_id,
            host: &info.ip,
            port: info.port,
            backend_id: &info.id,
            image_ref: &spec.image,
        };
        if let Err(error) =
            initialize_official_flag_file(state, &info.id, self.official.as_deref()).await
        {
            state.containers.destroy(&info.id).await?;
            return Err(error);
        }
        self.publish_volume_repair(state, endpoint, donor).await?;
        Ok(Some(info.id))
    }

    async fn publish_volume_repair(
        &self,
        state: &crate::app_state::SharedState,
        endpoint: ManagedBackendPublication<'_>,
        donor: &str,
    ) -> AppResult<()> {
        match self.publish_over(state.pg(), endpoint, Some(donor)).await {
            Ok(true) => {}
            Ok(false) => {
                state.containers.destroy(endpoint.backend_id).await?;
                return Err(AppError::conflict(
                    "The service changed during volume recovery",
                ));
            }
            Err(error) => {
                // A commit can succeed even when its acknowledgement is lost.
                // If this read also fails, leave both holders intact for retry.
                let committed: bool = sqlx::query_scalar(
                    r#"SELECT EXISTS(
                    SELECT 1 FROM "AdTeamServices" WHERE game_id=$1 AND participation_id=$2
                      AND challenge_id=$3 AND container_id=$4 AND host=$5 AND port=$6)"#,
                )
                .bind(endpoint.game_id)
                .bind(endpoint.participation_id)
                .bind(endpoint.challenge_id)
                .bind(endpoint.backend_id)
                .bind(endpoint.host)
                .bind(endpoint.port)
                .fetch_one(state.pg())
                .await
                .map_err(|e| AppError::internal(e.to_string()))?;
                if !committed {
                    state.containers.destroy(endpoint.backend_id).await?;
                    return Err(error);
                }
            }
        }
        // Generic orphan cleanup also removes the stopped donor after a crash.
        if let Err(error) = state.containers.destroy(donor).await {
            tracing::warn!(backend_id=donor,%error,"published volume repair; old container awaits cleanup");
        }
        Ok(())
    }

    async fn publish_over(
        &self,
        pool: &sqlx::PgPool,
        endpoint: ManagedBackendPublication<'_>,
        donor: Option<&str>,
    ) -> AppResult<bool> {
        // Round writers take the exclusive game lock. Different team services
        // can publish together and never wait behind a writer holding it.
        let mut control = crate::utils::single_flight::PgAdvisoryLock::try_acquire_shared(
            pool,
            &super::game_lock_key(endpoint.game_id),
        )
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::overloaded("The A&D round is changing; retry provisioning", 1))?;
        let connection = &mut **control.transaction_mut();
        let current = Self::read(
            &mut *connection,
            endpoint.game_id,
            endpoint.participation_id,
            endpoint.challenge_id,
        )
        .await?;
        if current != *self {
            return Err(AppError::conflict(
                "The A&D round changed while the service was starting",
            ));
        }
        if let Some(id) = donor {
            let retained = sqlx::query_scalar::<_, i32>(
                r#"SELECT id FROM "AdTeamServices" WHERE game_id=$1 AND participation_id=$2
                   AND challenge_id=$3 AND container_id=$4
                   AND (published_image_backend=$4 OR (published_image_backend IS NULL AND published_image_ref IS NULL))
                   AND host='' AND port=0 FOR UPDATE"#,
            )
            .bind(endpoint.game_id)
            .bind(endpoint.participation_id)
            .bind(endpoint.challenge_id)
            .bind(id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|e| AppError::internal(e.to_string()))?;
            if retained.is_none() {
                return Ok(false);
            }
        }
        let published = publish_managed_backend_if_eligible(connection, endpoint).await?;
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        Ok(published)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resetting_uses_a_bootstrap_flag_only_during_the_prepared_warmup() {
        assert_eq!(
            ProvisionFlag::default().into_reset_flag(None).unwrap(),
            None
        );
        assert!(ProvisionFlag::default().into_reset_flag(Some(7)).is_err());
        let late = ProvisionFlag {
            round_id: Some(7),
            official: None,
        };
        assert!(late.into_reset_flag(Some(7)).is_err());
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn provisioning_cannot_publish_after_the_round_changes() {
        let fixture = crate::services::ad::scoring::test_fixture::ad_scoring_fixture().await;
        let pool = &fixture.pool;
        sqlx::raw_sql(
            r#"UPDATE "Games" SET end_time_utc=now()+interval '1 hour' WHERE id=900001;
            UPDATE "GameChallenges" SET ad_self_hosted=FALSE WHERE id=900031"#,
        )
        .execute(pool)
        .await
        .unwrap();
        let read = || ProvisionFlag::read(pool, fixture.game_id, 900021, 900031);
        let row = || {
            sqlx::query_scalar::<_, serde_json::Value>(
                r#"SELECT to_jsonb(s) FROM "AdTeamServices" s WHERE id=900041"#,
            )
            .fetch_one(pool)
        };
        let initial = row().await.unwrap();
        let warmup = read().await.unwrap();
        assert!(warmup.round_id.is_none());
        let endpoint = ManagedBackendPublication {
            game_id: fixture.game_id,
            participation_id: 900021,
            challenge_id: 900031,
            host: "fixture",
            port: 8080,
            backend_id: "replacement",
            image_ref: "fixture-image",
        };
        sqlx::raw_sql(
            r#"SET LOCAL rsctf.maintenance_writer='on';
            UPDATE "AdRounds" SET finalized=FALSE WHERE id=900108"#,
        )
        .execute(pool)
        .await
        .unwrap();
        assert!(matches!(
            warmup.publish(pool, endpoint).await,
            Err(AppError::Conflict(_))
        ));
        let missing = ProvisionFlag::read(pool, fixture.game_id, i32::MAX, 900031)
            .await
            .unwrap();
        assert!(matches!(
            missing.into_reset_flag(Some(900108)),
            Err(AppError::Conflict(message)) if message.contains("flag is missing")
        ));
        let official = read().await.unwrap();
        assert_eq!(official.round_id, Some(900108));
        assert!(official.official.is_some());
        sqlx::raw_sql(
            r#"SET LOCAL rsctf.maintenance_writer='on';
            UPDATE "AdRounds" SET finalized=TRUE WHERE id=900108"#,
        )
        .execute(pool)
        .await
        .unwrap();
        assert!(matches!(
            official.publish(pool, endpoint).await,
            Err(AppError::Conflict(_))
        ));
        assert_eq!(row().await.unwrap(), initial);
        let key = crate::services::ad_engine::game_lock_key(fixture.game_id);
        let writer = crate::utils::single_flight::PgAdvisoryLock::acquire(pool, &key)
            .await
            .unwrap();
        assert!(matches!(
            warmup.publish(pool, endpoint).await,
            Err(AppError::RetryableUnavailable { .. })
        ));
        writer.release().await.unwrap();
        let reader = crate::utils::single_flight::PgAdvisoryLock::try_acquire_shared(pool, &key)
            .await
            .unwrap()
            .unwrap();
        assert!(warmup.publish(pool, endpoint).await.unwrap());
        reader.release().await.unwrap();
        fixture.cleanup().await;
    }
}
