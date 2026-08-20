//! Current-request identity handling at the game-registration boundary.

use super::*;
pub(crate) struct GameJoinIdentityScope {
    policy: PolicyFlags,
    identity: PreparedIdentity,
    locked_game_ids: Vec<i32>,
}

pub(crate) async fn lock_game_join_identity_scope(
    transaction: &mut Transaction<'_, Postgres>,
    config: &AppConfig,
    user_id: Uuid,
    current_ip: Option<&str>,
    fingerprint: Option<&str>,
) -> AppResult<GameJoinIdentityScope> {
    let policy = lock_and_load_admission_policy(transaction).await?;
    let identity = prepare_identity(
        config.identity_hash_key.as_bytes(),
        current_ip,
        policy
            .fingerprint_required()
            .then_some(fingerprint)
            .flatten(),
    );
    validate_required_identity(policy, &identity)?;
    lock_identity_user_scope(transaction, user_id).await?;
    lock_identity_values(transaction, &identity).await?;
    Ok(GameJoinIdentityScope {
        policy,
        identity,
        locked_game_ids: Vec::new(),
    })
}

/// After the caller owns the target game's advisory lock, acquire every Game
/// row relevant to observation recording in primary-key order. Keeping this a
/// separate phase avoids Game-row -> game-advisory inversion with editors.
pub(crate) async fn lock_game_join_observation_games(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    target_game_id: i32,
    scope: &mut GameJoinIdentityScope,
) -> AppResult<()> {
    scope.locked_game_ids =
        lock_observation_games(transaction, user_id, Some(target_game_id)).await?;
    Ok(())
}

pub(crate) struct GameJoinIdentityDecision {
    conflict: Option<Conflict>,
    observed_at: DateTime<Utc>,
}

impl GameJoinIdentityDecision {
    pub(crate) fn outcome(&self) -> AdmissionOutcome {
        if self.conflict.is_some() {
            AdmissionOutcome::Blocked
        } else {
            AdmissionOutcome::Accepted
        }
    }
}

/// Evaluate after ordinary join rules and the live-account guard, but defer
/// every identity write until the participation mutation has succeeded.
pub(crate) async fn evaluate_game_join_identity(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    scope: &GameJoinIdentityScope,
) -> AppResult<GameJoinIdentityDecision> {
    let observed_at = database_now(transaction).await?;
    let since = observed_at - Duration::hours(IDENTITY_WINDOW_HOURS);
    let conflict =
        find_conflict(transaction, scope.policy, user_id, &scope.identity, since).await?;
    Ok(GameJoinIdentityDecision {
        conflict,
        observed_at,
    })
}

/// Persist a previously evaluated decision. Call an accepted decision only
/// after the participation link is staged so its exact game context is visible;
/// call a blocked decision without staging membership so only its audit lands.
pub(crate) async fn record_game_join_identity_decision(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    user_name: Option<&str>,
    scope: &GameJoinIdentityScope,
    decision: &GameJoinIdentityDecision,
) -> AppResult<()> {
    if let Some(conflict) = &decision.conflict {
        record_block(
            transaction,
            user_id,
            user_name,
            conflict,
            decision.observed_at,
        )
        .await
    } else {
        record_observations(
            transaction,
            user_id,
            &scope.identity,
            IdentitySource::GameJoin,
            decision.observed_at,
            Some(&scope.locked_game_ids),
        )
        .await
    }
}

/// Attach accepted global identities from the last 24 hours to a newly linked
/// accepted game participation. The caller must already hold
/// [`lock_identity_user_scope`] and the target Game row `FOR SHARE`; those
/// locks serialize this copy with login observation writers and finalization.
/// Original source/timestamp provenance is preserved, and replay is idempotent.
pub(crate) async fn snapshot_recent_global_observations_for_game(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    game_id: i32,
    team_id: i32,
    participation_id: i32,
) -> AppResult<u64> {
    let inserted = sqlx::query(
        r#"WITH snapshot_clock AS MATERIALIZED (
               SELECT clock_timestamp() AS now
           ), candidates AS MATERIALIZED (
               SELECT DISTINCT ON (
                          observation.kind, observation.value_hash,
                          observation.source, observation.observed_at_utc
                      )
                      observation.kind, observation.value_hash,
                      observation.subnet_group_hash,
                      observation.broad_network_hash,
                      observation.value_hint, observation.source,
                      observation.observed_at_utc
                 FROM "IdentityObservations" observation
                 JOIN "Games" game ON game.id = $2
                 JOIN "Participations" participation
                   ON participation.id = $4
                  AND participation.game_id = game.id
                  AND participation.team_id = $3
                 JOIN "Teams" team
                   ON team.id = participation.team_id
                  AND team.deletion_pending = FALSE
                 CROSS JOIN snapshot_clock
                WHERE observation.user_id = $1
                  AND observation.team_id IS NULL
                  AND observation.game_id IS NULL
                  AND observation.participation_id IS NULL
                  AND observation.kind IN ('Ip', 'Fingerprint')
                  AND observation.observed_at_utc >=
                      snapshot_clock.now - INTERVAL '24 hours'
                  AND observation.observed_at_utc >= game.start_time_utc
                  AND observation.observed_at_utc < game.end_time_utc
                  AND game.deletion_pending = FALSE
                  AND game.start_time_utc <= snapshot_clock.now
                  AND game.end_time_utc > snapshot_clock.now
                  AND participation.status IN ($5, $6)
                  AND (
                        team.captain_id = $1
                        OR EXISTS (
                            SELECT 1 FROM "TeamMembers" member
                             WHERE member.team_id = team.id
                               AND member.user_id = $1
                        )
                  )
                  AND NOT EXISTS (
                        SELECT 1
                          FROM "SuspicionReconciliationState" reconciliation
                         WHERE reconciliation.game_id = game.id
                           AND reconciliation.evidence_closed_at_utc IS NOT NULL
                  )
                ORDER BY observation.kind, observation.value_hash,
                         observation.source, observation.observed_at_utc,
                         observation.id
           )
           INSERT INTO "IdentityObservations"
                (user_id, team_id, game_id, participation_id, kind, value_hash,
                 subnet_group_hash, broad_network_hash, value_hint, source,
                 observed_at_utc)
           SELECT $1, $3, $2, $4, candidate.kind, candidate.value_hash,
                  candidate.subnet_group_hash, candidate.broad_network_hash,
                  candidate.value_hint, candidate.source,
                  candidate.observed_at_utc
             FROM candidates candidate
            WHERE NOT EXISTS (
                  SELECT 1 FROM "IdentityObservations" existing
                   WHERE existing.user_id = $1
                     AND existing.team_id = $3
                     AND existing.game_id = $2
                     AND existing.participation_id = $4
                     AND existing.kind = candidate.kind
                     AND existing.value_hash = candidate.value_hash
                     AND existing.source = candidate.source
                     AND existing.observed_at_utc = candidate.observed_at_utc
            )"#,
    )
    .bind(user_id)
    .bind(game_id)
    .bind(team_id)
    .bind(participation_id)
    .bind(crate::utils::enums::ParticipationStatus::Accepted as i16)
    .bind(crate::utils::enums::ParticipationStatus::Suspended as i16)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(inserted.rows_affected())
}
