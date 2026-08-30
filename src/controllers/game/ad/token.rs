//! Team API-token endpoints (get/rotate/revoke) + Bearer-token resolution.

use super::*;
use axum::response::Response;

const ROTATE_TOKEN_BINDING: &[u8] = b"rotate-token";
const REVOKE_TOKEN_BINDING: &[u8] = b"revoke-token";

/// `AdTokenGenerateResultModel` — `POST Ad/Token` response (plaintext once).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdTokenGenerateResultModel {
    pub token: String,
    pub hint: String,
    pub participation_id: i32,
    pub team_id: i32,
    pub operation_id: uuid::Uuid,
    pub revision: i64,
    #[serde(with = "crate::utils::datetime::millis")]
    pub rotated_at: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub recovery_expires_at: DateTime<Utc>,
}

/// `AdTokenHintModel` — GET `Ad/Token` response (hint only).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdTokenHintModel {
    pub exists: bool,
    pub hint: String,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub last_rotated_at: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub last_used_at: Option<DateTime<Utc>>,
    pub can_manage: bool,
    pub revision: i64,
    pub participation_id: i32,
    pub team_id: i32,
}

/// `GET /api/Game/{id}/Ad/Token` — the caller team's API-token hint (never the
/// plaintext). `exists = false` when no token has been minted.
pub async fn get_token(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<AdTokenHintModel>> {
    let part = resolve_participation(&st, &user, id).await?;
    // Metadata and its fence must come from one PostgreSQL statement snapshot.
    // Otherwise an old hint can be paired with a newly committed revision and
    // authorize an overwrite of a credential the caller never observed.
    let row: (
        bool,
        String,
        Option<DateTime<Utc>>,
        Option<DateTime<Utc>>,
        Option<DateTime<Utc>>,
        i64,
    ) = sqlx::query_as(
        r#"SELECT token.participation_id IS NOT NULL AS credential_exists,
                  COALESCE(token.hint, '') AS hint,
                  token.created_at_utc,
                  token.last_rotated_at_utc,
                  token.last_used_at_utc,
                  COALESCE(revision.revision,
                           CASE WHEN token.participation_id IS NULL THEN 0 ELSE 1 END
                  )::BIGINT AS revision
             FROM (SELECT $1::INTEGER AS participation_id) scope
             LEFT JOIN "AdTeamApiTokens" token
               ON token.participation_id = scope.participation_id
             LEFT JOIN "PlayerCredentialRevisions" revision
               ON revision.participation_id = scope.participation_id
              AND revision.credential_kind = 'AdToken'
              AND revision.challenge_id = 0"#,
    )
    .bind(part.id)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let model = AdTokenHintModel {
        exists: row.0,
        hint: row.1,
        created_at: row.2,
        last_rotated_at: row.3,
        last_used_at: row.4,
        can_manage: true,
        revision: row.5,
        participation_id: part.id,
        team_id: part.team_id,
    };
    Ok(RequestResponse::ok(model))
}

/// `POST /api/Game/{id}/Ad/Token` — mint + rotate the caller team's submit token.
/// A fresh random `ad_...` plaintext is returned exactly once; only its SHA256
/// hash (plus a short hint) is persisted, upserted onto the participation's row.
pub async fn rotate_token(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    crate::controllers::game::credential_operations::CredentialMutationInput(request): crate::controllers::game::credential_operations::CredentialMutationInput,
) -> AppResult<Response> {
    let part = resolve_participation(&st, &user, id).await?;
    let mut roster = super::vpn::acquire_roster_access(&st, &user, &part).await?;
    let scope = crate::controllers::game::credential_operations::CredentialScope {
        participation_id: part.id,
        game_id: id,
        challenge_id: 0,
        actor_user_id: user.id,
        kind: crate::controllers::game::credential_operations::CredentialKind::AdToken,
    };
    let reservation: crate::controllers::game::credential_operations::CredentialReservation<
        AdTokenGenerateResultModel,
    > = crate::controllers::game::credential_operations::reserve(
        &st,
        roster.transaction_mut(),
        scope,
        request,
        ROTATE_TOKEN_BINDING,
    )
    .await?;
    let operation = match reservation {
        crate::controllers::game::credential_operations::CredentialReservation::Recovered(
            result,
        ) => {
            let expected_hash = crate::services::ad::api_token::hash(&result.token);
            let is_current: bool = sqlx::query_scalar(
                r#"SELECT EXISTS (
                     SELECT 1 FROM "AdTeamApiTokens"
                      WHERE participation_id = $1 AND token_hash = $2
                   )"#,
            )
            .bind(part.id)
            .bind(expected_hash)
            .fetch_one(&mut **roster.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            if !is_current {
                return Err(AppError::conflict(
                    "credential operation no longer names the active token",
                ));
            }
            roster.release().await?;
            return Ok(
                crate::controllers::game::credential_operations::private_credential_response(
                    result,
                ),
            );
        }
        crate::controllers::game::credential_operations::CredentialReservation::Fresh(
            operation,
        ) => operation,
    };
    let plaintext = generate_ad_token();
    let hint = build_hint(&plaintext);
    let hash = crate::services::ad::api_token::hash(&plaintext);
    let now = Utc::now();

    sqlx::query(
        r#"INSERT INTO "AdTeamApiTokens"
             (participation_id, token_hash, hint, created_at_utc,
              last_rotated_at_utc, last_used_at_utc)
           VALUES ($1, $2, $3, $4, $4, NULL)
           ON CONFLICT (participation_id) DO UPDATE SET
             token_hash = EXCLUDED.token_hash,
             hint = EXCLUDED.hint,
             last_rotated_at_utc = EXCLUDED.last_rotated_at_utc,
             last_used_at_utc = NULL"#,
    )
    .bind(part.id)
    .bind(hash)
    .bind(&hint)
    .bind(now)
    .execute(&mut **roster.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let result = AdTokenGenerateResultModel {
        token: plaintext,
        hint,
        participation_id: part.id,
        team_id: part.team_id,
        operation_id: operation.operation_id,
        revision: operation.result_revision,
        rotated_at: now,
        recovery_expires_at: operation.recovery_expires_at,
    };
    crate::controllers::game::credential_operations::complete(
        &st,
        roster.transaction_mut(),
        scope,
        operation,
        &result,
    )
    .await?;
    roster.release().await?;

    Ok(crate::controllers::game::credential_operations::private_credential_response(result))
}

/// `DELETE /api/Game/{id}/Ad/Token` — revoke the caller team's token. Subsequent
/// Bearer-token submissions fail until a new one is minted.
pub async fn revoke_token(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    crate::controllers::game::credential_operations::CredentialMutationInput(request): crate::controllers::game::credential_operations::CredentialMutationInput,
) -> AppResult<Response> {
    let part = resolve_participation(&st, &user, id).await?;
    let mut roster = super::vpn::acquire_roster_access(&st, &user, &part).await?;
    let scope = crate::controllers::game::credential_operations::CredentialScope {
        participation_id: part.id,
        game_id: id,
        challenge_id: 0,
        actor_user_id: user.id,
        kind: crate::controllers::game::credential_operations::CredentialKind::AdToken,
    };
    let reservation: crate::controllers::game::credential_operations::CredentialReservation<
        crate::controllers::game::credential_operations::CredentialMutationAck,
    > = crate::controllers::game::credential_operations::reserve(
        &st,
        roster.transaction_mut(),
        scope,
        request,
        REVOKE_TOKEN_BINDING,
    )
    .await?;
    let operation = match reservation {
        crate::controllers::game::credential_operations::CredentialReservation::Recovered(
            result,
        ) => {
            roster.release().await?;
            return Ok(
                crate::controllers::game::credential_operations::private_credential_response(
                    result,
                ),
            );
        }
        crate::controllers::game::credential_operations::CredentialReservation::Fresh(
            operation,
        ) => operation,
    };
    sqlx::query(r#"DELETE FROM "AdTeamApiTokens" WHERE participation_id = $1"#)
        .bind(part.id)
        .execute(&mut **roster.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let result = crate::controllers::game::credential_operations::CredentialMutationAck {
        operation_id: operation.operation_id,
        revision: operation.result_revision,
        recovery_expires_at: operation.recovery_expires_at,
    };
    crate::controllers::game::credential_operations::complete(
        &st,
        roster.transaction_mut(),
        scope,
        operation,
        &result,
    )
    .await?;
    roster.release().await?;
    Ok(crate::controllers::game::credential_operations::private_credential_response(result))
}

/// Resolve a participation from an `Authorization: Bearer ad_...` header. Hashes
/// the presented token, looks up the `ad_team_api_token` row by hash, and checks
/// the participation is accepted in this game — the port of RSCTF's
/// `ResolveTeamApiTokenAsync`. Stamps `last_used_at_utc` (throttled to 30s so a
/// tight polling loop doesn't hammer one hot row).
pub async fn resolve_team_api_token(
    st: &SharedState,
    headers: &HeaderMap,
    verified: Option<&crate::services::ad::api_token::VerifiedTeamToken>,
    game_id: i32,
) -> AppResult<Option<participation::Model>> {
    let loaded;
    let credential = match verified {
        Some(credential) => credential,
        None => {
            let Some(presented) = crate::services::ad::api_token::bearer_token(headers) else {
                return Ok(None);
            };
            loaded = crate::services::ad::api_token::authenticate(st.pg(), presented).await?;
            let Some(credential) = loaded.as_ref() else {
                return Ok(None);
            };
            credential
        }
    };
    Ok((credential.participation.game_id == game_id).then(|| credential.participation.clone()))
}

/// Mint a fresh plaintext token: `ad_` + unpadded base64url of 32 random bytes
/// (RSCTF `AdTokenUtils.GeneratePlaintext`).
fn generate_ad_token() -> String {
    let mut raw = [0u8; 32];
    fill_random(&mut raw);
    format!(
        "{}{}",
        crate::services::ad::api_token::PREFIX,
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
    )
}

/// Short public hint (RSCTF `AdTokenUtils.BuildHint`): first 7 chars + `…` + last 4.
fn build_hint(plaintext: &str) -> String {
    if plaintext.len() < 12 {
        return plaintext.to_string();
    }
    format!("{}…{}", &plaintext[..7], &plaintext[plaintext.len() - 4..])
}
