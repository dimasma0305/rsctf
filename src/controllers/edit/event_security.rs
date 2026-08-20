//! Organizer controls for deterministic challenge variants.

use axum::extract::{Path, State};
use serde::Serialize;

use super::*;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VariantGenerationResult {
    pub generated: usize,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct VariantSummary {
    pub challenge_id: i32,
    pub participation_id: i32,
    pub revision: i32,
    pub generator_image: String,
    pub generator_digest: String,
    pub artifact_hash: String,
    #[serde(with = "crate::utils::datetime::millis")]
    pub frozen_at_utc: DateTime<Utc>,
}

pub async fn generate_variants(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
) -> AppResult<RequestResponse<VariantGenerationResult>> {
    super::manager_or_admin(&st, &user, game_id).await?;
    let generated = crate::services::event_security::generate_event_variants(&st, game_id).await?;
    Ok(RequestResponse::ok(VariantGenerationResult { generated }))
}

pub async fn list_variants(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
) -> AppResult<RequestResponse<Vec<VariantSummary>>> {
    super::manager_or_admin(&st, &user, game_id).await?;
    let rows = sqlx::query_as::<_, (i32, i32, i32, String, String, Vec<u8>, DateTime<Utc>)>(
        r#"SELECT challenge_id, participation_id, revision, generator_image,
                  generator_digest, artifact_hash, frozen_at_utc
             FROM "ChallengeVariants"
            WHERE game_id = $1 AND frozen_at_utc IS NOT NULL
            ORDER BY challenge_id, participation_id"#,
    )
    .bind(game_id)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(RequestResponse::ok(
        rows.into_iter()
            .map(
                |(
                    challenge_id,
                    participation_id,
                    revision,
                    generator_image,
                    generator_digest,
                    artifact_hash,
                    frozen_at_utc,
                )| VariantSummary {
                    challenge_id,
                    participation_id,
                    revision,
                    generator_image,
                    generator_digest,
                    artifact_hash: hex::encode(artifact_hash),
                    frozen_at_utc,
                },
            )
            .collect(),
    ))
}
