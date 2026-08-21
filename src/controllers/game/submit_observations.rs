use super::*;

/// Acquire the same cross-replica team-roster fence as membership mutations,
/// then lock and revalidate the exact interactive caller on the grading
/// transaction. Cached participation context remains an early optimization;
/// these rows are the authorization decision that survives through commit.
pub(super) async fn lock_submit_caller_at_grade(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    expected_security_stamp: &str,
    game_id: i32,
    team_id: i32,
    participation_id: i32,
) -> AppResult<bool> {
    let roster_key = crate::services::live_roster::lock_key(team_id);
    crate::utils::single_flight::acquire_transaction_advisory_lock_shared(transaction, &roster_key)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    let exact_link: Option<Uuid> = sqlx::query_scalar(
        r#"SELECT membership.user_id
              FROM "UserParticipations" membership
              JOIN "Participations" participation
                ON participation.id = membership.participation_id
               AND participation.game_id = membership.game_id
               AND participation.team_id = membership.team_id
              JOIN "Teams" team ON team.id = participation.team_id
              JOIN "AspNetUsers" account ON account.id = membership.user_id
             WHERE membership.user_id = $1
               AND membership.game_id = $2
               AND membership.team_id = $3
               AND membership.participation_id = $4
             FOR SHARE OF membership, participation, team, account"#,
    )
    .bind(user_id)
    .bind(game_id)
    .bind(team_id)
    .bind(participation_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if exact_link.is_none() {
        return Ok(false);
    }
    crate::services::live_roster::participation_caller_is_live_on(
        &mut **transaction,
        user_id,
        expected_security_stamp,
        game_id,
        team_id,
        participation_id,
        true,
    )
    .await
}

type LockedGameTiming = (
    DateTime<Utc>,
    DateTime<Utc>,
    bool,
    Option<DateTime<Utc>>,
    DateTime<Utc>,
);
type GameTimingRow = (DateTime<Utc>, DateTime<Utc>, bool, Option<DateTime<Utc>>);

/// Lock the live game policy before assigning the submission's canonical
/// observation time. A final reconciliation pass takes the conflicting row
/// lock, so a request queued behind that barrier cannot retain a stale pre-end
/// timestamp after the final scan has completed.
pub(super) async fn lock_game_timing_at_grade(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
) -> AppResult<Option<LockedGameTiming>> {
    let timing: Option<GameTimingRow> = sqlx::query_as(
        r#"SELECT start_time_utc, end_time_utc, practice_mode, freeze_time_utc
                 FROM "Games" WHERE id = $1 FOR SHARE"#,
    )
    .bind(game_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((start, end, practice_mode, freeze)) = timing else {
        return Ok(None);
    };
    // Read the durable phase in a fresh statement after obtaining Games FOR
    // SHARE. A finalizer that committed while this request waited is therefore
    // visible even if PostgreSQL's wall clock subsequently moves backward.
    let competitive_evidence_open: bool = sqlx::query_scalar(
        r#"SELECT NOT EXISTS (
             SELECT 1 FROM "SuspicionReconciliationState"
              WHERE game_id = $1 AND evidence_closed_at_utc IS NOT NULL
           )"#,
    )
    .bind(game_id)
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let mut observed_at: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if !competitive_evidence_open && observed_at < end {
        // Exact end is outside every anti-cheat query's strict [start,end)
        // interval but still permits an explicitly configured practice submit.
        observed_at = end;
    }
    Ok(Some((start, end, practice_mode, freeze, observed_at)))
}

/// Snapshot only positive interaction telemetry already committed when the
/// grade is assigned. A later/earlier log cannot reinterpret this submission;
/// missing best-effort telemetry stays a safe false negative.
#[allow(clippy::too_many_arguments)]
pub(super) async fn load_first_positive_interactions(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    team_id: i32,
    challenge_id: i32,
    game_start: DateTime<Utc>,
    game_end: DateTime<Utc>,
    submit_time: DateTime<Utc>,
) -> AppResult<(
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
)> {
    sqlx::query_as(
        r#"SELECT
             MIN(publish_time_utc) FILTER (WHERE "Type" = $7),
             MIN(publish_time_utc) FILTER (WHERE "Type" = $8),
             MIN(publish_time_utc) FILTER (WHERE "Type" = $9)
           FROM "GameEvents"
          WHERE game_id = $1
            AND team_id = $2
            AND "values" ->> 0 = $3
            AND publish_time_utc >= $4
            AND publish_time_utc < $5
            AND publish_time_utc <= $6
            AND "Type" IN ($7, $8, $9)"#,
    )
    .bind(game_id)
    .bind(team_id)
    .bind(challenge_id.to_string())
    .bind(game_start)
    .bind(game_end)
    .bind(submit_time)
    .bind(EventType::ChallengeOpened as i16)
    .bind(EventType::Download as i16)
    .bind(EventType::ContainerStart as i16)
    .fetch_one(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}
