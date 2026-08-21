//! One-time conversion of mutable legacy identity state into the keyed ledger.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::{
    database_error, hash_value, normalize_ip, parse_ip, prepare_identity, redacted_identity_hint,
    valid_browser_fingerprint, IDENTITY_BOOTSTRAP_LOCK_ID,
};
use crate::models::internal::configs::AppConfig;
use crate::utils::error::{AppError, AppResult};

type LegacyIdentityRow = (Uuid, Option<String>, String, Option<String>, DateTime<Utc>);

fn bootstrap_key_identifier(config: &AppConfig) -> Vec<u8> {
    hash_value(
        config.identity_hash_key.as_bytes(),
        "BootstrapKeyIdentifier",
        "v1",
    )
}

/// Serving/non-owner roles validate the migration owner's completed marker;
/// they never take the bootstrap table locks or race its one-time snapshot.
pub async fn ensure_identity_bootstrap_complete(
    pool: &sqlx::PgPool,
    config: &AppConfig,
) -> AppResult<()> {
    config
        .validate_identity_hash_key()
        .map_err(|error| AppError::internal(error.to_string()))?;
    let completed_key: Option<Vec<u8>> = sqlx::query_scalar(
        r#"SELECT key_identifier
             FROM "IdentityObservationBootstrapState"
            WHERE version = 1"#,
    )
    .fetch_optional(pool)
    .await
    .map_err(database_error)?;
    match completed_key {
        Some(key) if key == bootstrap_key_identifier(config) => Ok(()),
        Some(_) => Err(AppError::internal(
            "RSCTF_IDENTITY_HASH_KEY does not match the completed identity bootstrap",
        )),
        None => Err(AppError::internal(
            "identity observation bootstrap has not been completed by the migration owner",
        )),
    }
}

/// Atomically seed the append-only ledger from recent, activated legacy
/// accounts. Only global rows are created: legacy membership has no immutable
/// link timestamp, so fabricating game-scoped attribution could create false
/// accusations. Competitive contexts begin at the next accepted admission.
pub async fn bootstrap_legacy_identity_observations(
    pool: &sqlx::PgPool,
    config: &AppConfig,
) -> AppResult<i64> {
    config
        .validate_identity_hash_key()
        .map_err(|error| AppError::internal(error.to_string()))?;
    let mut transaction = pool.begin().await.map_err(database_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(IDENTITY_BOOTSTRAP_LOCK_ID)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    let key_identifier = bootstrap_key_identifier(config);
    let completed_key: Option<Vec<u8>> = sqlx::query_scalar(
        r#"SELECT key_identifier
             FROM "IdentityObservationBootstrapState"
            WHERE version = 1"#,
    )
    .fetch_optional(&mut *transaction)
    .await
    .map_err(database_error)?;
    if completed_key
        .as_ref()
        .is_some_and(|completed_key| *completed_key != key_identifier)
    {
        return Err(AppError::internal(
            "RSCTF_IDENTITY_HASH_KEY does not match the completed identity bootstrap",
        ));
    }

    // Freeze every legacy identity/roster writer in this fixed order. Scrubbing runs
    // on every startup, including deployments that briefly completed an older
    // pre-release v1 bootstrap before the redaction step existed.
    sqlx::query(r#"LOCK TABLE "AspNetUsers" IN SHARE MODE"#)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    sqlx::query(r#"LOCK TABLE "AntiCheatBlocks" IN SHARE MODE"#)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    sqlx::query(r#"LOCK TABLE "Logs" IN SHARE MODE"#)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    sqlx::query(r#"LOCK TABLE "TeamMembers" IN SHARE MODE"#)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    if completed_key.is_some() {
        scrub_legacy_raw_identity(&mut transaction, config).await?;
        transaction.commit().await.map_err(database_error)?;
        return Ok(0);
    }
    let rows = sqlx::query_as::<_, LegacyIdentityRow>(
        r#"SELECT id, user_name, ip, browser_fingerprint, last_signed_in_utc
             FROM "AspNetUsers"
            WHERE email_confirmed = TRUE
              AND role <> $1
              AND last_signed_in_utc > clock_timestamp() - INTERVAL '24 hours'
              AND last_signed_in_utc <= clock_timestamp() + INTERVAL '5 minutes'
              AND last_signed_in_utc >= register_time_utc
            ORDER BY id"#,
    )
    .bind(crate::utils::enums::Role::Banned as i16)
    .fetch_all(&mut *transaction)
    .await
    .map_err(database_error)?;
    let mut inserted = 0_i64;
    for (user_id, user_name, raw_ip, browser_fingerprint, observed_at) in rows {
        // A legacy account row was mutable even on some rejected paths. Only a
        // closely corresponding successful account audit makes its value safe
        // to enforce; ambiguity is skipped and established on the next login.
        let accepted_audits = sqlx::query_as::<_, (Option<String>, Option<String>)>(
            r#"SELECT remote_ip, browser_fingerprint
                 FROM "Logs"
                WHERE logger = 'AccountController'
                  AND status = 'Success'
                  AND user_name IS NOT DISTINCT FROM $1
                  AND time_utc >= $2 - INTERVAL '5 minutes'
                  AND time_utc <= $2 + INTERVAL '5 minutes'
                ORDER BY time_utc DESC, id DESC"#,
        )
        .bind(user_name.as_deref())
        .bind(observed_at)
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        let stored_ip = parse_ip(&raw_ip)
            .filter(|address| !address.is_unspecified())
            .map(normalize_ip);
        let current_ip = stored_ip.filter(|stored| {
            accepted_audits.iter().any(|(audit_ip, _)| {
                audit_ip
                    .as_deref()
                    .and_then(parse_ip)
                    .map(normalize_ip)
                    .as_deref()
                    == Some(stored.as_str())
            })
        });
        let fingerprint = browser_fingerprint
            .as_deref()
            .map(str::trim)
            .filter(|value| valid_browser_fingerprint(value))
            .filter(|value| {
                accepted_audits
                    .iter()
                    .any(|(_, audit)| audit.as_deref() == Some(*value))
            });
        let identity = prepare_identity(
            config.identity_hash_key.as_bytes(),
            current_ip.as_deref(),
            fingerprint,
        );
        for value in &identity.values {
            let result = sqlx::query(
                r#"INSERT INTO "IdentityObservations"
                     (user_id, team_id, game_id, participation_id, kind,
                      value_hash, subnet_group_hash, broad_network_hash,
                      value_hint, source, observed_at_utc)
                   VALUES ($1, NULL, NULL, NULL, $2, $3, $4, $5, $6,
                           'Legacy', $7)"#,
            )
            .bind(user_id)
            .bind(value.kind)
            .bind(&value.hash)
            .bind(&value.subnet_group_hash)
            .bind(&value.broad_network_hash)
            .bind(&value.hint)
            .bind(observed_at)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
            inserted += result.rows_affected() as i64;
        }
    }

    scrub_legacy_raw_identity(&mut transaction, config).await?;

    sqlx::query(
        r#"INSERT INTO "IdentityObservationBootstrapState"
             (version, key_identifier, completed_at_utc, observations_inserted)
           VALUES (1, $1, clock_timestamp(), $2)"#,
    )
    .bind(&key_identifier)
    .bind(inserted)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok(inserted)
}

async fn scrub_legacy_raw_identity(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    config: &AppConfig,
) -> AppResult<()> {
    redact_legacy_blocks(transaction, config).await?;
    sqlx::query(r#"UPDATE "AspNetUsers" SET browser_fingerprint = NULL WHERE browser_fingerprint IS NOT NULL"#)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
    sqlx::query(
        r#"UPDATE "Logs"
              SET browser_fingerprint = NULL,
                  message = CASE WHEN LOWER(logger) = 'fingerprint'
                                 THEN 'Legacy browser identity removed during security migration'
                                 ELSE message END
            WHERE browser_fingerprint IS NOT NULL OR LOWER(logger) = 'fingerprint'"#,
    )
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

async fn redact_legacy_blocks(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    config: &AppConfig,
) -> AppResult<()> {
    let rows = sqlx::query_as::<_, (i32, String, Option<String>, Option<Vec<u8>>)>(
        r#"SELECT id, kind, conflicting_value, conflicting_value_hash
             FROM "AntiCheatBlocks"
            WHERE conflicting_value IS NOT NULL
            ORDER BY id
            FOR UPDATE"#,
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;
    for (id, kind, raw_value, existing_hash) in rows {
        let prepared = match (kind.as_str(), raw_value.as_deref()) {
            ("Ip", Some(raw)) => prepare_identity(
                config.identity_hash_key.as_bytes(),
                parse_ip(raw).map(normalize_ip).as_deref(),
                None,
            ),
            ("Fingerprint", Some(raw)) if valid_browser_fingerprint(raw) => {
                prepare_identity(config.identity_hash_key.as_bytes(), None, Some(raw))
            }
            _ => Default::default(),
        };
        let value = prepared.values.first();
        let value_hash = value
            .map(|value| value.hash.clone())
            .or(existing_hash)
            .or_else(|| {
                raw_value.as_deref().map(|raw| {
                    hash_value(
                        config.identity_hash_key.as_bytes(),
                        "LegacyInvalidBlock",
                        &format!("{kind}\0{raw}"),
                    )
                })
            });
        let hint = value
            .map(|value| value.hint.clone())
            .unwrap_or_else(|| redacted_identity_hint(&kind, raw_value.as_deref().unwrap_or("")));
        sqlx::query(
            r#"UPDATE "AntiCheatBlocks"
                  SET conflicting_value = $2, conflicting_value_hash = $3
                WHERE id = $1"#,
        )
        .bind(id)
        .bind(hint)
        .bind(value_hash)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
    }
    Ok(())
}
