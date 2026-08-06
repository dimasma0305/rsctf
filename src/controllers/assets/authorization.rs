use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::utils::enums::{ChallengeReviewStatus, ParticipationStatus};
use crate::utils::error::{AppError, AppResult};

/// The user-independent half of attachment authorization. The one-second
/// generation key bounds relationship staleness to the same interval as live
/// account authorization while collapsing range-download herds to one SQL
/// query per blob and replica.
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
}

fn delivery_for_gate(gate: &AssetGate) -> AuthorizedAsset {
    match gate {
        AssetGate::Public { file_size } => AuthorizedAsset {
            cache_policy: AssetCachePolicy::Public,
            file_size: *file_size,
            signed_delivery_allowed: false,
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
            }
        }
        AssetGate::Private { file_size } => AuthorizedAsset {
            cache_policy: AssetCachePolicy::PrivateNoStore,
            file_size: *file_size,
            signed_delivery_allowed: false,
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

/// Check one protected target in a single query. Hidden games deliberately use
/// the same rule as public games: an accepted participant who can open the
/// challenge can download its attachment. Game visibility controls discovery,
/// not access for already-enrolled players.
pub(super) async fn participant_can_download_target(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    target: &AssetTarget,
) -> AppResult<bool> {
    sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS (
            SELECT 1
              FROM "UserParticipations" up
              JOIN "Participations" p ON p.id = up.participation_id
             WHERE up.user_id = $1
               AND up.game_id = $2
               AND p.game_id = $2
               AND p.team_id = up.team_id
               AND p.status = $3
               AND ($4::integer IS NULL OR p.team_id = $4)
               AND (
                    $5::integer IS NULL
                    OR EXISTS (
                        SELECT 1
                          FROM "GameChallenges" c
                         WHERE c.id = $5
                           AND c.game_id = $2
                           AND c.is_enabled
                           AND c.review_status = $6
                    )
               )
        )
        "#,
    )
    .bind(user_id)
    .bind(target.game_id)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(target.source_team)
    .bind(target.challenge_id)
    .bind(ChallengeReviewStatus::Active as i16)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
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
    let authorization = delivery_for_gate(&gate);

    match gate {
        AssetGate::Public { .. } => Ok(authorization),
        AssetGate::Private { .. } => {
            if user.as_ref().is_some_and(CurrentUser::is_monitor) {
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
                return Ok(authorization);
            }
            for target in &targets {
                if participant_can_download_target(st.pg(), user.id, target).await? {
                    return Ok(authorization);
                }
            }
            Err(AppError::Forbidden)
        }
    }
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
    }

    #[test]
    fn malformed_hashes_never_enter_the_shared_cache_namespace() {
        assert!(valid_content_hash(&"a".repeat(64)));
        assert!(!valid_content_hash("../secrets"));
        assert!(!valid_content_hash(&"g".repeat(64)));
    }
}
