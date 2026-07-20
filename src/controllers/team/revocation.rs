//! Fail-closed roster and participation capability revocation.

use sqlx::Connection as _;
use uuid::Uuid;

use super::lifecycle::destroy_participation_ad_services;
use crate::app_state::SharedState;
use crate::models::data::participation;
use crate::utils::codec::random_hex;
use crate::utils::enums::ParticipationStatus;
use crate::utils::error::{AppError, AppResult};

#[derive(sqlx::FromRow)]
struct ParticipationRow {
    id: i32,
    status: i16,
    token: String,
    writeup_id: Option<i32>,
    game_id: i32,
    team_id: i32,
    division_id: Option<i32>,
    suspicion_score: i32,
}

impl TryFrom<ParticipationRow> for participation::Model {
    type Error = AppError;

    fn try_from(row: ParticipationRow) -> Result<Self, Self::Error> {
        let status = match row.status {
            value if value == ParticipationStatus::Pending as i16 => ParticipationStatus::Pending,
            value if value == ParticipationStatus::Accepted as i16 => ParticipationStatus::Accepted,
            value if value == ParticipationStatus::Rejected as i16 => ParticipationStatus::Rejected,
            value if value == ParticipationStatus::Suspended as i16 => {
                ParticipationStatus::Suspended
            }
            value if value == ParticipationStatus::Unsubmitted as i16 => {
                ParticipationStatus::Unsubmitted
            }
            _ => return Err(AppError::internal("Invalid participation status")),
        };
        Ok(Self {
            id: row.id,
            status,
            token: row.token,
            writeup_id: row.writeup_id,
            game_id: row.game_id,
            team_id: row.team_id,
            division_id: row.division_id,
            suspicion_score: row.suspicion_score,
        })
    }
}

async fn team_participations(
    pool: &sqlx::PgPool,
    team_id: i32,
) -> AppResult<Vec<participation::Model>> {
    let rows = sqlx::query_as::<_, ParticipationRow>(
        r#"SELECT id, status, token, writeup_id, game_id, team_id,
                  division_id, suspicion_score
             FROM "Participations"
            WHERE team_id = $1"#,
    )
    .bind(team_id)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    rows.into_iter().map(TryInto::try_into).collect()
}

async fn rotate_team_invite_secret(pool: &sqlx::PgPool, team_id: i32) -> Result<(), sqlx::Error> {
    sqlx::query(r#"UPDATE "Teams" SET invite_token = $1 WHERE id = $2"#)
        .bind(random_hex(16))
        .bind(team_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// A bounded per-team roster mutation lock. Some roster changes must finish
/// credential teardown before their membership deletion can commit; sharing
/// the credential-issuance admission gate keeps those retained transactions
/// from exhausting the pool while teardown performs nested DB/kernel work.
pub(crate) struct RosterMutationLock {
    distributed: crate::utils::single_flight::PgAdvisoryLock,
    local: crate::utils::single_flight::CoalesceGuard,
    _admission: tokio::sync::OwnedSemaphorePermit,
}

impl RosterMutationLock {
    pub(crate) fn advisory_mut(&mut self) -> &mut crate::utils::single_flight::PgAdvisoryLock {
        &mut self.distributed
    }

    pub(crate) fn transaction_mut(&mut self) -> &mut sqlx::Transaction<'static, sqlx::Postgres> {
        self.distributed.transaction_mut()
    }

    pub(crate) async fn release(self) -> AppResult<()> {
        let Self {
            distributed,
            local,
            _admission,
        } = self;
        distributed
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        drop(local);
        drop(_admission);
        Ok(())
    }

    /// Commit the roster transaction before external teardown while retaining
    /// only the process-local gate. This releases the bounded admission permit
    /// and pooled connection; a later final cascade reacquires the distributed
    /// lock without allowing same-replica mutations to queue ahead of it.
    pub(crate) async fn release_for_external(
        self,
    ) -> AppResult<crate::utils::single_flight::CoalesceGuard> {
        let Self {
            distributed,
            local,
            _admission,
        } = self;
        distributed
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        drop(_admission);
        Ok(local)
    }
}

pub(crate) async fn acquire_roster_mutation(
    pool: &sqlx::PgPool,
    team_id: i32,
) -> AppResult<RosterMutationLock> {
    let key = format!("team-roster:{team_id}");
    let local = crate::utils::single_flight::coalesce(&key).await;
    let admission = crate::utils::single_flight::roster_access_permit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let distributed = crate::utils::single_flight::PgAdvisoryLock::acquire(pool, &key)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(RosterMutationLock {
        distributed,
        local,
        _admission: admission,
    })
}

/// Reject ordinary team mutations after deletion has durably started. Callers
/// hold the team-roster advisory lock in this transaction, so the check cannot
/// race the deletion fence or final cascade on another replica.
pub(crate) async fn require_team_mutable(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    team_id: i32,
) -> AppResult<()> {
    let deletion_pending: Option<bool> =
        sqlx::query_scalar(r#"SELECT deletion_pending FROM "Teams" WHERE id = $1"#)
            .bind(team_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    match deletion_pending {
        Some(false) => Ok(()),
        Some(true) => Err(AppError::conflict("Team is being deleted")),
        None => Err(AppError::not_found("Team not found")),
    }
}

/// Atomically remove all database ownership rows once external teardown has
/// succeeded. The caller reacquires the team-roster advisory lock first. A
/// missing team is a successful concurrent duplicate; a non-fenced team can
/// never be finalized through this path.
pub(crate) async fn finalize_team_deletion(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    team_id: i32,
) -> AppResult<bool> {
    let deletion_pending: Option<bool> =
        sqlx::query_scalar(r#"SELECT deletion_pending FROM "Teams" WHERE id = $1 FOR UPDATE"#)
            .bind(team_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    match deletion_pending {
        None => return Ok(false),
        Some(false) => {
            return Err(AppError::conflict(
                "Team deletion has not been durably fenced",
            ));
        }
        Some(true) => {}
    }

    let game_ids: Vec<i32> = sqlx::query_scalar(
        r#"SELECT DISTINCT game_id
              FROM "Participations"
             WHERE team_id = $1
             ORDER BY game_id"#,
    )
    .bind(team_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    for &game_id in &game_ids {
        crate::utils::single_flight::acquire_transaction_advisory_lock(
            transaction,
            &crate::services::ad_engine::game_lock_key(game_id),
        )
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }

    // External teardown deliberately commits the initial deletion fence before
    // doing slow container/network work. Re-read and lock every identity now:
    // an audit/scoring writer either commits first and blocks this cascade, or
    // waits and observes the identity gone after this transaction commits.
    let participations: Vec<(i32, i32)> = sqlx::query_as(
        r#"SELECT id, game_id
              FROM "Participations"
             WHERE team_id = $1
             ORDER BY game_id, id
             FOR UPDATE"#,
    )
    .bind(team_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    for &game_id in &game_ids {
        let protected = sqlx::query_scalar::<_, bool>(
            r#"SELECT ad_scoring_start_round IS NOT NULL
                      OR koth_scoring_start_round IS NOT NULL
                 FROM "Games" WHERE id = $1"#,
        )
        .bind(game_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .unwrap_or(false);
        if protected {
            return Err(AppError::bad_request(
                "A team cannot be deleted after its game has started or recorded competition evidence.",
            ));
        }
    }
    for &(participation_id, _) in &participations {
        if crate::services::participation_evidence::has_competition_evidence(
            transaction,
            participation_id,
        )
        .await?
        {
            return Err(AppError::bad_request(
                "A team cannot be deleted after its game has started or recorded competition evidence.",
            ));
        }
    }

    sqlx::query(r#"DELETE FROM "Participations" WHERE team_id = $1"#)
        .bind(team_id)
        .execute(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(r#"DELETE FROM "UserParticipations" WHERE team_id = $1"#)
        .bind(team_id)
        .execute(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(r#"DELETE FROM "TeamMembers" WHERE team_id = $1"#)
        .bind(team_id)
        .execute(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let removed = sqlx::query_scalar::<_, i32>(
        r#"DELETE FROM "Teams"
            WHERE id = $1 AND deletion_pending = TRUE
        RETURNING id"#,
    )
    .bind(team_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if removed.is_none() {
        return Err(AppError::conflict("Team deletion fence changed"));
    }
    Ok(true)
}

/// Cross-replica ownership of the external phase of one durable team deletion.
/// The session advisory lock stays held while container/network teardown uses
/// independent transactions, preventing duplicate requests from performing the
/// same expensive cleanup concurrently.
pub(crate) struct TeamDeletionLease {
    lock: crate::utils::single_flight::PgSessionAdvisoryLock,
}

impl TeamDeletionLease {
    pub(crate) async fn acquire(
        pool: &sqlx::PgPool,
        roster_key: &str,
        team_id: i32,
    ) -> AppResult<Option<Self>> {
        let mut lock =
            crate::utils::single_flight::PgSessionAdvisoryLock::acquire_roster(pool, roster_key)
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
        let pending: Option<bool> =
            sqlx::query_scalar(r#"SELECT deletion_pending FROM "Teams" WHERE id = $1"#)
                .bind(team_id)
                .fetch_optional(lock.connection_mut())
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
        match pending {
            Some(true) => Ok(Some(Self { lock })),
            Some(false) => Err(AppError::conflict(
                "Team deletion has not been durably fenced",
            )),
            None => {
                lock.release()
                    .await
                    .map_err(|error| AppError::internal(error.to_string()))?;
                Ok(None)
            }
        }
    }

    /// Finalize all ownership rows in one transaction on the connection that
    /// already owns the session lock, then release that lock explicitly.
    pub(crate) async fn finalize(mut self, team_id: i32) -> AppResult<()> {
        let mut transaction = self
            .lock
            .connection_mut()
            .begin()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        finalize_team_deletion(&mut transaction, team_id).await?;
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        self.lock
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))
    }
}

/// Revoke every participation-shared A&D capability for one participation.
pub(crate) async fn revoke_participation_capabilities(
    st: &SharedState,
    participation_id: i32,
) -> AppResult<()> {
    // BYOC tokens are derived from the team invite secret. Rotate it so a bundle
    // rejected once cannot silently become valid again if the participation is
    // later re-accepted. This intentionally invalidates BYOC bundles for every
    // participation of the team until the remaining players download fresh ones.
    let team_id =
        sqlx::query_scalar::<_, i32>(r#"SELECT team_id FROM "Participations" WHERE id = $1"#)
            .bind(participation_id)
            .fetch_optional(st.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    let team_parts = if let Some(team_id) = team_id {
        team_participations(st.pg(), team_id).await?
    } else {
        Vec::new()
    };
    let mut errors = Vec::new();
    if let Some(team_id) = team_id {
        if let Err(error) = rotate_team_invite_secret(st.pg(), team_id).await {
            errors.push(format!("rotate team secret: {error}"));
        }
    }
    if let Err(error) = sqlx::query(r#"DELETE FROM "AdTeamApiTokens" WHERE participation_id = $1"#)
        .bind(participation_id)
        .execute(st.pg())
        .await
    {
        errors.push(format!("revoke API token: {error}"));
    }
    if let Err(error) = sqlx::query(r#"DELETE FROM "AdSshKeys" WHERE participation_id = $1"#)
        .bind(participation_id)
        .execute(st.pg())
        .await
    {
        errors.push(format!("revoke SSH key: {error}"));
    }
    if let Err(error) =
        crate::services::ad_vpn::revoke_peers_for_participations(&st.db, &[participation_id]).await
    {
        errors.push(format!("revoke VPN peer: {error}"));
    }
    if let Err(error) = destroy_participation_ad_services(st, participation_id).await {
        errors.push(format!("destroy A&D service: {error}"));
    }
    if team_parts.is_empty() {
        if let Err(error) = st
            .byoc
            .disconnect_participation(&st.db, participation_id)
            .await
        {
            errors.push(format!("revoke BYOC tunnel: {error}"));
        }
    } else {
        for part in team_parts {
            if let Err(error) = st.byoc.disconnect_participation(&st.db, part.id).await {
                errors.push(format!("revoke BYOC tunnel: {error}"));
            }
        }
    }
    if let Err(error) = crate::services::ad_engine::revoke_koth_capabilities(
        &st.db,
        st.cache.as_ref(),
        &[participation_id],
    )
    .await
    {
        errors.push(format!("revoke KotH capability: {error}"));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(AppError::internal(errors.join("; ")))
    }
}

/// Revoke credentials copied by any member of a team. A roster-removal caller
/// retains a bounded [`RosterMutationLock`] through this teardown and commits
/// the membership deletion only after it succeeds. Other callers may invoke it
/// after a durable participation/team authorization fence has committed.
async fn revoke_team_shared_capabilities_inner(
    st: &SharedState,
    team_id: i32,
    locked_transaction: Option<&mut sqlx::Transaction<'_, sqlx::Postgres>>,
) -> AppResult<(
    Vec<participation::Model>,
    Option<crate::services::ad_engine::KothCapabilityCacheInvalidation>,
)> {
    let parts = team_participations(st.pg(), team_id).await?;
    let part_ids: Vec<i32> = parts.iter().map(|part| part.id).collect();
    let mut errors = Vec::new();
    if let Err(error) = rotate_team_invite_secret(st.pg(), team_id).await {
        errors.push(format!("rotate team secret: {error}"));
    }
    if !part_ids.is_empty() {
        if let Err(error) =
            sqlx::query(r#"DELETE FROM "AdTeamApiTokens" WHERE participation_id = ANY($1)"#)
                .bind(&part_ids)
                .execute(st.pg())
                .await
        {
            errors.push(format!("revoke API tokens: {error}"));
        }
        if let Err(error) =
            sqlx::query(r#"DELETE FROM "AdSshKeys" WHERE participation_id = ANY($1)"#)
                .bind(&part_ids)
                .execute(st.pg())
                .await
        {
            errors.push(format!("revoke SSH keys: {error}"));
        }
        if let Err(error) =
            crate::services::ad_vpn::revoke_peers_for_participations(&st.db, &part_ids).await
        {
            errors.push(format!("revoke VPN peers: {error}"));
        }
    }
    for part in &parts {
        if let Err(error) = st.byoc.disconnect_participation(&st.db, part.id).await {
            errors.push(format!("revoke BYOC tunnel: {error}"));
        }
    }
    let koth_cache_invalidation = if part_ids.is_empty() {
        None
    } else if let Some(transaction) = locked_transaction {
        match crate::services::ad_engine::revoke_koth_capabilities_locked(transaction, &part_ids)
            .await
        {
            Ok(invalidation) => Some(invalidation),
            Err(error) => {
                errors.push(format!("revoke KotH capabilities: {error}"));
                None
            }
        }
    } else {
        if let Err(error) = crate::services::ad_engine::revoke_koth_capabilities(
            &st.db,
            st.cache.as_ref(),
            &part_ids,
        )
        .await
        {
            errors.push(format!("revoke KotH capabilities: {error}"));
        }
        None
    };
    if !errors.is_empty() {
        return Err(AppError::internal(errors.join("; ")));
    }
    Ok((parts, koth_cache_invalidation))
}

pub(crate) async fn revoke_team_shared_capabilities(
    st: &SharedState,
    team_id: i32,
) -> AppResult<Vec<participation::Model>> {
    let (parts, _) = revoke_team_shared_capabilities_inner(st, team_id, None).await?;
    Ok(parts)
}

/// Public leave/kick already owns the roster transaction and its ordered game
/// locks. Reuse that transaction for KotH mutation instead of reacquiring the
/// same lock from another connection. The caller applies the cache plan after
/// committing membership removal.
pub(super) async fn revoke_team_shared_capabilities_locked(
    st: &SharedState,
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    team_id: i32,
) -> AppResult<(
    Vec<participation::Model>,
    crate::services::ad_engine::KothCapabilityCacheInvalidation,
)> {
    let (parts, invalidation) =
        revoke_team_shared_capabilities_inner(st, team_id, Some(transaction)).await?;
    Ok((parts, invalidation.unwrap_or_default()))
}

/// Establish a durable fail-closed gate before team deletion starts teardown.
/// Game keys are ordered and added to the caller's team-lock transaction, so
/// row locking, evidence checks, and the status change are one atomic
/// cross-replica step.
pub(crate) async fn mark_team_participations_revoked(
    control: &mut crate::utils::single_flight::PgAdvisoryLock,
    team_id: i32,
) -> AppResult<()> {
    let deletion_pending: Option<bool> =
        sqlx::query_scalar(r#"SELECT deletion_pending FROM "Teams" WHERE id = $1"#)
            .bind(team_id)
            .fetch_optional(&mut **control.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(deletion_pending) = deletion_pending else {
        return Err(AppError::not_found("Team not found"));
    };
    let game_ids: Vec<i32> = sqlx::query_scalar(
        r#"SELECT DISTINCT game_id
              FROM "Participations"
             WHERE team_id = $1
             ORDER BY game_id"#,
    )
    .bind(team_id)
    .fetch_all(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    // Lock the games in their stable numeric order before taking any
    // participation row lock. A&D/KotH writers use the same game advisory key,
    // and keeping the hierarchy team -> games -> participations prevents a
    // multi-game team deletion from inverting their lock order.
    for &game_id in &game_ids {
        control
            .acquire_additional(&crate::services::ad_engine::game_lock_key(game_id))
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }

    // Jeopardy submitters hold FOR SHARE on their live participation through
    // commit. Take the conflicting row locks before checking for evidence: if a
    // submit is already in flight, this statement waits for it and the following
    // evidence query receives a fresh READ COMMITTED snapshot containing the
    // committed submission. If deletion wins first, the row stays locked until
    // its status is suspended and a stale submit fails its live-status check.
    let participations: Vec<(i32, i32)> = sqlx::query_as(
        r#"SELECT id, game_id
              FROM "Participations"
             WHERE team_id = $1
             ORDER BY game_id, id
             FOR UPDATE"#,
    )
    .bind(team_id)
    .fetch_all(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    for game_id in game_ids {
        if crate::controllers::edit::ad_epoch_scoring_started_locked(
            control.transaction_mut(),
            game_id,
        )
        .await?
        {
            return Err(AppError::bad_request(
                "A team cannot be deleted after A&D epoch scoring has started.",
            ));
        }
        let game_started: bool = sqlx::query_scalar(
            r#"SELECT start_time_utc <= clock_timestamp()
                 FROM "Games" WHERE id = $1"#,
        )
        .bind(game_id)
        .fetch_optional(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::not_found("Game not found"))?;
        if !deletion_pending && game_started {
            return Err(AppError::bad_request(
                "A team cannot be deleted after its game has started or recorded competition evidence.",
            ));
        }
    }
    for (participation_id, _) in participations {
        if crate::services::participation_evidence::has_competition_evidence(
            control.transaction_mut(),
            participation_id,
        )
        .await?
        {
            return Err(AppError::bad_request(
                "A team cannot be deleted after its game has started or recorded competition evidence.",
            ));
        }
    }

    let fenced = sqlx::query_scalar::<_, i32>(
        r#"UPDATE "Teams"
              SET deletion_pending = TRUE
            WHERE id = $1
        RETURNING id"#,
    )
    .bind(team_id)
    .fetch_optional(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if fenced.is_none() {
        return Err(AppError::not_found("Team not found"));
    }
    sqlx::query(r#"UPDATE "Participations" SET status = $1 WHERE team_id = $2"#)
        .bind(crate::utils::enums::ParticipationStatus::Suspended as i16)
        .bind(team_id)
        .execute(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

/// Persist the roster removal in the transaction that owns the team lock.
pub(super) async fn remove_membership(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    team_id: i32,
    user_id: Uuid,
) -> AppResult<()> {
    // A UserParticipations row is both the live game link and the durable
    // attribution of a player to a team's scoring identity. Lock the parent
    // participations before deciding which links are mutable: an in-flight
    // evidence writer either commits first and is observed below, or waits
    // until this transaction has retained its link.
    let linked_participations: Vec<(i32, i16, bool)> = sqlx::query_as(
        r#"SELECT participation.id,
                  participation.status,
                  COALESCE(game.end_time_utc <= clock_timestamp(), FALSE) AS completed
              FROM "UserParticipations" membership
              JOIN "Participations" participation
                ON participation.id = membership.participation_id
         LEFT JOIN "Games" game ON game.id = participation.game_id
             WHERE membership.team_id = $1 AND membership.user_id = $2
             ORDER BY participation.id
             FOR UPDATE OF membership, participation"#,
    )
    .bind(team_id)
    .bind(user_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let mut removable_participation_ids = Vec::new();
    for (participation_id, status, completed) in linked_participations {
        let known_status = matches!(
            status,
            value if value == ParticipationStatus::Pending as i16
                || value == ParticipationStatus::Accepted as i16
                || value == ParticipationStatus::Rejected as i16
                || value == ParticipationStatus::Suspended as i16
                || value == ParticipationStatus::Unsubmitted as i16
        );
        let completed_scoring_identity = completed
            && matches!(
                status,
                value if value == ParticipationStatus::Accepted as i16
                    || value == ParticipationStatus::Suspended as i16
            );
        let owns_evidence = if completed_scoring_identity {
            false
        } else {
            crate::services::participation_evidence::has_competition_evidence(
                transaction,
                participation_id,
            )
            .await?
        };
        // Unknown legacy statuses fail closed. A completed Accepted/Suspended
        // row is the official zero-score roster identity even when it owns no
        // child evidence; removing it would permit a post-event re-registration
        // to rewrite who represented the team.
        if !completed_scoring_identity && !owns_evidence && known_status {
            removable_participation_ids.push(participation_id);
        }
    }

    sqlx::query(r#"DELETE FROM "TeamMembers" WHERE team_id = $1 AND user_id = $2"#)
        .bind(team_id)
        .bind(user_id)
        .execute(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    // Completed Accepted/Suspended and evidence-bearing links remain as
    // immutable historical attribution. Dangling legacy links and live,
    // unscored registrations still disappear, preserving the existing
    // leave/kick revocation behavior for current game membership.
    sqlx::query(
        r#"DELETE FROM "UserParticipations" membership
            WHERE membership.team_id = $1
              AND membership.user_id = $2
              AND (
                    membership.participation_id = ANY($3)
                    OR NOT EXISTS (
                        SELECT 1 FROM "Participations" participation
                         WHERE participation.id = membership.participation_id
                    )
              )"#,
    )
    .bind(team_id)
    .bind(user_id)
    .bind(&removable_participation_ids)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

pub(crate) async fn invalidate_removed_membership_cache(
    st: &SharedState,
    user_id: Uuid,
    parts: &[participation::Model],
) -> AppResult<()> {
    for part in parts {
        st.cache
            .remove(&crate::controllers::game::ad::participation_cache_key(
                user_id,
                part.game_id,
            ))
            .await;
    }
    Ok(())
}

#[cfg(test)]
#[path = "revocation_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "revocation_retry_tests.rs"]
mod retry_tests;

#[cfg(test)]
#[path = "revocation_model_tests.rs"]
mod model_tests;
