//! Transactional authorization for BYOC bundles, agents, and image exports.

use crate::app_state::SharedState;
use crate::utils::enums::{ChallengeReviewStatus, ChallengeType, ParticipationStatus};
use crate::utils::error::{AppError, AppResult};

/// The mutable database fields from which a BYOC capability is derived. Every
/// field is read on the transaction that owns the roster and row-level fences.
pub(crate) struct ByocGrant {
    pub(crate) participation_id: i32,
    pub(crate) title: String,
    pub(crate) container_image: Option<String>,
    pub(crate) build_status: i16,
    pub(crate) build_image_digest: Option<String>,
    pub(crate) expose_port: Option<i32>,
    pub(crate) game_secret: String,
    pub(crate) team_secret: String,
}

impl ByocGrant {
    pub(crate) fn runtime_image(&self, st: &SharedState) -> AppResult<String> {
        crate::services::challenge_images::runtime_image_from_build_fields(
            st,
            self.build_status,
            self.build_image_digest.as_deref(),
        )
    }

    pub(crate) fn setup_runtime_image(&self, st: &SharedState) -> AppResult<Option<String>> {
        if self
            .container_image
            .as_deref()
            .is_some_and(|image| !image.trim().is_empty())
        {
            self.runtime_image(st).map(Some)
        } else {
            Ok(None)
        }
    }
}

/// A live bearer capability plus the database fence that makes its admission
/// decision atomic. Callers release it at their bounded hand-off point; it must
/// never be retained across client-paced streaming or a long-lived tunnel.
pub(crate) struct ByocCapabilityFence {
    grant: ByocGrant,
    roster: crate::utils::single_flight::PgAdvisoryLock,
}

impl ByocCapabilityFence {
    pub(crate) fn grant(&self) -> &ByocGrant {
        &self.grant
    }

    pub(crate) async fn release(self) -> AppResult<()> {
        self.roster
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))
    }
}

#[derive(sqlx::FromRow)]
struct ByocGrantRow {
    participation_id: i32,
    title: String,
    container_image: Option<String>,
    build_status: i16,
    build_image_digest: Option<String>,
    expose_port: Option<i32>,
    game_secret: String,
    team_secret: String,
}

impl From<ByocGrantRow> for ByocGrant {
    fn from(row: ByocGrantRow) -> Self {
        Self {
            participation_id: row.participation_id,
            title: row.title,
            container_image: row.container_image,
            build_status: row.build_status,
            build_image_digest: row.build_image_digest,
            expose_port: row.expose_port,
            game_secret: row.game_secret,
            team_secret: row.team_secret,
        }
    }
}

/// Re-read and row-lock every mutable grant after the caller has acquired the
/// matching team-roster advisory lock. `require_active_game` is false for setup
/// bundles so teams may prepare during warmup; their embedded bearer URLs still
/// require an active game when used.
pub(crate) async fn load_byoc_grant_on(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    participation_id: i32,
    team_id: i32,
    challenge_id: i32,
    require_active_game: bool,
) -> AppResult<Option<ByocGrant>> {
    if !crate::services::ad::roster::lock_team_shared_credentials_on(connection, team_id).await? {
        return Ok(None);
    }

    let row = sqlx::query_as::<_, ByocGrantRow>(
        r#"SELECT participation.id AS participation_id,
                  challenge.title,
                  challenge.container_image,
                  challenge.build_status,
                  challenge.build_image_digest,
                  challenge.expose_port,
                  game.private_key AS game_secret,
                  team.invite_token AS team_secret
             FROM "Participations" participation
             JOIN "Games" game ON game.id = participation.game_id
             JOIN "Teams" team ON team.id = participation.team_id
             JOIN "GameChallenges" challenge
               ON challenge.game_id = participation.game_id
              AND challenge.id = $4
            WHERE participation.id = $2
              AND participation.game_id = $1
              AND participation.team_id = $3
              AND participation.status = $6
              AND NOT team.deletion_pending
              AND game.deletion_pending = FALSE
              AND challenge."Type" = $7
              AND challenge.ad_self_hosted = TRUE
              AND challenge.is_enabled = TRUE
              AND challenge.deletion_pending = FALSE
              AND challenge.review_status = $8
              AND (
                    $5 = FALSE OR (
                        game.start_time_utc <= clock_timestamp()
                        AND clock_timestamp() <= game.end_time_utc
                    )
              )
            FOR SHARE OF participation, game, team, challenge"#,
    )
    .bind(game_id)
    .bind(participation_id)
    .bind(team_id)
    .bind(challenge_id)
    .bind(require_active_game)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(ChallengeType::AttackDefense as i16)
    .bind(ChallengeReviewStatus::Active as i16)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(row.map(Into::into))
}

/// Validate a deterministic BYOC bearer against one atomic snapshot and retain
/// all corresponding read fences for the caller-selected lifetime.
pub(crate) async fn authorize_byoc_capability(
    pool: &sqlx::PgPool,
    game_id: i32,
    participation_id: i32,
    challenge_id: i32,
    domain: &str,
    token: &str,
) -> AppResult<Option<ByocCapabilityFence>> {
    // The preliminary team id chooses the advisory domain only. It grants
    // nothing: the fenced query below requires the participation to still map
    // to this exact team, closing a concurrent move/delete/recreate race.
    let Some(team_id) = sqlx::query_scalar::<_, i32>(
        r#"SELECT team_id FROM "Participations"
            WHERE id = $1 AND game_id = $2"#,
    )
    .bind(participation_id)
    .bind(game_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    else {
        return Ok(None);
    };

    let key = format!("team-roster:{team_id}");
    let Some(mut roster) =
        crate::utils::single_flight::PgAdvisoryLock::try_acquire_shared(pool, &key).await?
    else {
        return Err(AppError::unavailable(
            "Team credentials are changing; retry this request",
        ));
    };
    let grant = load_byoc_grant_on(
        roster.transaction_mut(),
        game_id,
        participation_id,
        team_id,
        challenge_id,
        true,
    )
    .await?;
    let Some(grant) = grant else {
        roster.release().await?;
        return Ok(None);
    };
    let expected = super::byoc::byoc_token(
        domain,
        &grant.game_secret,
        &grant.team_secret,
        grant.participation_id,
        challenge_id,
    );
    if !crate::utils::crypto_utils::ct_eq(&expected, token) {
        roster.release().await?;
        return Ok(None);
    }
    Ok(Some(ByocCapabilityFence { grant, roster }))
}

#[cfg(test)]
mod tests;
