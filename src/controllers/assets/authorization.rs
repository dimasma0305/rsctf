use std::net::Ipv4Addr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::utils::enums::{ChallengeReviewStatus, GamePermission, ParticipationStatus, Role};
use crate::utils::error::{AppError, AppResult};

/// The user-independent half of attachment authorization. The short-lived
/// generation key bounds candidate-discovery staleness while collapsing
/// range-download herds to one SQL query per blob and replica. Every allowed
/// class is re-proved after storage, so this cache is never the final gate.
const ASSET_GATE_TTL: Duration = Duration::from_secs(2);

static ASSET_GATE_SINGLE_FLIGHT: std::sync::LazyLock<
    crate::utils::single_flight::SingleFlight<Option<Bytes>>,
> = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct AssetTarget {
    pub(super) game_id: i32,
    pub(super) source_team: Option<i32>,
    pub(super) challenge_id: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum AssetGate {
    Public {
        file_size: Option<u64>,
    },
    Protected {
        file_size: Option<u64>,
        targets: Vec<AssetTarget>,
    },
    Private {
        file_size: Option<u64>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AssetCachePolicy {
    Public,
    PrivateImmutable,
    PrivateNoStore,
}

impl AssetCachePolicy {
    pub(super) fn header(self) -> &'static str {
        match self {
            Self::Public => "public, max-age=31536000, immutable",
            // A static challenge attachment is already saved permanently when
            // downloaded. Private browser caching avoids repeat transfers but
            // never permits a shared intermediary to serve it.
            Self::PrivateImmutable => "private, max-age=31536000, immutable",
            // Team-generated attachments and writeups may contain secrets and
            // therefore retain the existing no-store policy.
            Self::PrivateNoStore => "private, no-store",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AuthorizedAsset {
    pub(super) cache_policy: AssetCachePolicy,
    pub(super) file_size: Option<u64>,
    /// Public branding continues through RSCTF because object-store metadata
    /// may not preserve its inline media type. Protected downloads may use the
    /// explicitly configured short-lived signed delivery path.
    pub(super) signed_delivery_allowed: bool,
    /// Exact protected scope selected during the early authorization pass.
    /// The handler must revalidate it after any storage delay and immediately
    /// before constructing a response.
    final_grant: AssetFinalGrant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AssetFinalGrant {
    None,
    Public {
        content_hash: String,
    },
    Monitor {
        user_id: Uuid,
        expected_security_stamp: String,
    },
    Protected(ProtectedAssetGrant),
}

/// Immutable attribution captured by the same decision that authorizes the
/// bytes. The logger must never reconstruct targets from a content hash: one
/// blob can legitimately be shared by several challenges or public branding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DownloadEventTarget {
    pub(super) game_id: i32,
    pub(super) team_id: i32,
    pub(super) challenge_id: i32,
    pub(super) challenge_title: String,
    pub(super) user_id: Uuid,
    pub(super) observed_at_utc: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProtectedAssetGrant {
    user_id: Uuid,
    expected_security_stamp: String,
    game_id: i32,
    team_id: i32,
    participation_id: i32,
    target: AssetTarget,
    content_hash: String,
}

fn delivery_for_gate(gate: &AssetGate) -> AuthorizedAsset {
    match gate {
        AssetGate::Public { file_size } => AuthorizedAsset {
            cache_policy: AssetCachePolicy::Public,
            file_size: *file_size,
            signed_delivery_allowed: false,
            final_grant: AssetFinalGrant::None,
        },
        AssetGate::Protected { file_size, targets } => {
            let static_only = !targets.is_empty()
                && targets
                    .iter()
                    .all(|target| target.source_team.is_none() && target.challenge_id.is_some());
            AuthorizedAsset {
                cache_policy: if static_only {
                    AssetCachePolicy::PrivateImmutable
                } else {
                    AssetCachePolicy::PrivateNoStore
                },
                file_size: *file_size,
                // Team-specific flag attachments must never become bearer-URL
                // downloads. Only immutable, game-static attachments may use
                // the explicitly enabled object-store delivery path.
                signed_delivery_allowed: static_only,
                final_grant: AssetFinalGrant::None,
            }
        }
        AssetGate::Private { file_size } => AuthorizedAsset {
            cache_policy: AssetCachePolicy::PrivateNoStore,
            file_size: *file_size,
            signed_delivery_allowed: false,
            final_grant: AssetFinalGrant::None,
        },
    }
}

#[derive(sqlx::FromRow)]
struct AssetGateRow {
    is_public: bool,
    file_size: Option<i64>,
    game_id: Option<i32>,
    source_team: Option<i32>,
    challenge_id: Option<i32>,
}

const ASSET_GATE_SQL: &str = r#"
WITH file AS (
    SELECT id, file_size, reference_count
      FROM "Files"
     WHERE hash = $1
     LIMIT 1
), meta AS (
    SELECT (
        EXISTS (SELECT 1 FROM "AspNetUsers" WHERE avatar_hash = $1)
        OR EXISTS (SELECT 1 FROM "Teams" WHERE avatar_hash = $1)
        OR EXISTS (SELECT 1 FROM "Games" WHERE poster_hash = $1)
        OR EXISTS (
            SELECT 1
              FROM "Configs"
             WHERE config_key IN ('GlobalConfig:LogoHash', 'GlobalConfig:FaviconHash')
               AND value = $1
        )
    ) AS is_public,
    (SELECT file_size FROM file) AS file_size
), targets AS (
    SELECT gc.game_id, NULL::integer AS source_team, gc.id AS challenge_id
      FROM file f
      JOIN "Attachments" a ON a.local_file_id = f.id
      JOIN "GameChallenges" gc ON gc.attachment_id = a.id
     WHERE f.reference_count > 0
    UNION
    SELECT p.game_id, p.team_id AS source_team, gi.challenge_id
      FROM file f
      JOIN "Attachments" a ON a.local_file_id = f.id
      JOIN "FlagContexts" fc ON fc.attachment_id = a.id
      JOIN "GameInstances" gi ON gi.flag_id = fc.id
      JOIN "Participations" p ON p.id = gi.participation_id
     WHERE f.reference_count > 0
    UNION
    SELECT p.game_id, p.team_id AS source_team, NULL::integer AS challenge_id
      FROM file f
      JOIN "Participations" p ON p.writeup_id = f.id
     WHERE f.reference_count > 0
)
SELECT m.is_public, m.file_size, t.game_id, t.source_team, t.challenge_id
  FROM meta m
  LEFT JOIN targets t ON TRUE
 ORDER BY t.game_id, t.source_team, t.challenge_id
"#;

fn valid_content_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn asset_gate_window() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn asset_gate_cache_key(hash: &str, window: u64) -> String {
    format!("assetgate:{hash}:{window:016x}")
}

fn decode_asset_gate(bytes: &[u8]) -> Option<AssetGate> {
    serde_json::from_slice(bytes).ok()
}

pub(super) async fn query_asset_gate(pool: &sqlx::PgPool, hash: &str) -> AppResult<AssetGate> {
    if !valid_content_hash(hash) {
        return Ok(AssetGate::Private { file_size: None });
    }

    let rows = sqlx::query_as::<_, AssetGateRow>(ASSET_GATE_SQL)
        .bind(hash)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let first = rows
        .first()
        .ok_or_else(|| AppError::internal("asset gate query returned no metadata row"))?;
    let file_size = first.file_size.and_then(|size| u64::try_from(size).ok());
    if first.is_public {
        return Ok(AssetGate::Public { file_size });
    }

    let targets = rows
        .into_iter()
        .filter_map(|row| {
            row.game_id.map(|game_id| AssetTarget {
                game_id,
                source_team: row.source_team,
                challenge_id: row.challenge_id,
            })
        })
        .collect::<Vec<_>>();
    if targets.is_empty() {
        Ok(AssetGate::Private { file_size })
    } else {
        Ok(AssetGate::Protected { file_size, targets })
    }
}

async fn cached_asset_gate(st: &SharedState, hash: &str) -> AppResult<AssetGate> {
    if !valid_content_hash(hash) {
        return Ok(AssetGate::Private { file_size: None });
    }

    let key = asset_gate_cache_key(hash, asset_gate_window());
    if let Some(bytes) = st.cache.get(&key).await {
        if let Some(gate) = decode_asset_gate(&bytes) {
            return Ok(gate);
        }
    }

    let state = st.clone();
    let fill_key = key.clone();
    let fill_hash = hash.to_string();
    let encoded = ASSET_GATE_SINGLE_FLIGHT
        .run(&key, move || async move {
            if let Some(bytes) = state.cache.get(&fill_key).await {
                if decode_asset_gate(&bytes).is_some() {
                    return Some(bytes);
                }
            }

            let gate = match query_asset_gate(state.pg(), &fill_hash).await {
                Ok(gate) => gate,
                Err(error) => {
                    tracing::warn!(hash = %fill_hash, %error, "asset authorization cache fill failed");
                    return None;
                }
            };
            let bytes = match serde_json::to_vec(&gate) {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(hash = %fill_hash, %error, "asset authorization encoding failed");
                    return None;
                }
            };
            state
                .cache
                .set(&fill_key, &bytes, Some(ASSET_GATE_TTL))
                .await;
            Some(Bytes::from(bytes))
        })
        .await
        .ok_or_else(|| AppError::internal("asset authorization cache fill failed"))?;

    decode_asset_gate(&encoded)
        .ok_or_else(|| AppError::internal("invalid cached asset authorization"))
}

#[derive(sqlx::FromRow)]
struct HistoricalParticipationTarget {
    team_id: i32,
    participation_id: i32,
}

#[derive(sqlx::FromRow)]
struct AuthorizedTargetRow {
    challenge_title: Option<String>,
    observed_at_utc: DateTime<Utc>,
    vpn_access_required: bool,
}

#[derive(Debug)]
struct ParticipantTargetAuthorization {
    grant: ProtectedAssetGrant,
    vpn_access_required: bool,
}

/// Re-check the mutable game/challenge/division half on the transaction that
/// already owns the exact caller's shared roster fence.
async fn load_authorized_target_on(
    connection: &mut sqlx::PgConnection,
    user_id: Uuid,
    game_id: i32,
    participation_id: i32,
    team_id: i32,
    target: &AssetTarget,
    content_hash: Option<&str>,
) -> AppResult<Option<AuthorizedTargetRow>> {
    sqlx::query_as::<_, AuthorizedTargetRow>(
        r#"
        SELECT CASE
                   WHEN $5::integer IS NULL THEN NULL::text
                   ELSE challenge.title
               END AS challenge_title,
               clock_timestamp() AS observed_at_utc,
               game.vpn_access_required
          FROM "Participations" participation
          JOIN "UserParticipations" historical
            ON historical.user_id = $1
           AND historical.game_id = participation.game_id
           AND historical.team_id = participation.team_id
           AND historical.participation_id = participation.id
          JOIN "Games" game
            ON game.id = participation.game_id
     LEFT JOIN "GameChallenges" challenge
            ON challenge.id = $5
           AND challenge.game_id = participation.game_id
     LEFT JOIN "Divisions" division
            ON division.id = participation.division_id
           AND division.game_id = participation.game_id
     LEFT JOIN "DivisionChallengeConfigs" permission
            ON permission.division_id = division.id
           AND permission.challenge_id = challenge.id
         WHERE participation.id = $3
           AND participation.game_id = $2
           AND participation.team_id = $4
           AND participation.status = $6
           AND game.deletion_pending = FALSE
           AND ($7::integer IS NULL OR participation.team_id = $7)
           AND (
                ($5::integer IS NULL AND $7::integer IS NOT NULL)
                OR (
                    $5::integer IS NOT NULL
                    AND challenge.id IS NOT NULL
                    AND challenge.is_enabled
                    AND challenge.deletion_pending = FALSE
                    AND challenge.review_status = $8
                )
           )
           AND (
                participation.division_id IS NULL
                OR (
                    CASE
                        WHEN $5::integer IS NULL
                            THEN COALESCE(division.default_permissions, 0)
                        ELSE COALESCE(
                            permission.permissions,
                            division.default_permissions,
                            0
                        )
                    END & $9
                ) = $9
           )
           AND (
                $10::text IS NULL
                OR (
                    $5::integer IS NOT NULL
                    AND $7::integer IS NULL
                    AND EXISTS (
                        SELECT 1
                          FROM "Files" file
                          JOIN "Attachments" attachment
                            ON attachment.local_file_id = file.id
                         WHERE file.hash = $10
                           AND file.reference_count > 0
                           AND challenge.attachment_id = attachment.id
                    )
                )
                OR (
                    $5::integer IS NOT NULL
                    AND $7::integer IS NOT NULL
                    AND EXISTS (
                        SELECT 1
                          FROM "Files" file
                          JOIN "Attachments" attachment
                            ON attachment.local_file_id = file.id
                          JOIN "FlagContexts" flag_context
                            ON flag_context.attachment_id = attachment.id
                          JOIN "GameInstances" instance
                            ON instance.flag_id = flag_context.id
                         WHERE file.hash = $10
                           AND file.reference_count > 0
                           AND instance.challenge_id = challenge.id
                           AND instance.participation_id = participation.id
                    )
                )
                OR (
                    $5::integer IS NULL
                    AND $7::integer IS NOT NULL
                    AND EXISTS (
                        SELECT 1
                          FROM "Files" file
                         WHERE file.hash = $10
                           AND file.reference_count > 0
                           AND participation.writeup_id = file.id
                    )
                )
           )
        "#,
    )
    .bind(user_id)
    .bind(game_id)
    .bind(participation_id)
    .bind(team_id)
    .bind(target.challenge_id)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(target.source_team)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(GamePermission::VIEW_CHALLENGE)
    .bind(content_hash)
    .fetch_optional(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Check one protected target while holding the same shared roster fence that
/// conflicts with kicks, leaves, captain transfers, bans, and team deletion.
/// `UserParticipations` remains historical evidence only; the canonical live
/// roster service makes the interactive authorization decision.
async fn authorize_participant_target(
    pool: &sqlx::PgPool,
    user: &CurrentUser,
    target: &AssetTarget,
    content_hash: &str,
) -> AppResult<Option<ParticipantTargetAuthorization>> {
    // The historical link identifies which team-roster key to fence. It is
    // revalidated by exact ids after the lock, so a concurrent relink can only
    // make this attempt fail closed.
    let historical = sqlx::query_as::<_, HistoricalParticipationTarget>(
        r#"
        SELECT historical.team_id, historical.participation_id
          FROM "UserParticipations" historical
          JOIN "Participations" participation
            ON participation.id = historical.participation_id
           AND participation.game_id = historical.game_id
           AND participation.team_id = historical.team_id
         WHERE historical.user_id = $1
           AND historical.game_id = $2
           AND ($3::integer IS NULL OR historical.team_id = $3)
         LIMIT 1
        "#,
    )
    .bind(user.id)
    .bind(target.game_id)
    .bind(target.source_team)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(historical) = historical else {
        return Ok(None);
    };

    let Some(mut roster) = crate::services::live_roster::try_acquire_participation_fence(
        pool,
        user.id,
        &user.security_stamp,
        target.game_id,
        historical.team_id,
        historical.participation_id,
        true,
    )
    .await?
    else {
        // A roster mutation is in progress. Never authorize from the stale
        // historical link while it is deciding the live membership.
        return Ok(None);
    };

    // Hidden games deliberately use the same rule as public games: visibility
    // controls discovery, not access for an already-enrolled player. Challenge
    // state and the effective per-division VIEW_CHALLENGE permission remain
    // authoritative. A writeup has no challenge override, so its division's
    // default permission is the effective policy.
    let row = load_authorized_target_on(
        roster.transaction_mut(),
        user.id,
        target.game_id,
        historical.participation_id,
        historical.team_id,
        target,
        None,
    )
    .await?;

    roster
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    Ok(row.map(|row| ParticipantTargetAuthorization {
        grant: ProtectedAssetGrant {
            user_id: user.id,
            expected_security_stamp: user.security_stamp.clone(),
            game_id: target.game_id,
            team_id: historical.team_id,
            participation_id: historical.participation_id,
            target: target.clone(),
            content_hash: content_hash.to_string(),
        },
        vpn_access_required: row.vpn_access_required,
    }))
}

#[cfg(test)]
pub(super) async fn participant_can_download_target(
    pool: &sqlx::PgPool,
    user: &CurrentUser,
    target: &AssetTarget,
) -> AppResult<bool> {
    Ok(
        authorize_participant_target(pool, user, target, "test-unchecked-hash")
            .await?
            .is_some(),
    )
}

#[cfg(test)]
pub(super) async fn participant_grant_for_test(
    pool: &sqlx::PgPool,
    user: &CurrentUser,
    target: &AssetTarget,
    content_hash: &str,
) -> AppResult<Option<ProtectedAssetGrant>> {
    Ok(
        authorize_participant_target(pool, user, target, content_hash)
            .await?
            .map(|authorization| authorization.grant),
    )
}

#[cfg(test)]
pub(super) async fn finalize_grant_for_test(
    pool: &sqlx::PgPool,
    grant: &ProtectedAssetGrant,
    token: Option<&str>,
    record_download: bool,
) -> AppResult<()> {
    finalize_asset_download_on(
        pool,
        &AuthorizedAsset {
            cache_policy: AssetCachePolicy::PrivateNoStore,
            file_size: None,
            signed_delivery_allowed: false,
            final_grant: AssetFinalGrant::Protected(grant.clone()),
        },
        None,
        token,
        record_download,
    )
    .await
    .map(|_| ())
}

/// Authorize a download against the cached user-independent gate. The caller's
/// live participation remains an authoritative per-request query, so caching
/// never turns a revoked membership into a valid download.
pub(super) async fn authorize_asset_download(
    st: &SharedState,
    hash: &str,
    user: &Option<CurrentUser>,
) -> AppResult<AuthorizedAsset> {
    let gate = cached_asset_gate(st, hash).await?;
    let mut authorization = delivery_for_gate(&gate);

    match gate {
        AssetGate::Public { .. } => {
            authorization.final_grant = AssetFinalGrant::Public {
                content_hash: hash.to_string(),
            };
            Ok(authorization)
        }
        AssetGate::Private { .. } => {
            if let Some(user) = user.as_ref().filter(|user| user.is_monitor()) {
                authorization.final_grant = AssetFinalGrant::Monitor {
                    user_id: user.id,
                    expected_security_stamp: user.security_stamp.clone(),
                };
                Ok(authorization)
            } else {
                Err(AppError::Forbidden)
            }
        }
        AssetGate::Protected { targets, .. } => {
            let Some(user) = user else {
                return Err(AppError::Forbidden);
            };
            if user.is_monitor() {
                authorization.final_grant = AssetFinalGrant::Monitor {
                    user_id: user.id,
                    expected_security_stamp: user.security_stamp.clone(),
                };
                return Ok(authorization);
            }
            for target in &targets {
                if let Some(target_authorization) =
                    authorize_participant_target(st.pg(), user, target, hash).await?
                {
                    if target_authorization.vpn_access_required {
                        // A signed object-store URL is a bearer capability and
                        // would remain usable away from the event tunnel.
                        authorization.signed_delivery_allowed = false;
                    }
                    authorization.final_grant =
                        AssetFinalGrant::Protected(target_authorization.grant);
                    return Ok(authorization);
                }
            }
            Err(AppError::Forbidden)
        }
    }
}

fn download_event_lock_key(target: &DownloadEventTarget) -> String {
    format!(
        "asset-download-event:{}:{}:{}",
        target.game_id, target.team_id, target.challenge_id
    )
}

async fn insert_download_event_on(
    connection: &mut sqlx::PgConnection,
    target: &DownloadEventTarget,
    token: Option<&str>,
) -> AppResult<Option<i32>> {
    let challenge_id = target.challenge_id.to_string();
    let event_id = sqlx::query_scalar(
        r#"INSERT INTO "GameEvents"
               (game_id, "Type", "values", publish_time_utc, user_id, team_id)
           SELECT $1, $2, jsonb_build_array($3::text, $4::text, $5::text),
                  $6, $7, $8
            WHERE NOT EXISTS (
                  SELECT 1
                    FROM "GameEvents" existing
                   WHERE existing.game_id = $1
                     AND existing.team_id = $8
                     AND existing."Type" = $2
                     AND existing."values" ->> 0 = $3
            )
           RETURNING id"#,
    )
    .bind(target.game_id)
    .bind(crate::utils::enums::EventType::Download as i16)
    .bind(challenge_id)
    .bind(&target.challenge_title)
    .bind(token.unwrap_or(""))
    .bind(target.observed_at_utc)
    .bind(target.user_id)
    .bind(target.team_id)
    .fetch_optional(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(event_id)
}

const PUBLIC_ASSET_FINAL_SQL: &str = r#"
WITH public_user AS MATERIALIZED (
    SELECT id FROM "AspNetUsers" WHERE avatar_hash = $1 FOR SHARE
), public_team AS MATERIALIZED (
    SELECT id FROM "Teams" WHERE avatar_hash = $1 FOR SHARE
), public_game AS MATERIALIZED (
    SELECT id FROM "Games" WHERE poster_hash = $1 FOR SHARE
), public_config AS MATERIALIZED (
    SELECT config_key
      FROM "Configs"
     WHERE config_key IN ('GlobalConfig:LogoHash', 'GlobalConfig:FaviconHash')
       AND value = $1
     FOR SHARE
)
SELECT EXISTS (SELECT 1 FROM public_user)
    OR EXISTS (SELECT 1 FROM public_team)
    OR EXISTS (SELECT 1 FROM public_game)
    OR EXISTS (SELECT 1 FROM public_config)
"#;

async fn finalize_public_asset(pool: &sqlx::PgPool, content_hash: &str) -> AppResult<()> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let still_public = sqlx::query_scalar::<_, bool>(PUBLIC_ASSET_FINAL_SQL)
        .bind(content_hash)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if !still_public {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Err(AppError::Forbidden);
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

async fn finalize_monitor_asset(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    expected_security_stamp: &str,
) -> AppResult<()> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let still_monitor = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS (
               SELECT 1
                 FROM "AspNetUsers" account
                WHERE account.id = $1
                  AND account.security_stamp = $2
                  AND account.role IN ($3, $4)
                FOR SHARE OF account
           )"#,
    )
    .bind(user_id)
    .bind(expected_security_stamp)
    .bind(Role::Monitor as i16)
    .bind(Role::Admin as i16)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !still_monitor {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Err(AppError::Forbidden);
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

#[cfg(test)]
pub(super) async fn finalize_public_grant_for_test(
    pool: &sqlx::PgPool,
    content_hash: &str,
) -> AppResult<()> {
    finalize_public_asset(pool, content_hash).await
}

#[cfg(test)]
pub(super) async fn finalize_monitor_grant_for_test(
    pool: &sqlx::PgPool,
    user: &CurrentUser,
) -> AppResult<()> {
    finalize_monitor_asset(pool, user.id, &user.security_stamp).await
}

/// Finalize a prepared protected response. Storage work deliberately happens
/// before this call. It reacquires the exact stamped roster fence, rechecks the
/// mutable challenge/division policy, and commits the precisely attributed
/// Download event before releasing the fence. The response body is streamed
/// only after this transaction has committed, so no database guard is retained
/// for the network lifetime.
#[derive(Default)]
struct FinalizedAssetDownload {
    vpn_gate_active: bool,
    event_id: Option<i32>,
}

async fn finalize_asset_download_on(
    pool: &sqlx::PgPool,
    authorization: &AuthorizedAsset,
    source: Option<Ipv4Addr>,
    token: Option<&str>,
    record_download: bool,
) -> AppResult<FinalizedAssetDownload> {
    let grant = match &authorization.final_grant {
        AssetFinalGrant::None => return Ok(FinalizedAssetDownload::default()),
        AssetFinalGrant::Public { content_hash } => {
            finalize_public_asset(pool, content_hash).await?;
            return Ok(FinalizedAssetDownload::default());
        }
        AssetFinalGrant::Monitor {
            user_id,
            expected_security_stamp,
        } => {
            finalize_monitor_asset(pool, *user_id, expected_security_stamp).await?;
            return Ok(FinalizedAssetDownload::default());
        }
        AssetFinalGrant::Protected(grant) => grant,
    };
    let Some(mut roster) = crate::services::live_roster::try_acquire_participation_fence(
        pool,
        grant.user_id,
        &grant.expected_security_stamp,
        grant.game_id,
        grant.team_id,
        grant.participation_id,
        true,
    )
    .await?
    else {
        return Err(AppError::Forbidden);
    };

    let vpn_gate_active = crate::services::event_security::require_event_vpn_source_on(
        roster.transaction_mut().as_mut(),
        grant.game_id,
        grant.user_id,
        grant.participation_id,
        source,
    )
    .await?;

    let row = load_authorized_target_on(
        roster.transaction_mut(),
        grant.user_id,
        grant.game_id,
        grant.participation_id,
        grant.team_id,
        &grant.target,
        Some(&grant.content_hash),
    )
    .await?;
    let Some(row) = row else {
        roster
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Err(AppError::Forbidden);
    };

    let mut event_id = None;
    if record_download {
        if let Some((challenge_id, challenge_title)) =
            grant.target.challenge_id.zip(row.challenge_title)
        {
            let event = DownloadEventTarget {
                game_id: grant.game_id,
                team_id: grant.team_id,
                challenge_id,
                challenge_title,
                user_id: grant.user_id,
                observed_at_utc: row.observed_at_utc,
            };
            roster
                .acquire_additional(&download_event_lock_key(&event))
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            event_id = insert_download_event_on(roster.transaction_mut(), &event, token).await?;
        }
    }

    roster
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(FinalizedAssetDownload {
        vpn_gate_active,
        event_id,
    })
}

pub(super) async fn finalize_asset_download(
    st: &SharedState,
    authorization: &AuthorizedAsset,
    source: Option<Ipv4Addr>,
    token: Option<&str>,
    record_download: bool,
) -> AppResult<bool> {
    let outcome =
        finalize_asset_download_on(st.pg(), authorization, source, token, record_download).await?;
    if let Some(event_id) = outcome.event_id {
        if let Err(error) =
            crate::services::game_event_feed::publish_committed(st, &[event_id]).await
        {
            tracing::warn!(event_id, %error, "asset download event publish failed");
        }
    }
    Ok(outcome.vpn_gate_active)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_attachments_are_privately_cacheable_but_team_files_are_not() {
        let static_gate = AssetGate::Protected {
            file_size: Some(42),
            targets: vec![AssetTarget {
                game_id: 1,
                source_team: None,
                challenge_id: Some(2),
            }],
        };
        let sensitive_gate = AssetGate::Protected {
            file_size: Some(42),
            targets: vec![AssetTarget {
                game_id: 1,
                source_team: Some(3),
                challenge_id: Some(2),
            }],
        };

        let static_delivery = delivery_for_gate(&static_gate);
        assert_eq!(
            static_delivery.cache_policy,
            AssetCachePolicy::PrivateImmutable
        );
        assert!(static_delivery.signed_delivery_allowed);

        let sensitive_delivery = delivery_for_gate(&sensitive_gate);
        assert_eq!(
            sensitive_delivery.cache_policy,
            AssetCachePolicy::PrivateNoStore
        );
        assert!(!sensitive_delivery.signed_delivery_allowed);

        let private_delivery = delivery_for_gate(&AssetGate::Private {
            file_size: Some(42),
        });
        assert_eq!(
            private_delivery.cache_policy,
            AssetCachePolicy::PrivateNoStore
        );
        assert!(!private_delivery.signed_delivery_allowed);

        let public_delivery = delivery_for_gate(&AssetGate::Public {
            file_size: Some(42),
        });
        assert!(
            matches!(public_delivery.final_grant, AssetFinalGrant::None),
            "public downloads must never become participant anti-cheat evidence"
        );
    }

    #[test]
    fn malformed_hashes_never_enter_the_shared_cache_namespace() {
        assert!(valid_content_hash(&"a".repeat(64)));
        assert!(!valid_content_hash("../secrets"));
        assert!(!valid_content_hash(&"g".repeat(64)));
    }

    #[test]
    fn public_finalization_reproves_every_public_hash_relation() {
        for relation in [
            r#""AspNetUsers" WHERE avatar_hash = $1"#,
            r#""Teams" WHERE avatar_hash = $1"#,
            r#""Games" WHERE poster_hash = $1"#,
            "GlobalConfig:LogoHash",
            "GlobalConfig:FaviconHash",
        ] {
            assert!(
                PUBLIC_ASSET_FINAL_SQL.contains(relation),
                "missing public relation: {relation}"
            );
        }
        assert!(PUBLIC_ASSET_FINAL_SQL.matches("FOR SHARE").count() >= 4);
    }
}
