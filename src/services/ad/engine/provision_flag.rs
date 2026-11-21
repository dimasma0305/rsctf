use crate::services::ad::service_lifecycle::{
    publish_managed_backend_if_eligible, ManagedBackendPublication,
};
use crate::utils::error::{AppError, AppResult};

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
