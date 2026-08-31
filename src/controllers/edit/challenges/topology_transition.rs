//! Durable ownership for ordinary edits that must drain live runtimes first.

use super::*;

pub(super) struct TopologyTransition {
    pub final_enabled: bool,
    pub resuming: bool,
}

pub(super) async fn resolve(
    connection: &mut sqlx::PgConnection,
    challenge_id: i32,
    actor_id: Uuid,
    operation_id: Uuid,
    request_fingerprint: [u8; 32],
    expected_revision: i64,
    requested: bool,
    requested_final_enabled: bool,
) -> AppResult<Option<TopologyTransition>> {
    let existing: Option<(Uuid, Uuid, Vec<u8>, i64, bool)> = sqlx::query_as(
        r#"SELECT actor_id, operation_id, request_fingerprint,
                  expected_revision, restore_enabled
             FROM "ChallengeDefinitionTransitions"
            WHERE challenge_id = $1
            FOR UPDATE"#,
    )
    .bind(challenge_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(database_error)?;
    let Some((stored_actor, stored_operation, stored_fingerprint, stored_revision, restore)) =
        existing
    else {
        return Ok(requested.then_some(TopologyTransition {
            final_enabled: requested_final_enabled,
            resuming: false,
        }));
    };
    if stored_fingerprint.as_slice() != request_fingerprint.as_slice()
        || stored_revision != expected_revision
    {
        return Err(AppError::conflict(
            "Another challenge topology transition must finish before this edit",
        ));
    }
    // A reload may lose the in-memory operation UUID. Let an authorized editor
    // adopt only the byte-identical request/revision; a different intent stays
    // fenced while the disabled transition remains recoverable.
    if stored_actor != actor_id || stored_operation != operation_id {
        sqlx::query(
            r#"UPDATE "ChallengeDefinitionTransitions"
                  SET actor_id = $2, operation_id = $3
                WHERE challenge_id = $1 AND actor_id = $4 AND operation_id = $5"#,
        )
        .bind(challenge_id)
        .bind(actor_id)
        .bind(operation_id)
        .bind(stored_actor)
        .bind(stored_operation)
        .execute(&mut *connection)
        .await
        .map_err(database_error)?;
    }
    Ok(Some(TopologyTransition {
        final_enabled: restore,
        resuming: true,
    }))
}

pub(super) async fn begin(
    connection: &mut sqlx::PgConnection,
    challenge_id: i32,
    game_id: i32,
    actor_id: Uuid,
    operation_id: Uuid,
    request_fingerprint: [u8; 32],
    expected_revision: i64,
    transition: &TopologyTransition,
) -> AppResult<()> {
    if transition.resuming {
        return Ok(());
    }
    sqlx::query(
        r#"INSERT INTO "ChallengeDefinitionTransitions"
             (challenge_id, actor_id, operation_id, request_fingerprint,
              expected_revision, restore_enabled)
           VALUES ($1,$2,$3,$4,$5,$6)"#,
    )
    .bind(challenge_id)
    .bind(actor_id)
    .bind(operation_id)
    .bind(request_fingerprint.as_slice())
    .bind(expected_revision)
    .bind(transition.final_enabled)
    .execute(&mut *connection)
    .await
    .map_err(database_error)?;
    let fenced = sqlx::query(
        r#"UPDATE "GameChallenges"
              SET is_enabled = FALSE
            WHERE id = $1 AND game_id = $2
              AND revision = $3
              AND is_enabled = TRUE
              AND deletion_pending = FALSE"#,
    )
    .bind(challenge_id)
    .bind(game_id)
    .bind(expected_revision)
    .execute(&mut *connection)
    .await
    .map_err(database_error)?;
    if fenced.rows_affected() != 1 {
        return Err(AppError::conflict(
            "Challenge eligibility changed; retry the topology update",
        ));
    }
    Ok(())
}

pub(super) async fn complete(
    connection: &mut sqlx::PgConnection,
    challenge_id: i32,
    actor_id: Uuid,
    operation_id: Uuid,
) -> AppResult<()> {
    let deleted = sqlx::query(
        r#"DELETE FROM "ChallengeDefinitionTransitions"
            WHERE challenge_id = $1 AND actor_id = $2 AND operation_id = $3"#,
    )
    .bind(challenge_id)
    .bind(actor_id)
    .bind(operation_id)
    .execute(connection)
    .await
    .map_err(database_error)?;
    if deleted.rows_affected() != 1 {
        return Err(AppError::conflict(
            "Challenge topology transition ownership changed",
        ));
    }
    Ok(())
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn transition_and_disable_share_one_transaction_connection() {
        let source = include_str!("topology_transition.rs");
        let transition = source
            .find("INSERT INTO \"ChallengeDefinitionTransitions\"")
            .unwrap();
        let disable = source.find("SET is_enabled = FALSE").unwrap();
        assert!(transition < disable);
        assert!(source.contains("stored_fingerprint.as_slice() != request_fingerprint.as_slice()"));
        assert!(source.contains("stored_revision != expected_revision"));
    }
}
