use super::*;

/// Load one challenge through the caller's retained domain transaction.
/// PostgreSQL stores the SeaORM enums as small integers, so the JSON projection
/// names them explicitly before Serde reconstructs the model.
pub(crate) async fn load_challenge_locked(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<game_challenge::Model> {
    load_challenge_rows_locked(connection, game_id, Some(challenge_id))
        .await?
        .pop()
        .ok_or_else(|| AppError::not_found("Challenge not found"))
}

async fn load_challenge_rows_locked(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: Option<i32>,
) -> AppResult<Vec<game_challenge::Model>> {
    let rows = sqlx::query_scalar::<_, serde_json::Value>(
        r#"SELECT (to_jsonb(challenge) - 'Type') || jsonb_build_object(
                    'type', CASE challenge."Type"
                        WHEN 0 THEN 'StaticAttachment'
                        WHEN 1 THEN 'StaticContainer'
                        WHEN 2 THEN 'DynamicAttachment'
                        WHEN 3 THEN 'DynamicContainer'
                        WHEN 4 THEN 'AttackDefense'
                        WHEN 5 THEN 'KingOfTheHill'
                        ELSE concat('__invalid_', challenge."Type")
                    END,
                    'category', CASE challenge.category
                        WHEN 0 THEN 'Misc'
                        WHEN 1 THEN 'Crypto'
                        WHEN 2 THEN 'Pwn'
                        WHEN 3 THEN 'Web'
                        WHEN 4 THEN 'Reverse'
                        WHEN 5 THEN 'Blockchain'
                        WHEN 6 THEN 'Forensics'
                        WHEN 7 THEN 'Hardware'
                        WHEN 8 THEN 'Mobile'
                        WHEN 9 THEN 'PPC'
                        WHEN 10 THEN 'AI'
                        WHEN 11 THEN 'Pentest'
                        WHEN 12 THEN 'OSINT'
                        ELSE concat('__invalid_', challenge.category)
                    END,
                    'review_status', CASE challenge.review_status
                        WHEN 0 THEN 'Active'
                        WHEN 1 THEN 'Pending'
                        WHEN 2 THEN 'Rejected'
                        ELSE concat('__invalid_', challenge.review_status)
                    END,
                    'build_status', CASE challenge.build_status
                        WHEN 0 THEN 'None'
                        WHEN 1 THEN 'Success'
                        WHEN 2 THEN 'Failed'
                        WHEN 3 THEN 'Building'
                        WHEN 4 THEN 'NotApplicable'
                        WHEN 5 THEN 'Queued'
                        WHEN 6 THEN 'MissingDockerfile'
                        ELSE concat('__invalid_', challenge.build_status)
                    END,
                    'score_curve', CASE challenge.score_curve
                        WHEN 0 THEN 'Standard'
                        WHEN 1 THEN 'Linear'
                        WHEN 2 THEN 'Logarithmic'
                        ELSE concat('__invalid_', challenge.score_curve)
                    END,
                    'network_mode', CASE
                        WHEN challenge.network_mode IS NULL THEN NULL
                        WHEN challenge.network_mode = 0 THEN 'Open'
                        WHEN challenge.network_mode = 32 THEN 'Isolated'
                        WHEN challenge.network_mode = 255 THEN 'Custom'
                        ELSE concat('__invalid_', challenge.network_mode)
                    END,
                    'variant_mode', CASE challenge.variant_mode
                        WHEN 0 THEN 'Disabled'
                        WHEN 1 THEN 'PerParticipation'
                        ELSE concat('__invalid_', challenge.variant_mode)
                    END,
                    'variant_generator_build_status',
                        CASE challenge.variant_generator_build_status
                            WHEN 0 THEN 'None'
                            WHEN 1 THEN 'Success'
                            WHEN 2 THEN 'Failed'
                            WHEN 3 THEN 'Building'
                            WHEN 4 THEN 'NotApplicable'
                            WHEN 5 THEN 'Queued'
                            WHEN 6 THEN 'MissingDockerfile'
                            ELSE concat(
                                '__invalid_',
                                challenge.variant_generator_build_status
                            )
                        END,
                    'solve_receipt_mode', CASE challenge.solve_receipt_mode
                        WHEN 0 THEN 'Disabled'
                        WHEN 1 THEN 'Optional'
                        WHEN 2 THEN 'Required'
                        ELSE concat('__invalid_', challenge.solve_receipt_mode)
                    END
                )
             FROM "GameChallenges" challenge
            WHERE challenge.game_id = $1
              AND ($2::integer IS NULL OR challenge.id = $2)
            ORDER BY challenge.id"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .fetch_all(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    rows.into_iter()
        .map(|row| {
            serde_json::from_value(row).map_err(|error| {
                AppError::internal(format!("could not decode challenge row: {error}"))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn projection_covers_every_non_string_seaorm_enum() {
        let source = include_str!("challenge_rows.rs");
        for field in [
            "challenge.\"Type\"",
            "challenge.category",
            "challenge.review_status",
            "challenge.build_status",
            "challenge.score_curve",
            "challenge.network_mode",
            "challenge.variant_mode",
            "challenge.variant_generator_build_status",
            "challenge.solve_receipt_mode",
        ] {
            assert!(
                source.contains(field),
                "missing JSON enum projection for {field}"
            );
        }
    }
}
