//! Credential delivery handlers split from `users.rs`.

use super::*;
use futures::{stream, StreamExt};

const MAX_CREDENTIAL_SEND_ITEMS: usize = 100;
const CREDENTIAL_SEND_CONCURRENCY: usize = 8;

/// Email remains the lookup key so the public request shape does not change,
/// but the cached value binds the plaintext to the immutable account id. A
/// legacy plaintext-only value deliberately fails deserialization.
pub(super) const CRED_CACHE_PREFIX: &str = "credimport:";
pub(super) const CRED_CACHE_TTL_SECS: u64 = 60 * 60;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CachedImportCredential {
    pub user_id: Uuid,
    pub user_name: String,
    pub security_stamp: String,
    pub password: String,
}

pub(super) fn credential_cache_key(email: &str) -> String {
    format!("{CRED_CACHE_PREFIX}{}", email.trim().to_uppercase())
}

pub(super) struct CachedImportCredentialPublication {
    key: String,
    value: Vec<u8>,
}

/// Publish only from the still-open identity transaction that generated this
/// stamp and password. The caller must retain the registration/row fences
/// until the subsequent database commit completes.
pub(super) async fn cache_import_credential(
    cache: &dyn crate::services::cache::Cache,
    user_id: Uuid,
    email: &str,
    user_name: &str,
    security_stamp: &str,
    password: &str,
) -> AppResult<CachedImportCredentialPublication> {
    let key = credential_cache_key(email);
    let value = serde_json::to_vec(&CachedImportCredential {
        user_id,
        user_name: user_name.to_string(),
        security_stamp: security_stamp.to_string(),
        password: password.to_string(),
    })
    .map_err(|error| AppError::internal(error.to_string()))?;
    cache
        .set(
            &key,
            &value,
            Some(std::time::Duration::from_secs(CRED_CACHE_TTL_SECS)),
        )
        .await;
    Ok(CachedImportCredentialPublication { key, value })
}

/// Remove a credential published for a database transaction whose commit
/// failed. The comparison cannot delete a newer import that already replaced
/// the same email key.
pub(super) async fn rollback_import_credential_publication(
    cache: &dyn crate::services::cache::Cache,
    publication: &CachedImportCredentialPublication,
) {
    cache
        .compare_and_remove(&publication.key, &publication.value)
        .await;
}

/// Remove only the credential that belongs to `user_id`. The comparison keeps
/// an overlapping re-import from deleting a newer credential for this email.
pub(super) async fn invalidate_import_credential(
    cache: &dyn crate::services::cache::Cache,
    user_id: Uuid,
    email: &str,
) {
    let key = credential_cache_key(email);
    let Some(value) = cache.get(&key).await else {
        return;
    };
    let belongs_to_user = serde_json::from_slice::<CachedImportCredential>(&value)
        .is_ok_and(|credential| credential.user_id == user_id);
    if belongs_to_user {
        cache.compare_and_remove(&key, &value).await;
    }
}

async fn lock_credential_recipient(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    credential: &CachedImportCredential,
    normalized_email: &str,
) -> AppResult<Option<String>> {
    sqlx::query_scalar(
        r#"SELECT COALESCE(user_name, '')
             FROM "AspNetUsers"
            WHERE id = $1
              AND normalized_email = $2
              AND security_stamp = $3
              AND role NOT IN ($4, $5)
            FOR UPDATE"#,
    )
    .bind(credential.user_id)
    .bind(normalized_email)
    .bind(&credential.security_stamp)
    .bind(Role::Admin as i16)
    .bind(Role::Banned as i16)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Restore a failed delivery reservation only while the same account identity
/// is still eligible. `set_if_absent` prevents a concurrent re-import from
/// being overwritten by the older plaintext. The row lock is intentionally
/// held only for this bounded cache operation, never for SMTP network I/O.
async fn restore_reserved_credential(
    pool: &sqlx::PgPool,
    cache: &dyn crate::services::cache::Cache,
    cache_key: &str,
    cached_value: &[u8],
    credential: &CachedImportCredential,
    normalized_email: &str,
) -> AppResult<bool> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let still_eligible = lock_credential_recipient(&mut transaction, credential, normalized_email)
        .await?
        .is_some();
    let restored = if still_eligible {
        cache
            .set_if_absent(
                cache_key,
                cached_value,
                Some(std::time::Duration::from_secs(CRED_CACHE_TTL_SECS)),
            )
            .await
    } else {
        false
    };
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(restored)
}

/// Await the bounded SMTP attempt while retaining the recipient's `FOR UPDATE`
/// fence. Returning the transaction makes the success/rollback boundary
/// explicit: no email, role, password-stamp, or deletion mutation can race the
/// delivery after the recipient identity has been validated.
async fn deliver_while_recipient_locked<'c, F>(
    transaction: sqlx::Transaction<'c, sqlx::Postgres>,
    delivery: F,
) -> (sqlx::Transaction<'c, sqlx::Postgres>, AppResult<()>)
where
    F: std::future::Future<Output = AppResult<()>>,
{
    let result = delivery.await;
    (transaction, result)
}

/// Prevent browsers and intermediaries from retaining a response that contains
/// a freshly generated account credential.
pub(super) fn private_no_store(body: impl IntoResponse) -> Response {
    (
        [
            (header::CACHE_CONTROL, "private, no-store"),
            (header::PRAGMA, "no-cache"),
        ],
        body,
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialSendItem {
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub user_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialSendRequest {
    #[serde(default)]
    pub items: Vec<CredentialSendItem>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialSendResult {
    pub email: String,
    pub user_name: String,
    pub sent: bool,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailSendResult {
    pub sent: usize,
    pub failed: usize,
    pub results: Vec<CredentialSendResult>,
}

fn failed_credential_send(
    email: String,
    user_name: String,
    error: impl Into<String>,
) -> CredentialSendResult {
    CredentialSendResult {
        email,
        user_name,
        sent: false,
        error: Some(error.into()),
    }
}

async fn send_one_credential(
    st: &SharedState,
    sender: &crate::services::mail::MailSender,
    platform: &str,
    item: CredentialSendItem,
) -> AppResult<CredentialSendResult> {
    let email = item.email.trim().to_lowercase();
    if !sender.is_configured() {
        return Ok(failed_credential_send(
            email,
            item.user_name,
            "no SMTP configured - nothing sent (dry run)",
        ));
    }

    let normalized_email = email.to_uppercase();
    let cache_key = credential_cache_key(&email);
    let Some(cached_value) = st.cache.get(&cache_key).await else {
        return Ok(failed_credential_send(
            email,
            item.user_name,
            "credentials expired or not cached - reset the user's password to re-issue",
        ));
    };
    let Ok(credential) = serde_json::from_slice::<CachedImportCredential>(&cached_value) else {
        // Old plaintext-only entries cannot be associated with an immutable
        // account identity, so consume them instead of guessing by email.
        st.cache.compare_and_remove(&cache_key, &cached_value).await;
        return Ok(failed_credential_send(
            email,
            item.user_name,
            "credentials expired or not cached - reset the user's password to re-issue",
        ));
    };

    let mut transaction = st
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(live_user_name) =
        lock_credential_recipient(&mut transaction, &credential, &normalized_email).await?
    else {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        st.cache.compare_and_remove(&cache_key, &cached_value).await;
        return Ok(failed_credential_send(
            email,
            credential.user_name,
            "cached credentials no longer match an eligible user",
        ));
    };
    // Reserve the value atomically in authoritative L2 after the row lock.
    // Unlike an L1 `get`, this cannot be stale on another replica, so only one
    // concurrent delivery can proceed.
    if !st.cache.compare_and_remove(&cache_key, &cached_value).await {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(failed_credential_send(
            email,
            live_user_name,
            "credentials expired or not cached - reset the user's password to re-issue",
        ));
    }
    let subject = format!("Your {platform} account credentials");
    let body = format!(
        "<p>Hello,</p>\
         <p>An account has been created for you on <b>{platform}</b>.</p>\
         <p><b>Username:</b> {user}<br/><b>Password:</b> {pass}</p>\
         <p>Please sign in and change your password.</p>",
        user = html_escape(&live_user_name),
        pass = html_escape(&credential.password),
    );

    // MailSender bounds this await to 15 seconds. Keeping the row lock for that
    // finite window prevents an admin email reassignment (or reset/delete) from
    // making the just-validated plaintext valid for a different live identity.
    // At most CREDENTIAL_SEND_CONCURRENCY distinct user rows are fenced; no
    // global lock or database connection is monopolized indefinitely.
    let (transaction, delivery) =
        deliver_while_recipient_locked(transaction, sender.send_required(&email, &subject, &body))
            .await;

    match delivery {
        Ok(()) => {
            transaction
                .commit()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            Ok(CredentialSendResult {
                email,
                user_name: live_user_name,
                sent: true,
                error: None,
            })
        }
        Err(error) => {
            // Release the delivery fence before acquiring a fresh validation
            // transaction in `restore_reserved_credential`.
            transaction
                .rollback()
                .await
                .map_err(|rollback_error| AppError::internal(rollback_error.to_string()))?;
            // Delivery did not occur. Restore only if this exact account is
            // still live and no newer import credential won the cache key.
            // Failure is safe: an admin can explicitly re-issue, while stale
            // plaintext never returns.
            let _ = restore_reserved_credential(
                st.pg(),
                st.cache.as_ref(),
                &cache_key,
                &cached_value,
                &credential,
                &normalized_email,
            )
            .await;
            Ok(failed_credential_send(
                email,
                live_user_name,
                format!("delivery failed: {error}"),
            ))
        }
    }
}

/// Email cached import credentials without ever resetting the stored password.
pub async fn send_credentials(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Json(req): Json<CredentialSendRequest>,
) -> AppResult<Response> {
    if req.items.len() > MAX_CREDENTIAL_SEND_ITEMS {
        return Err(AppError::bad_request(format!(
            "at most {MAX_CREDENTIAL_SEND_ITEMS} credentials can be sent per request"
        )));
    }
    let sender = crate::services::mail::MailSender::from_env();
    let platform = st.config.global.title.as_str();
    let results = stream::iter(
        req.items
            .into_iter()
            .map(|item| send_one_credential(&st, &sender, platform, item)),
    )
    .buffered(CREDENTIAL_SEND_CONCURRENCY)
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .collect::<AppResult<Vec<_>>>()?;
    let sent = results.iter().filter(|result| result.sent).count();
    let failed = results.len().saturating_sub(sent);

    Ok(private_no_store(Json(EmailSendResult {
        sent,
        failed,
        results,
    })))
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::cache::{Cache, InMemoryCache};
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;

    #[test]
    fn cached_credentials_bind_the_account_and_reject_legacy_plaintext() {
        let user_id = Uuid::new_v4();
        let value = serde_json::to_vec(&CachedImportCredential {
            user_id,
            user_name: "alice".to_string(),
            security_stamp: "stamp-at-import".to_string(),
            password: "temporary-secret".to_string(),
        })
        .unwrap();
        let decoded: CachedImportCredential = serde_json::from_slice(&value).unwrap();
        assert_eq!(decoded.user_id, user_id);
        assert_eq!(decoded.user_name, "alice");
        assert_eq!(decoded.security_stamp, "stamp-at-import");
        assert_eq!(decoded.password, "temporary-secret");
        assert!(serde_json::from_slice::<CachedImportCredential>(b"temporary-secret").is_err());
    }

    #[tokio::test]
    async fn invalidation_cannot_remove_another_accounts_cached_credential() {
        let cache = InMemoryCache::new();
        let owner = Uuid::new_v4();
        let other_user = Uuid::new_v4();
        let email = "owner@example.test";
        let key = credential_cache_key(email);
        let value = serde_json::to_vec(&CachedImportCredential {
            user_id: owner,
            user_name: "owner".to_string(),
            security_stamp: "stamp-at-import".to_string(),
            password: "temporary-secret".to_string(),
        })
        .unwrap();
        cache.set(&key, &value, None).await;

        invalidate_import_credential(&cache, other_user, email).await;
        assert_eq!(cache.get(&key).await.as_deref(), Some(value.as_slice()));

        invalidate_import_credential(&cache, owner, email).await;
        assert!(cache.get(&key).await.is_none());
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn credential_delivery_fences_email_mutation_until_delivery_finishes() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_credential_delivery_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin_pool)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            r#"CREATE TABLE "AspNetUsers" (
                 id UUID PRIMARY KEY,
                 user_name TEXT,
                 normalized_email TEXT UNIQUE,
                 security_stamp TEXT,
                 role SMALLINT NOT NULL
               )"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let user_id = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "AspNetUsers"
                 (id, user_name, normalized_email, security_stamp, role)
               VALUES ($1, 'alice', 'OLD@EXAMPLE.TEST', 'delivery-stamp', $2)"#,
        )
        .bind(user_id)
        .bind(Role::User as i16)
        .execute(&pool)
        .await
        .unwrap();
        let credential = CachedImportCredential {
            user_id,
            user_name: "alice".to_string(),
            security_stamp: "delivery-stamp".to_string(),
            password: "temporary-secret".to_string(),
        };

        let mut transaction = pool.begin().await.unwrap();
        assert_eq!(
            lock_credential_recipient(&mut transaction, &credential, "OLD@EXAMPLE.TEST")
                .await
                .unwrap()
                .as_deref(),
            Some("alice")
        );

        let (release_delivery, delivery_released) = tokio::sync::oneshot::channel();
        let delivery = tokio::spawn(async move {
            let simulated_smtp = async move {
                delivery_released.await.unwrap();
                Ok(())
            };
            let (transaction, result) =
                deliver_while_recipient_locked(transaction, simulated_smtp).await;
            result.unwrap();
            transaction.commit().await.unwrap();
        });

        let mutation_pool = pool.clone();
        let (mutation_started, mutation_start) = tokio::sync::oneshot::channel();
        let mut mutation = tokio::spawn(async move {
            mutation_started.send(()).unwrap();
            sqlx::query(
                r#"UPDATE "AspNetUsers"
                      SET normalized_email = 'NEW@EXAMPLE.TEST'
                    WHERE id = $1"#,
            )
            .bind(user_id)
            .execute(&mutation_pool)
            .await
            .unwrap();
        });
        mutation_start.await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut mutation)
                .await
                .is_err(),
            "email mutation completed while credential delivery still held the row fence"
        );

        release_delivery.send(()).unwrap();
        delivery.await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), mutation)
            .await
            .expect("email mutation stayed blocked after delivery completed")
            .unwrap();
        let normalized_email: String =
            sqlx::query_scalar(r#"SELECT normalized_email FROM "AspNetUsers" WHERE id = $1"#)
                .bind(user_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(normalized_email, "NEW@EXAMPLE.TEST");

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin_pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn cached_identity_cannot_survive_a_stamp_rotation_or_reassigned_email() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_credential_cache_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin_pool)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            r#"CREATE TABLE "AspNetUsers" (
                 id UUID PRIMARY KEY,
                 user_name TEXT,
                 normalized_email TEXT UNIQUE,
                 security_stamp TEXT,
                 role SMALLINT NOT NULL
               )"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let original_user = Uuid::new_v4();
        let replacement_user = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "AspNetUsers"
                 (id, user_name, normalized_email, security_stamp, role)
               VALUES ($1, 'alice', 'TEAM@EXAMPLE.TEST', 'stamp-at-import', $3),
                      ($2, 'bob', 'BOB@EXAMPLE.TEST', 'replacement-stamp', $3)"#,
        )
        .bind(original_user)
        .bind(replacement_user)
        .bind(Role::User as i16)
        .execute(&pool)
        .await
        .unwrap();
        let cached = CachedImportCredential {
            user_id: original_user,
            user_name: "alice".to_string(),
            security_stamp: "stamp-at-import".to_string(),
            password: "temporary-secret".to_string(),
        };

        let mut transaction = pool.begin().await.unwrap();
        assert_eq!(
            lock_credential_recipient(&mut transaction, &cached, "TEAM@EXAMPLE.TEST")
                .await
                .unwrap()
                .as_deref(),
            Some("alice")
        );
        transaction.rollback().await.unwrap();

        let cache = InMemoryCache::new();
        let cache_key = credential_cache_key("team@example.test");
        let cached_value = serde_json::to_vec(&cached).unwrap();
        cache.set(&cache_key, b"newer-import", None).await;
        assert!(
            !restore_reserved_credential(
                &pool,
                &cache,
                &cache_key,
                &cached_value,
                &cached,
                "TEAM@EXAMPLE.TEST",
            )
            .await
            .unwrap(),
            "a failed delivery overwrote a newer cached credential"
        );
        assert_eq!(
            cache.get(&cache_key).await.as_deref(),
            Some(b"newer-import".as_slice())
        );
        cache.remove(&cache_key).await;
        assert!(
            restore_reserved_credential(
                &pool,
                &cache,
                &cache_key,
                &cached_value,
                &cached,
                "TEAM@EXAMPLE.TEST",
            )
            .await
            .unwrap(),
            "an unchanged eligible account could not restore a failed delivery"
        );
        assert_eq!(
            cache.get(&cache_key).await.as_deref(),
            Some(cached_value.as_slice())
        );

        sqlx::query(
            r#"UPDATE "AspNetUsers" SET security_stamp = 'post-reset-stamp' WHERE id = $1"#,
        )
        .bind(original_user)
        .execute(&pool)
        .await
        .unwrap();
        let mut transaction = pool.begin().await.unwrap();
        assert!(
            lock_credential_recipient(&mut transaction, &cached, "TEAM@EXAMPLE.TEST")
                .await
                .unwrap()
                .is_none(),
            "a cached pre-reset password survived security-stamp rotation"
        );
        transaction.rollback().await.unwrap();
        cache.remove(&cache_key).await;
        assert!(
            !restore_reserved_credential(
                &pool,
                &cache,
                &cache_key,
                &cached_value,
                &cached,
                "TEAM@EXAMPLE.TEST",
            )
            .await
            .unwrap(),
            "a pre-reset plaintext was restored after stamp rotation"
        );
        assert!(cache.get(&cache_key).await.is_none());

        let post_reset_cached = CachedImportCredential {
            user_id: original_user,
            user_name: "alice".to_string(),
            security_stamp: "post-reset-stamp".to_string(),
            password: "new-temporary-secret".to_string(),
        };
        sqlx::query(
            r#"UPDATE "AspNetUsers" SET normalized_email = 'ALICE@EXAMPLE.TEST' WHERE id = $1"#,
        )
        .bind(original_user)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"UPDATE "AspNetUsers" SET normalized_email = 'TEAM@EXAMPLE.TEST' WHERE id = $1"#,
        )
        .bind(replacement_user)
        .execute(&pool)
        .await
        .unwrap();

        let mut transaction = pool.begin().await.unwrap();
        assert!(
            lock_credential_recipient(&mut transaction, &post_reset_cached, "TEAM@EXAMPLE.TEST",)
                .await
                .unwrap()
                .is_none(),
            "a cached credential followed an email to a different user id"
        );
        transaction.rollback().await.unwrap();

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin_pool)
            .await
            .unwrap();
    }
}
