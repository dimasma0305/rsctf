//! Registration review and participation-status lifecycle.

use std::future::Future;

use axum::extract::{Path, State};
use axum::Json;
use sea_orm::ActiveEnum;
use serde::{Deserialize, Deserializer};
use sqlx::Connection as _;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::utils::enums::ParticipationStatus;
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::MessageResponse;

/// RSCTF `ParticipationEditModel`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParticipationEditModel {
    #[serde(default)]
    pub status: Option<ParticipationStatus>,
    /// Missing leaves the current division unchanged, explicit `null` clears
    /// it, and a number selects that division. The nested option preserves the
    /// existing JSON contract while avoiding status-only accidental clears.
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub division_id: Option<Option<i32>>,
}

fn deserialize_present_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Clone, Copy, Debug)]
struct ParticipationIdentity {
    id: i32,
    game_id: i32,
    team_id: i32,
}

/// Bounded cross-replica ownership of one team's review side effects.
///
/// The PostgreSQL session lock is acquired before the short status transaction
/// and retained after it commits. Opposing reviews therefore cannot revoke a
/// freshly-provisioned team or provision a team whose rejection already won.
/// The underlying roster admission semaphore bounds connections retained while
/// the container/VPN side effect is in flight.
struct ParticipationReviewLease {
    session: crate::utils::single_flight::PgSessionAdvisoryLock,
    local: crate::utils::single_flight::CoalesceGuard,
}

impl ParticipationReviewLease {
    async fn acquire(pool: &sqlx::PgPool, team_id: i32) -> AppResult<Self> {
        let key = format!("team-roster:{team_id}");
        let local = crate::utils::single_flight::coalesce(&key).await;
        let session =
            crate::utils::single_flight::PgSessionAdvisoryLock::acquire_roster(pool, &key)
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
        Ok(Self { session, local })
    }

    fn connection_mut(&mut self) -> &mut sqlx::PgConnection {
        self.session.connection_mut()
    }

    async fn terminal_status_matches(
        &mut self,
        identity: ParticipationIdentity,
        expected: ParticipationStatus,
    ) -> AppResult<bool> {
        sqlx::query_scalar(
            r#"SELECT EXISTS(
                   SELECT 1
                     FROM "Participations" participation
                     JOIN "Teams" team ON team.id = participation.team_id
                     JOIN "Games" game ON game.id = participation.game_id
                    WHERE participation.id = $1
                      AND participation.game_id = $2
                      AND participation.team_id = $3
                      AND participation.status = $4
                      AND team.deletion_pending = FALSE
                      AND game.deletion_pending = FALSE
               )"#,
        )
        .bind(identity.id)
        .bind(identity.game_id)
        .bind(identity.team_id)
        .bind(expected as i16)
        .fetch_one(self.connection_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))
    }

    async fn release(self) -> AppResult<()> {
        let Self { session, local } = self;
        session
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        drop(local);
        Ok(())
    }
}

async fn persist_participation_status(
    lease: &mut ParticipationReviewLease,
    identity: ParticipationIdentity,
    requested_status: ParticipationStatus,
    requested_division_id: Option<Option<i32>>,
) -> AppResult<()> {
    let mut transaction = lease
        .connection_mut()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    crate::utils::single_flight::acquire_transaction_advisory_lock(
        &mut transaction,
        &crate::services::ad_engine::game_lock_key(identity.game_id),
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let live: Option<(i16, Option<i32>, i32, bool, bool)> = sqlx::query_as(
        r#"SELECT participation.status,
                  participation.division_id,
                  participation.team_id,
                  team.deletion_pending,
                  game.deletion_pending
             FROM "Participations" participation
            JOIN "Teams" team ON team.id = participation.team_id
             JOIN "Games" game ON game.id = participation.game_id
            WHERE participation.id = $1 AND participation.game_id = $2
            FOR UPDATE OF participation, team"#,
    )
    .bind(identity.id)
    .bind(identity.game_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((
        live_status_value,
        live_division_id,
        live_team_id,
        team_deletion_pending,
        game_deletion_pending,
    )) = live
    else {
        return Err(AppError::not_found("Participation not found"));
    };
    if live_team_id != identity.team_id {
        return Err(AppError::conflict(
            "Participation changed; retry the request",
        ));
    }
    if team_deletion_pending || game_deletion_pending {
        return Err(AppError::conflict("Team or game is being deleted"));
    }
    let live_status = <ParticipationStatus as ActiveEnum>::try_from_value(&live_status_value)
        .map_err(|error| AppError::internal(error.to_string()))?;
    // RSCTF ParticipationRepository.UpdateDivision: an unknown/out-of-game id
    // is ignored; explicit null clears. Rejection always clears it.
    let mut division_id = resolve_requested_division(
        &mut transaction,
        identity.game_id,
        live_division_id,
        requested_division_id,
    )
    .await?;
    if requested_status == ParticipationStatus::Rejected {
        division_id = None;
    }
    crate::services::participation_evidence::ensure_evidence_preserving_update(
        &mut transaction,
        identity.id,
        live_status,
        requested_status,
        live_division_id,
        division_id,
    )
    .await?;
    let scoring_started = crate::controllers::edit::ad_epoch_scoring_started_locked(
        &mut transaction,
        identity.game_id,
    )
    .await?;
    // Suspension and reinstatement are the only reversible status mutations
    // after scoring starts. They retain the same participation and division;
    // rejection remains subject to both the engine boundary and evidence fence.
    crate::controllers::edit::ensure_ad_roster_status_mutable(
        scoring_started,
        Some(live_status),
        requested_status,
    )?;
    ensure_scored_division_unchanged(scoring_started, live_division_id, division_id)?;
    sqlx::query(
        r#"UPDATE "Participations"
              SET status = $1, division_id = $2
            WHERE id = $3 AND game_id = $4"#,
    )
    .bind(requested_status as i16)
    .bind(division_id)
    .bind(identity.id)
    .bind(identity.game_id)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if requested_status == ParticipationStatus::Accepted {
        sqlx::query(r#"UPDATE "Teams" SET locked = TRUE WHERE id = $1"#)
            .bind(identity.team_id)
            .execute(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

async fn resolve_requested_division(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    live_division_id: Option<i32>,
    requested_division_id: Option<Option<i32>>,
) -> AppResult<Option<i32>> {
    let candidate = match requested_division_id {
        None => return Ok(live_division_id),
        Some(None) => return Ok(None),
        Some(Some(candidate)) => candidate,
    };
    let in_game: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
              SELECT 1 FROM "Divisions" WHERE id = $1 AND game_id = $2
           )"#,
    )
    .bind(candidate)
    .bind(game_id)
    .fetch_one(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(if in_game {
        Some(candidate)
    } else {
        live_division_id
    })
}

fn ensure_scored_division_unchanged(
    scoring_started: bool,
    current: Option<i32>,
    requested: Option<i32>,
) -> AppResult<()> {
    if scoring_started && current != requested {
        return Err(AppError::bad_request(
            "Participation division cannot change after A&D/KotH epoch scoring has started.",
        ));
    }
    Ok(())
}

async fn run_terminal_effect<F, Fut>(
    lease: &mut ParticipationReviewLease,
    identity: ParticipationIdentity,
    expected: ParticipationStatus,
    effect: F,
) -> AppResult<()>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = AppResult<()>>,
{
    // This is intentionally the final await before entering the side effect.
    // The retained session lock prevents a supported status writer from changing
    // the row between this check and completion of the external operation.
    if !lease.terminal_status_matches(identity, expected).await? {
        return Err(AppError::conflict(
            "Participation status changed; retry the request",
        ));
    }
    effect().await
}

async fn update_division_only(
    lease: &mut ParticipationReviewLease,
    identity: ParticipationIdentity,
    requested_division_id: Option<Option<i32>>,
) -> AppResult<()> {
    let mut transaction = lease
        .connection_mut()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    crate::utils::single_flight::acquire_transaction_advisory_lock(
        &mut transaction,
        &crate::services::ad_engine::game_lock_key(identity.game_id),
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let live: Option<(i16, Option<i32>, i32, bool, bool)> = sqlx::query_as(
        r#"SELECT participation.status,
                  participation.division_id,
                  participation.team_id,
                  team.deletion_pending,
                  game.deletion_pending
             FROM "Participations" participation
            JOIN "Teams" team ON team.id = participation.team_id
             JOIN "Games" game ON game.id = participation.game_id
            WHERE participation.id = $1 AND participation.game_id = $2
            FOR UPDATE OF participation, team"#,
    )
    .bind(identity.id)
    .bind(identity.game_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((
        live_status_value,
        live_division_id,
        live_team_id,
        team_deletion_pending,
        game_deletion_pending,
    )) = live
    else {
        return Err(AppError::not_found("Participation not found"));
    };
    if live_team_id != identity.team_id {
        return Err(AppError::conflict(
            "Participation changed; retry the request",
        ));
    }
    if team_deletion_pending || game_deletion_pending {
        return Err(AppError::conflict("Team or game is being deleted"));
    }

    let division_id = resolve_requested_division(
        &mut transaction,
        identity.game_id,
        live_division_id,
        requested_division_id,
    )
    .await?;
    let live_status = <ParticipationStatus as ActiveEnum>::try_from_value(&live_status_value)
        .map_err(|error| AppError::internal(error.to_string()))?;
    crate::services::participation_evidence::ensure_evidence_preserving_update(
        &mut transaction,
        identity.id,
        live_status,
        live_status,
        live_division_id,
        division_id,
    )
    .await?;
    let scoring_started = crate::controllers::edit::ad_epoch_scoring_started_locked(
        &mut transaction,
        identity.game_id,
    )
    .await?;
    ensure_scored_division_unchanged(scoring_started, live_division_id, division_id)?;
    sqlx::query(
        r#"UPDATE "Participations"
              SET division_id = $1
            WHERE id = $2 AND game_id = $3"#,
    )
    .bind(division_id)
    .bind(identity.id)
    .bind(identity.game_id)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

/// `PUT /api/admin/participation/{id}` — update a participation's status /
/// division (registration review).
///
/// RSCTF's `AdminController.Participation` is `[RequireUser]`, not
/// `[RequireAdmin]`: a platform Admin OR an EventManager of the participation's
/// game may review it. 404-before-403 ordering matches RSCTF.
pub async fn update_participation(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    Json(model): Json<ParticipationEditModel>,
) -> AppResult<MessageResponse> {
    let identity = sqlx::query_as::<_, (i32, i32)>(
        r#"SELECT game_id, team_id FROM "Participations" WHERE id = $1"#,
    )
    .bind(id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .map(|(game_id, team_id)| ParticipationIdentity {
        id,
        game_id,
        team_id,
    })
    .ok_or_else(|| AppError::not_found("Participation not found"))?;

    if !user.is_admin() {
        let is_manager: bool = sqlx::query_scalar(
            r#"SELECT EXISTS(
                   SELECT 1 FROM "GameManagers"
                    WHERE game_id = $1 AND user_id = $2
               )"#,
        )
        .bind(identity.game_id)
        .bind(user.id)
        .fetch_one(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if !is_manager {
            return Err(AppError::Forbidden);
        }
    }

    if let Some(requested_status) = model.status {
        let mut lease = ParticipationReviewLease::acquire(st.pg(), identity.team_id).await?;
        persist_participation_status(&mut lease, identity, requested_status, model.division_id)
            .await?;

        let effect = run_terminal_effect(&mut lease, identity, requested_status, || async {
            if requested_status == ParticipationStatus::Accepted {
                crate::controllers::edit::provision_accepted_participation(
                    &st,
                    identity.game_id,
                    identity.id,
                )
                .await
            } else {
                crate::controllers::team::revoke_participation_capabilities(&st, identity.id).await
            }
        })
        .await;
        let release = lease.release().await;
        effect?;
        release?;
    } else {
        let mut lease = ParticipationReviewLease::acquire(st.pg(), identity.team_id).await?;
        let update = update_division_only(&mut lease, identity, model.division_id).await;
        let release = lease.release().await;
        update?;
        release?;
    }

    // Participation status is a scoring and access input. Evict every live and
    // frozen board family plus each member's accepted-participation cache.
    for key in [
        format!("_ScoreBoard_{}", identity.game_id),
        format!("_ScoreBoardFrozen_{}", identity.game_id),
        format!("_KothScoreBoard_{}", identity.game_id),
        format!("_KothScoreBoardFrozen_{}", identity.game_id),
        format!("_KothTimeline_{}", identity.game_id),
        format!("_KothTimelineFrozen_{}", identity.game_id),
    ] {
        st.cache.remove(&key).await;
    }
    crate::controllers::game::ad::hard_invalidate_ad_scoreboard(&st, identity.game_id).await;
    crate::controllers::game::ad::flush_participation_cache(&st, identity.game_id, identity.id)
        .await;

    Ok(MessageResponse::ok(""))
}

#[cfg(test)]
#[path = "participation_tests.rs"]
mod tests;
