//! Team API-token endpoints (get/rotate/revoke) + Bearer-token resolution.

use super::*;

/// `AdTokenGenerateResultModel` — `POST Ad/Token` response (plaintext once).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdTokenGenerateResultModel {
    pub token: String,
    pub hint: String,
    #[serde(with = "crate::utils::datetime::millis")]
    pub rotated_at: DateTime<Utc>,
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
}

/// `GET /api/Game/{id}/Ad/Token` — the caller team's API-token hint (never the
/// plaintext). `exists = false` when no token has been minted.
pub async fn get_token(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<AdTokenHintModel>> {
    let part = resolve_participation(&st, &user, id).await?;
    let existing = ad_team_api_token::Entity::find()
        .filter(ad_team_api_token::Column::ParticipationId.eq(part.id))
        .one(&st.db)
        .await?;
    let model = match existing {
        None => AdTokenHintModel {
            exists: false,
            hint: String::new(),
            created_at: None,
            last_rotated_at: None,
            last_used_at: None,
            can_manage: true,
        },
        Some(t) => AdTokenHintModel {
            exists: true,
            hint: t.hint,
            created_at: Some(t.created_at_utc),
            last_rotated_at: t.last_rotated_at_utc,
            last_used_at: t.last_used_at_utc,
            can_manage: true,
        },
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
) -> AppResult<RequestResponse<AdTokenGenerateResultModel>> {
    let part = resolve_participation(&st, &user, id).await?;
    let plaintext = generate_ad_token();
    let hint = build_hint(&plaintext);
    let hash = crate::services::ad::api_token::hash(&plaintext);
    let now = Utc::now();
    let roster = super::vpn::acquire_roster_access(&st, &user, &part).await?;

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
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    roster.release().await?;

    Ok(RequestResponse::ok(AdTokenGenerateResultModel {
        token: plaintext,
        hint,
        rotated_at: now,
    }))
}

/// `DELETE /api/Game/{id}/Ad/Token` — revoke the caller team's token. Subsequent
/// Bearer-token submissions fail until a new one is minted.
pub async fn revoke_token(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<StatusCode> {
    let part = resolve_participation(&st, &user, id).await?;
    let roster = super::vpn::acquire_roster_access(&st, &user, &part).await?;
    ad_team_api_token::Entity::delete_many()
        .filter(ad_team_api_token::Column::ParticipationId.eq(part.id))
        .exec(&st.db)
        .await?;
    roster.release().await?;
    // RSCTF `AdGameController.RevokeToken` returns 204 NoContent.
    Ok(StatusCode::NO_CONTENT)
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
