//! Transactional game-membership mutations shared by the join/leave surface.

use super::*;

pub(super) fn game_membership_lock_key(user_id: Uuid, game_id: i32) -> String {
    format!("game-membership:{game_id}:{user_id}")
}

/// Ordered local + cross-replica locks for one game-membership mutation.
///
/// The local user + team gates are acquired before the PostgreSQL transaction
/// retains a pool connection. A combined path intentionally does not wait for
/// the local game coalescer: the database advisory lock is authoritative, and
/// waiting on that local optimization while retaining a transaction can form a
/// pool cycle with the engine. Database keys preserve user -> team -> game,
/// whose team -> game suffix matches admin review and A&D engine paths.
pub(super) struct MembershipMutationLocks {
    database: crate::utils::single_flight::PgAdvisoryLock,
    game_key: Option<String>,
    team_local: crate::utils::single_flight::CoalesceGuard,
    membership_local: crate::utils::single_flight::CoalesceGuard,
}

impl MembershipMutationLocks {
    pub(super) async fn acquire(
        pool: &sqlx::PgPool,
        user_id: Uuid,
        game_id: i32,
        team_id: i32,
        reserve_game_lock: bool,
    ) -> AppResult<Self> {
        let membership_key = game_membership_lock_key(user_id, game_id);
        let membership_local = crate::utils::single_flight::coalesce(&membership_key).await;
        let team_key = format!("team-roster:{team_id}");
        let team_local = crate::utils::single_flight::coalesce(&team_key).await;
        let game_key =
            reserve_game_lock.then(|| crate::services::ad_engine::game_lock_key(game_id));

        let mut database =
            crate::utils::single_flight::PgAdvisoryLock::acquire(pool, &membership_key)
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
        database
            .acquire_additional(&team_key)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        Ok(Self {
            database,
            game_key,
            team_local,
            membership_local,
        })
    }

    pub(super) fn transaction_mut(&mut self) -> &mut sqlx::Transaction<'static, sqlx::Postgres> {
        self.database.transaction_mut()
    }

    pub(super) async fn acquire_game_advisory(&mut self) -> AppResult<()> {
        let key = self
            .game_key
            .as_deref()
            .expect("the game advisory key must be reserved for an accepted join");
        self.database
            .acquire_additional(key)
            .await
            .map_err(|error| AppError::internal(error.to_string()))
    }

    pub(super) async fn release(self) -> AppResult<()> {
        let Self {
            database,
            team_local,
            membership_local,
            ..
        } = self;
        database
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        drop(team_local);
        drop(membership_local);
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(super) struct ExistingTeamParticipation {
    pub(super) status: i16,
}

pub(super) async fn existing_team_participation_locked(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    team_id: i32,
) -> AppResult<Option<ExistingTeamParticipation>> {
    sqlx::query_scalar::<_, i16>(
        r#"SELECT status
              FROM "Participations"
             WHERE game_id = $1 AND team_id = $2
             ORDER BY id
             LIMIT 1"#,
    )
    .bind(game_id)
    .bind(team_id)
    .fetch_optional(&mut **transaction)
    .await
    .map(|row| row.map(|status| ExistingTeamParticipation { status }))
    .map_err(|error| AppError::internal(error.to_string()))
}

pub(super) fn participation_status(value: i16) -> AppResult<ParticipationStatus> {
    match value {
        value if value == ParticipationStatus::Pending as i16 => Ok(ParticipationStatus::Pending),
        value if value == ParticipationStatus::Accepted as i16 => Ok(ParticipationStatus::Accepted),
        value if value == ParticipationStatus::Rejected as i16 => Ok(ParticipationStatus::Rejected),
        value if value == ParticipationStatus::Suspended as i16 => {
            Ok(ParticipationStatus::Suspended)
        }
        value if value == ParticipationStatus::Unsubmitted as i16 => {
            Ok(ParticipationStatus::Unsubmitted)
        }
        _ => Err(AppError::internal("Invalid participation status")),
    }
}

pub(super) async fn load_join_team_locked(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    team_id: i32,
    user_id: Uuid,
) -> AppResult<String> {
    let row: Option<(String, bool, bool)> = sqlx::query_as(
        r#"SELECT team.name,
                  team.deletion_pending,
                  team.captain_id = $2 OR EXISTS (
                      SELECT 1 FROM "TeamMembers" member
                       WHERE member.team_id = team.id AND member.user_id = $2
                  ) AS is_member
              FROM "Teams" team
             WHERE team.id = $1
             FOR SHARE OF team"#,
    )
    .bind(team_id)
    .bind(user_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((name, deletion_pending, is_member)) = row else {
        return Err(AppError::not_found("Team not found"));
    };
    if deletion_pending {
        return Err(AppError::conflict("Team is being deleted"));
    }
    if !is_member {
        return Err(AppError::Forbidden);
    }
    Ok(name)
}

#[derive(Debug)]
pub(super) struct LiveJoinPolicy {
    pub(super) division_id: Option<i32>,
    pub(super) target_status: ParticipationStatus,
    pub(super) member_limit: i32,
}

/// Resolve every mutable join rule after the authoritative per-game advisory
/// lock is held. Game and division editors use that same lock, so an invite,
/// review-policy, permission, or game-window change cannot be bypassed by a
/// request that began before the edit committed.
pub(super) async fn resolve_join_policy_locked(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    requested_division_id: Option<i32>,
    supplied_invite_code: Option<&str>,
) -> AppResult<LiveJoinPolicy> {
    type PolicyRow = (
        bool,
        bool,
        Option<String>,
        i32,
        bool,
        Option<i32>,
        Option<String>,
        Option<i32>,
    );
    let row: Option<PolicyRow> = sqlx::query_as(
        r#"SELECT game.practice_mode OR game.end_time_utc >= clock_timestamp() AS join_open,
                  game.accept_without_review,
                  game.invite_code,
                  game.team_member_count_limit,
                  EXISTS(SELECT 1 FROM "Divisions" candidate
                          WHERE candidate.game_id = game.id) AS has_divisions,
                  division.id,
                  division.invite_code,
                  division.default_permissions
             FROM "Games" game
             LEFT JOIN "Divisions" division
               ON division.game_id = game.id AND division.id = $2
            WHERE game.id = $1"#,
    )
    .bind(game_id)
    .bind(requested_division_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((
        join_open,
        accept_without_review,
        game_invite,
        member_limit,
        has_divisions,
        live_division_id,
        division_invite,
        division_permissions,
    )) = row
    else {
        return Err(AppError::not_found("Game not found"));
    };
    if !join_open {
        return Err(AppError::game_ended());
    }

    let (division_id, required_invite, should_accept) = if has_divisions {
        let division_id = requested_division_id
            .ok_or_else(|| AppError::bad_request("A division must be selected"))?;
        if live_division_id != Some(division_id) {
            return Err(AppError::bad_request("Invalid division"));
        }
        let permissions = GamePermission(
            division_permissions.ok_or_else(|| AppError::bad_request("Invalid division"))?,
        );
        if !permissions.contains(GamePermission::JOIN_GAME) {
            return Err(AppError::bad_request("Invalid division"));
        }
        (
            Some(division_id),
            division_invite.filter(|code| !code.is_empty()),
            !permissions.contains(GamePermission::REQUIRE_REVIEW),
        )
    } else {
        (
            None,
            game_invite.filter(|code| !code.is_empty()),
            accept_without_review,
        )
    };
    if required_invite
        .as_deref()
        .is_some_and(|required| supplied_invite_code != Some(required))
    {
        return Err(AppError::bad_request("Invalid invitation code"));
    }
    Ok(LiveJoinPolicy {
        division_id,
        target_status: if should_accept {
            ParticipationStatus::Accepted
        } else {
            ParticipationStatus::Pending
        },
        member_limit,
    })
}

pub(super) struct JoinMutation<'a> {
    pub(super) user_id: Uuid,
    pub(super) game_id: i32,
    pub(super) team_id: i32,
    pub(super) division_id: Option<i32>,
    pub(super) target_status: ParticipationStatus,
    pub(super) token: &'a str,
    pub(super) member_limit: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PersistedGameJoin {
    pub(super) participation_id: i32,
    pub(super) status: ParticipationStatus,
}

impl PersistedGameJoin {
    pub(super) fn is_accepted(self) -> bool {
        self.status == ParticipationStatus::Accepted
    }
}

/// Persist the participation and its user link in the transaction that owns the
/// ordered user/game + team advisory locks. Any conflict therefore rolls back a
/// newly-created participation instead of leaving a scoring-visible orphan.
pub(super) async fn persist_game_join_locked(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    mutation: JoinMutation<'_>,
) -> AppResult<PersistedGameJoin> {
    let current = sqlx::query_as::<_, (i32, i16)>(
        r#"SELECT membership.participation_id, participation.status
              FROM "UserParticipations" membership
              JOIN "Participations" participation
                ON participation.id = membership.participation_id
             WHERE membership.user_id = $1 AND membership.game_id = $2
             FOR UPDATE OF membership, participation"#,
    )
    .bind(mutation.user_id)
    .bind(mutation.game_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let rejected_participation_id = match current {
        Some((_, status)) if status != ParticipationStatus::Rejected as i16 => {
            return Err(AppError::bad_request("Already participating in this game"));
        }
        Some((participation_id, _)) => Some(participation_id),
        None => None,
    };

    // Also repair a legacy dangling link. A valid non-rejected link was rejected
    // above; a valid rejected one is deliberately replaced below.
    sqlx::query(
        r#"DELETE FROM "UserParticipations"
            WHERE user_id = $1 AND game_id = $2"#,
    )
    .bind(mutation.user_id)
    .bind(mutation.game_id)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let existing = sqlx::query_as::<_, (i32, i16, Option<i32>)>(
        r#"SELECT id, status, division_id
              FROM "Participations"
             WHERE game_id = $1 AND team_id = $2
             ORDER BY id
             LIMIT 1
             FOR UPDATE"#,
    )
    .bind(mutation.game_id)
    .bind(mutation.team_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let (part_id, persisted_status) = match existing {
        Some((id, status, _)) if status == ParticipationStatus::Rejected as i16 => {
            sqlx::query(
                r#"UPDATE "Participations"
                      SET division_id = $1, status = $2
                    WHERE id = $3"#,
            )
            .bind(mutation.division_id)
            .bind(mutation.target_status as i16)
            .bind(id)
            .execute(&mut **transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            (id, mutation.target_status)
        }
        Some((id, status, division_id)) => {
            if division_id != mutation.division_id {
                return Err(AppError::bad_request("Invalid division"));
            }
            (id, participation_status(status)?)
        }
        None => {
            let id = sqlx::query_scalar::<_, i32>(
                r#"INSERT INTO "Participations"
                 (status, token, writeup_id, game_id, team_id, division_id, suspicion_score)
               VALUES ($1, $2, NULL, $3, $4, $5, 0)
               RETURNING id"#,
            )
            .bind(mutation.target_status as i16)
            .bind(mutation.token)
            .bind(mutation.game_id)
            .bind(mutation.team_id)
            .bind(mutation.division_id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            (id, mutation.target_status)
        }
    };

    if mutation.member_limit > 0 {
        let member_count: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*)::bigint
                  FROM "UserParticipations"
                 WHERE participation_id = $1"#,
        )
        .bind(part_id)
        .fetch_one(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if member_count >= i64::from(mutation.member_limit) {
            return Err(AppError::bad_request(
                "The number of participants in the team exceeds the limit",
            ));
        }
    }

    let linked = sqlx::query(
        r#"INSERT INTO "UserParticipations"
             (user_id, game_id, team_id, participation_id)
           VALUES ($1, $2, $3, $4)
           ON CONFLICT (user_id, game_id) DO NOTHING"#,
    )
    .bind(mutation.user_id)
    .bind(mutation.game_id)
    .bind(mutation.team_id)
    .bind(part_id)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if linked.rows_affected() != 1 {
        return Err(AppError::bad_request("Already participating in this game"));
    }

    // Re-registration through another team used to strand the old rejected row.
    // Remove it only when this transaction removed its final member; the status
    // predicate prevents a concurrent review from turning it into an accepted
    // orphan while this request is in flight.
    if rejected_participation_id.is_some_and(|old_id| old_id != part_id) {
        sqlx::query(
            r#"DELETE FROM "Participations" participation
                WHERE participation.id = $1
                  AND participation.status = $2
                  AND NOT EXISTS (
                      SELECT 1 FROM "UserParticipations" membership
                       WHERE membership.participation_id = participation.id
                  )"#,
        )
        .bind(rejected_participation_id)
        .bind(ParticipationStatus::Rejected as i16)
        .execute(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }

    Ok(PersistedGameJoin {
        participation_id: part_id,
        status: persisted_status,
    })
}

#[cfg(test)]
#[path = "membership_tests.rs"]
mod tests;
