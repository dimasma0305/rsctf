//! Authentication for administrator-managed personal API tokens.
//!
//! This credential domain is deliberately separate from browser JWTs, A&D
//! team tokens, KotH capabilities, and worker credentials. Only the exact
//! versioned grammar below reaches PostgreSQL, and a managed credential is
//! never reinterpreted as another authority after it has been rejected.

use std::sync::LazyLock;
use std::time::Duration;

use axum::http::Method;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::utils::enums::Role;
use crate::utils::error::{AppError, AppResult};
use crate::utils::single_flight::SingleFlight;

pub const PREFIX: &str = "rsctf_pat_v1_";
pub const FAMILY_PREFIX: &str = "rsctf_pat_";
pub const AUDIENCE: &str = "rsctf-api";
pub const READ_SCOPE: &str = "api:read";
pub const WRITE_SCOPE: &str = "api:write";
pub const VERSION: i16 = 1;
const ENCODED_SECRET_LEN: usize = 43;
const LAST_USED_WRITE_INTERVAL: chrono::Duration = chrono::Duration::seconds(30);
const NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(5);
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(1);

const AUTHENTICATE_SQL: &str = r#"
SELECT token.id,
       account.id,
       account.role,
       COALESCE(account.user_name, ''),
       account.security_stamp,
       token.owner_security_stamp_digest,
       token.scopes,
       token.last_used_at
  FROM "ApiTokens" token
  JOIN "AspNetUsers" account ON account.id = token.creator_id
 WHERE token.token_hash = $1
   AND token.token_version = $2
   AND token.audience = $3
   AND NOT token.is_revoked
   AND (token.expires_at IS NULL OR token.expires_at > clock_timestamp())
   AND account.role = $4
   AND account.security_stamp IS NOT NULL
   AND account.security_stamp <> ''
 LIMIT 1
"#;

const TOUCH_LAST_USED_SQL: &str = r#"
UPDATE "ApiTokens"
   SET last_used_at = clock_timestamp()
 WHERE id = $1
   AND token_hash = $2
   AND NOT is_revoked
   AND (expires_at IS NULL OR expires_at > clock_timestamp())
   AND (last_used_at IS NULL OR last_used_at < clock_timestamp() - interval '30 seconds')
"#;

type AuthenticatedRow = (
    Uuid,
    Uuid,
    i16,
    String,
    Option<String>,
    Option<Vec<u8>>,
    Vec<String>,
    Option<DateTime<Utc>>,
);

#[derive(Clone, Debug)]
pub struct VerifiedManagedApiToken {
    pub user: CurrentUser,
    pub token_id: Uuid,
    pub partition_key: String,
    scopes: Vec<String>,
}

/// Terminal decision marker for this credential family. Extractors must never
/// retry it as a session JWT or another automation credential.
#[derive(Clone, Copy, Debug)]
pub struct RejectedManagedApiToken;

impl VerifiedManagedApiToken {
    pub fn permits(&self, method: &Method) -> bool {
        let required = if matches!(method, &Method::GET | &Method::HEAD | &Method::OPTIONS) {
            READ_SCOPE
        } else {
            WRITE_SCOPE
        };
        self.scopes.iter().any(|scope| scope == required)
    }
}

#[derive(Clone, Default)]
enum LookupResult {
    Found(VerifiedManagedApiToken),
    Missing,
    #[default]
    Failed,
}

static AUTHENTICATION_FLIGHT: LazyLock<SingleFlight<LookupResult>> =
    LazyLock::new(SingleFlight::new);

pub fn looks_managed(token: &str) -> bool {
    token.starts_with(FAMILY_PREFIX)
}

/// Exact public shape emitted by token creation. The bound makes malformed and
/// oversized bearer input cheap to reject before hashing or database work.
pub fn is_well_formed(token: &str) -> bool {
    let Some(secret) = token.strip_prefix(PREFIX) else {
        return false;
    };
    secret.len() == ENCODED_SECRET_LEN
        && secret
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub fn hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

pub fn owner_stamp_digest(stamp: &str) -> Vec<u8> {
    let mut digest = Sha256::new();
    digest.update(b"rsctf-managed-token-owner-v1\0");
    digest.update(stamp.as_bytes());
    digest.finalize().to_vec()
}

pub fn negative_cache_key(token_hash: &str) -> String {
    format!("_ManagedApiTokenNegative_v1_{token_hash}")
}

fn scopes_are_valid(scopes: &[String]) -> bool {
    !scopes.is_empty()
        && scopes.len() <= 2
        && scopes
            .iter()
            .all(|scope| matches!(scope.as_str(), READ_SCOPE | WRITE_SCOPE))
        && !(scopes.len() == 2 && scopes[0] == scopes[1])
}

async fn lookup(st: SharedState, token_hash: String) -> LookupResult {
    let row = match sqlx::query_as::<_, AuthenticatedRow>(AUTHENTICATE_SQL)
        .bind(&token_hash)
        .bind(VERSION)
        .bind(AUDIENCE)
        .bind(Role::Admin as i16)
        .fetch_optional(st.pg())
        .await
    {
        Ok(row) => row,
        Err(error) => {
            tracing::warn!(%error, "managed API token lookup failed");
            return LookupResult::Failed;
        }
    };

    let Some((token_id, owner_id, role, name, stamp, stored_digest, scopes, last_used_at)) = row
    else {
        st.cache
            .set(
                &negative_cache_key(&token_hash),
                b"missing",
                Some(NEGATIVE_CACHE_TTL),
            )
            .await;
        return LookupResult::Missing;
    };
    let Some(stamp) = stamp else {
        return LookupResult::Missing;
    };
    if stamp.is_empty() {
        return LookupResult::Missing;
    }
    let expected_digest = owner_stamp_digest(&stamp);
    if stored_digest.as_deref() != Some(expected_digest.as_slice())
        || role != Role::Admin as i16
        || !scopes_are_valid(&scopes)
    {
        return LookupResult::Missing;
    }

    if last_used_at.is_none_or(|last_used| Utc::now() - last_used >= LAST_USED_WRITE_INTERVAL) {
        if let Err(error) = sqlx::query(TOUCH_LAST_USED_SQL)
            .bind(token_id)
            .bind(&token_hash)
            .execute(st.pg())
            .await
        {
            tracing::warn!(token_id = %token_id, %error, "managed API token usage update failed");
            return LookupResult::Failed;
        }
    }

    LookupResult::Found(VerifiedManagedApiToken {
        user: CurrentUser {
            id: owner_id,
            role: Role::Admin,
            name,
            security_stamp: stamp,
        },
        token_id,
        partition_key: format!("managed:{token_hash}"),
        scopes,
    })
}

/// Resolve one exact managed bearer. Missing lookups are cached briefly in the
/// shared bounded cache, while same-digest misses and valid bursts are
/// single-flighted per replica. Positive authorization is never cached: every
/// later request rechecks expiry, revocation, role, and security generation.
pub async fn authenticate(
    st: &SharedState,
    token: &str,
) -> AppResult<Option<VerifiedManagedApiToken>> {
    if !is_well_formed(token) {
        return Ok(None);
    }
    let token_hash = hash(token);
    let cache_key = negative_cache_key(&token_hash);
    if st.cache.get(&cache_key).await.is_some() {
        return Ok(None);
    }

    let flight_state = st.clone();
    // SingleFlight logs its bounded key on timeout. Use a domain-separated
    // secondary digest so logs never contain the persisted credential digest.
    let flight_key = format!("managed-auth:{}", hash(&token_hash));
    match AUTHENTICATION_FLIGHT
        .run_with_timeout(&flight_key, LOOKUP_TIMEOUT, move || {
            lookup(flight_state, token_hash)
        })
        .await
    {
        LookupResult::Found(token) => Ok(Some(token)),
        LookupResult::Missing => Ok(None),
        LookupResult::Failed => Err(AppError::internal("managed API token lookup failed")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_is_exact_and_non_overlapping() {
        let valid = format!("{PREFIX}{}", "a".repeat(ENCODED_SECRET_LEN));
        assert!(looks_managed(&valid));
        assert!(is_well_formed(&valid));
        assert!(!is_well_formed(
            "ad_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
        assert!(!is_well_formed("header.payload.signature"));
        assert!(!is_well_formed(&format!("{PREFIX}{}", "a".repeat(44))));
        assert!(!is_well_formed(&format!("{PREFIX}{}=", "a".repeat(42))));
        assert!(looks_managed("rsctf_pat_v2_unknown"));
    }

    #[test]
    fn scopes_are_explicit_and_method_bound() {
        let token = VerifiedManagedApiToken {
            user: CurrentUser {
                id: Uuid::nil(),
                role: Role::Admin,
                name: String::new(),
                security_stamp: "stamp".into(),
            },
            token_id: Uuid::nil(),
            partition_key: String::new(),
            scopes: vec![READ_SCOPE.into()],
        };
        assert!(token.permits(&Method::GET));
        assert!(!token.permits(&Method::POST));
        assert!(scopes_are_valid(&[READ_SCOPE.into(), WRITE_SCOPE.into()]));
        assert!(!scopes_are_valid(&[WRITE_SCOPE.into(), WRITE_SCOPE.into()]));
        assert!(!scopes_are_valid(&["admin:*".into()]));
    }

    #[test]
    fn hashes_and_cache_keys_never_expose_the_credential() {
        let token = format!("{PREFIX}{}", "s".repeat(ENCODED_SECRET_LEN));
        let token_hash = hash(&token);
        assert_eq!(token_hash.len(), 64);
        assert!(!negative_cache_key(&token_hash).contains(&token));
        assert_ne!(owner_stamp_digest("stamp-a"), owner_stamp_digest("stamp-b"));
    }

    #[test]
    fn authentication_sql_fences_every_live_dimension() {
        for fragment in [
            "token.token_version = $2",
            "token.audience = $3",
            "NOT token.is_revoked",
            "token.expires_at > clock_timestamp()",
            "account.role = $4",
            "account.security_stamp IS NOT NULL",
            "account.security_stamp <> ''",
            "LIMIT 1",
        ] {
            assert!(AUTHENTICATE_SQL.contains(fragment), "missing {fragment}");
        }
        assert!(TOUCH_LAST_USED_SQL.contains("interval '30 seconds'"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn real_postgres_round_trip_enforces_live_fences_and_throttles_usage() {
        use std::sync::Arc;

        use sea_orm::SqlxPostgresConnector;
        use sqlx::postgres::PgPoolOptions;

        use crate::app_state::AppState;
        use crate::models::internal::configs::AppConfig;
        use crate::services::cache::InMemoryCache;
        use crate::services::container::NoopContainerManager;
        use crate::services::token::TokenService;
        use crate::storage::LocalBlobStorage;

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TEMP TABLE "AspNetUsers" (
                   id UUID PRIMARY KEY,
                   role SMALLINT NOT NULL,
                   user_name TEXT,
                   security_stamp TEXT
               );
               CREATE TEMP TABLE "ApiTokens" (
                   id UUID PRIMARY KEY,
                   name TEXT NOT NULL,
                   token_hash TEXT NOT NULL UNIQUE,
                   creator_id UUID,
                   created_at TIMESTAMPTZ NOT NULL,
                   expires_at TIMESTAMPTZ,
                   last_used_at TIMESTAMPTZ,
                   is_revoked BOOLEAN NOT NULL,
                   token_version SMALLINT NOT NULL,
                   audience TEXT NOT NULL,
                   scopes TEXT[] NOT NULL,
                   owner_security_stamp_digest BYTEA
               )"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let owner_id = Uuid::new_v4();
        let token_id = Uuid::new_v4();
        let token = format!("{PREFIX}{}", "a".repeat(ENCODED_SECRET_LEN));
        let token_hash = hash(&token);
        sqlx::query(
            r#"INSERT INTO "AspNetUsers" (id, role, user_name, security_stamp)
               VALUES ($1, $2, 'operator', 'stamp-a')"#,
        )
        .bind(owner_id)
        .bind(Role::Admin as i16)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "ApiTokens"
                      (id, name, token_hash, creator_id, created_at, expires_at,
                       last_used_at, is_revoked, token_version, audience, scopes,
                       owner_security_stamp_digest)
               VALUES ($1, 'automation', $2, $3, clock_timestamp(), NULL,
                       NULL, FALSE, $4, $5, ARRAY[$6, $7], $8)"#,
        )
        .bind(token_id)
        .bind(&token_hash)
        .bind(owner_id)
        .bind(VERSION)
        .bind(AUDIENCE)
        .bind(READ_SCOPE)
        .bind(WRITE_SCOPE)
        .bind(owner_stamp_digest("stamp-a"))
        .execute(&pool)
        .await
        .unwrap();

        let database = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
        let storage_root = std::env::temp_dir().join(format!(
            "rsctf-managed-api-token-test-{}",
            Uuid::new_v4().simple()
        ));
        let state = AppState::new(
            database,
            Arc::new(AppConfig::default()),
            Arc::new(InMemoryCache::new()),
            Arc::new(LocalBlobStorage::new(storage_root)),
            TokenService::new("0123456789abcdef0123456789abcdef", 60),
            Arc::new(NoopContainerManager),
        );

        let burst = futures::future::join_all((0..16).map(|_| authenticate(&state, &token))).await;
        assert!(burst
            .iter()
            .all(|result| result.as_ref().is_ok_and(|token| token.is_some())));
        let first = burst.into_iter().next().unwrap().unwrap().unwrap();
        assert_eq!(first.user.id, owner_id);
        assert!(first.permits(&Method::GET));
        assert!(first.permits(&Method::POST));
        let first_used: DateTime<Utc> =
            sqlx::query_scalar(r#"SELECT last_used_at FROM "ApiTokens" WHERE id = $1"#)
                .bind(token_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        authenticate(&state, &token).await.unwrap().unwrap();
        let second_used: DateTime<Utc> =
            sqlx::query_scalar(r#"SELECT last_used_at FROM "ApiTokens" WHERE id = $1"#)
                .bind(token_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(first_used, second_used);

        sqlx::query(r#"UPDATE "ApiTokens" SET audience = 'wrong' WHERE id = $1"#)
            .bind(token_id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(authenticate(&state, &token).await.unwrap().is_none());
        state.cache.remove(&negative_cache_key(&token_hash)).await;
        sqlx::query(r#"UPDATE "ApiTokens" SET audience = $2 WHERE id = $1"#)
            .bind(token_id)
            .bind(AUDIENCE)
            .execute(&pool)
            .await
            .unwrap();

        sqlx::query(r#"UPDATE "AspNetUsers" SET role = $2 WHERE id = $1"#)
            .bind(owner_id)
            .bind(Role::User as i16)
            .execute(&pool)
            .await
            .unwrap();
        assert!(authenticate(&state, &token).await.unwrap().is_none());
        state.cache.remove(&negative_cache_key(&token_hash)).await;
        sqlx::query(
            r#"UPDATE "AspNetUsers" SET role = $2, security_stamp = 'stamp-b' WHERE id = $1"#,
        )
        .bind(owner_id)
        .bind(Role::Admin as i16)
        .execute(&pool)
        .await
        .unwrap();
        assert!(authenticate(&state, &token).await.unwrap().is_none());

        sqlx::query(
            r#"UPDATE "ApiTokens"
                  SET owner_security_stamp_digest = $2, is_revoked = TRUE
                WHERE id = $1"#,
        )
        .bind(token_id)
        .bind(owner_stamp_digest("stamp-b"))
        .execute(&pool)
        .await
        .unwrap();
        assert!(authenticate(&state, &token).await.unwrap().is_none());
        state.cache.remove(&negative_cache_key(&token_hash)).await;

        sqlx::query(
            r#"UPDATE "ApiTokens"
                  SET is_revoked = FALSE, expires_at = clock_timestamp() - interval '1 second'
                WHERE id = $1"#,
        )
        .bind(token_id)
        .execute(&pool)
        .await
        .unwrap();
        assert!(authenticate(&state, &token).await.unwrap().is_none());
        state.cache.remove(&negative_cache_key(&token_hash)).await;

        sqlx::query(r#"UPDATE "ApiTokens" SET expires_at = NULL WHERE id = $1"#)
            .bind(token_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"DELETE FROM "AspNetUsers" WHERE id = $1"#)
            .bind(owner_id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(authenticate(&state, &token).await.unwrap().is_none());
    }
}
