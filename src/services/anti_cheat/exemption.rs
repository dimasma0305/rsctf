//! Scoped, expiring adjudication of identity-policy false positives.

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use super::{
    database_error, hash_value, lock_identity_bootstrap_shared, lock_identity_values, normalize_ip,
    parse_ip, valid_browser_fingerprint, IdentityValue, PreparedIdentity, EXEMPTION_TTL_DAYS,
};
use crate::models::internal::configs::AppConfig;
use crate::utils::error::{AppError, AppResult};

#[derive(Debug, Clone, Copy)]
pub struct ExemptionGrant {
    pub expires_at_utc: DateTime<Utc>,
}

pub(super) fn canonical_pair(left: Uuid, right: Uuid) -> (Uuid, Uuid) {
    if left.as_bytes() < right.as_bytes() {
        (left, right)
    } else {
        (right, left)
    }
}

/// Retain a block as audit evidence and grant a seven-day exemption scoped to
/// exactly the same account pair, identity kind, and hashed value.
pub async fn exempt_block(
    pool: &sqlx::PgPool,
    config: &AppConfig,
    block_id: i32,
    adjudicator_id: Uuid,
) -> AppResult<ExemptionGrant> {
    let mut transaction = pool.begin().await.map_err(database_error)?;
    // Bootstrap takes its exclusive advisory before table/tuple locks. Join
    // that canonical order before locking the audit row, otherwise bootstrap's
    // table SHARE lock and this later UPDATE's ROW EXCLUSIVE lock can cycle.
    lock_identity_bootstrap_shared(&mut transaction).await?;
    let row = sqlx::query_as::<_, (Uuid, Option<Uuid>, String, Option<String>, Option<Vec<u8>>)>(
        r#"SELECT user_id, conflict_user_id, kind, conflicting_value,
                  conflicting_value_hash
             FROM "AntiCheatBlocks"
            WHERE id = $1
            FOR UPDATE"#,
    )
    .bind(block_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(|| AppError::not_found("Anti-cheat block not found"))?;
    let conflict_user_id = row
        .1
        .ok_or_else(|| AppError::bad_request("This block has no conflicting account"))?;
    if !matches!(row.2.as_str(), "Ip" | "Fingerprint") {
        return Err(AppError::bad_request(
            "This block has an invalid identity kind",
        ));
    }
    let value_hash = match row.4 {
        Some(hash) if hash.len() == 32 => hash,
        _ => {
            let raw = row
                .3
                .as_deref()
                .ok_or_else(|| AppError::bad_request("This legacy block has no identity value"))?;
            let normalized = if row.2 == "Ip" {
                parse_ip(raw)
                    .map(normalize_ip)
                    .ok_or_else(|| AppError::bad_request("This legacy block has an invalid IP"))?
            } else {
                if !valid_browser_fingerprint(raw) {
                    return Err(AppError::bad_request(
                        "This legacy block has no recoverable fingerprint",
                    ));
                }
                raw.to_string()
            };
            hash_value(config.identity_hash_key.as_bytes(), &row.2, &normalized)
        }
    };
    let prepared = PreparedIdentity {
        values: vec![IdentityValue {
            kind: if row.2 == "Ip" { "Ip" } else { "Fingerprint" },
            hash: value_hash.clone(),
            subnet_group_hash: None,
            broad_network_hash: None,
            hint: String::new(),
        }],
        ..Default::default()
    };
    lock_identity_values(&mut transaction, &prepared).await?;
    let now = super::database_now(&mut transaction).await?;
    let expires_at = now + Duration::days(EXEMPTION_TTL_DAYS);
    let (user_a, user_b) = canonical_pair(row.0, conflict_user_id);
    sqlx::query(
        r#"INSERT INTO "AntiCheatExemptions"
             (user_a, user_b, kind, value_hash, created_from_block_id,
              created_by_user_id, created_at_utc, expires_at_utc, revoked_at_utc)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NULL)
           "#,
    )
    .bind(user_a)
    .bind(user_b)
    .bind(&row.2)
    .bind(&value_hash)
    .bind(block_id)
    .bind(adjudicator_id)
    .bind(now)
    .bind(expires_at)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    sqlx::query(
        r#"UPDATE "AntiCheatBlocks"
              SET conflicting_value_hash = $2,
                  adjudicated_at_utc = $3,
                  adjudicated_by_user_id = $4,
                  exemption_expires_at_utc = $5
            WHERE id = $1"#,
    )
    .bind(block_id)
    .bind(&value_hash)
    .bind(now)
    .bind(adjudicator_id)
    .bind(expires_at)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok(ExemptionGrant {
        expires_at_utc: expires_at,
    })
}
