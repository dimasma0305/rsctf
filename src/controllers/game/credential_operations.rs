//! Exactly-once player credential mutations with short-lived encrypted recovery.

use aes_gcm::aead::consts::U12;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use axum::extract::{FromRequest, Request};
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

const MAX_CREDENTIAL_REVISION: i64 = 9_007_199_254_740_990;
const MAX_RECOVERY_PLAINTEXT_BYTES: usize = 64 * 1024;
const EXPIRED_PURGE_BATCH: i64 = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CredentialKind {
    AdToken,
    AdSsh,
    KothApi,
}

impl CredentialKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::AdToken => "AdToken",
            Self::AdSsh => "AdSsh",
            Self::KothApi => "KothApi",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CredentialScope {
    pub(crate) participation_id: i32,
    pub(crate) game_id: i32,
    pub(crate) challenge_id: i32,
    pub(crate) actor_user_id: Uuid,
    pub(crate) kind: CredentialKind,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialMutationRequest {
    pub(crate) operation_id: Uuid,
    pub(crate) expected_revision: i64,
}

/// Required revision-fenced identity for mutations whose only request fields
/// are the operation ID and expected revision.
#[derive(Debug)]
pub struct CredentialMutationInput(pub(crate) CredentialMutationRequest);

impl<S> FromRequest<S> for CredentialMutationInput
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<CredentialMutationRequest>::from_request(request, state)
            .await
            .map(|Json(request)| Self(request))
            .map_err(axum::response::IntoResponse::into_response)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FreshCredentialOperation {
    pub(crate) operation_id: Uuid,
    pub(crate) expected_revision: i64,
    pub(crate) result_revision: i64,
    pub(crate) recovery_expires_at: DateTime<Utc>,
    request_hash: [u8; 32],
}

pub(crate) enum CredentialReservation<T> {
    Recovered(T),
    Fresh(FreshCredentialOperation),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CredentialMutationAck {
    pub(crate) operation_id: Uuid,
    pub(crate) revision: i64,
    #[serde(with = "crate::utils::datetime::millis")]
    pub(crate) recovery_expires_at: DateTime<Utc>,
}

/// One-time plaintext responses must not be retained by intermediaries or the
/// browser cache. The encrypted database copy is independently time-bounded.
pub(crate) fn private_credential_response<T: Serialize>(model: T) -> Response {
    let mut response = RequestResponse::ok(model).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

#[derive(Debug, sqlx::FromRow)]
struct StoredOperationRow {
    participation_id: i32,
    game_id: i32,
    actor_user_id: Uuid,
    credential_kind: String,
    challenge_id: i32,
    expected_revision: i64,
    request_hash: Vec<u8>,
    result_revision: Option<i64>,
    result_ciphertext: Option<Vec<u8>>,
    result_nonce: Option<Vec<u8>>,
}

/// AppConfig has no dedicated player-recovery key. Derive a disjoint AES key
/// from the replica-stable, startup-validated JWT secret instead of reusing the
/// JWT key bytes directly. The versioned domain prevents cross-protocol use.
fn recovery_key(secret: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"rsctf:player-credential-recovery:key:v1\0");
    digest.update((secret.len() as u64).to_be_bytes());
    digest.update(secret.as_bytes());
    digest.finalize().into()
}

fn request_hash(scope: CredentialScope, request_binding: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"rsctf:player-credential-recovery:request:v1\0");
    digest.update(scope.participation_id.to_be_bytes());
    digest.update(scope.game_id.to_be_bytes());
    digest.update(scope.challenge_id.to_be_bytes());
    digest.update(scope.actor_user_id.as_bytes());
    digest.update(scope.kind.as_str().as_bytes());
    digest.update((request_binding.len() as u64).to_be_bytes());
    digest.update(request_binding);
    digest.finalize().into()
}

fn operation_aad(
    scope: CredentialScope,
    operation_id: Uuid,
    expected_revision: i64,
    result_revision: i64,
    request_hash: &[u8; 32],
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(160);
    aad.extend_from_slice(b"rsctf:player-credential-recovery:result:v1\0");
    aad.extend_from_slice(&scope.participation_id.to_be_bytes());
    aad.extend_from_slice(&scope.game_id.to_be_bytes());
    aad.extend_from_slice(&scope.challenge_id.to_be_bytes());
    aad.extend_from_slice(scope.actor_user_id.as_bytes());
    aad.extend_from_slice(scope.kind.as_str().as_bytes());
    aad.extend_from_slice(operation_id.as_bytes());
    aad.extend_from_slice(&expected_revision.to_be_bytes());
    aad.extend_from_slice(&result_revision.to_be_bytes());
    aad.extend_from_slice(request_hash);
    aad
}

fn encrypt_result<T: Serialize>(
    st: &SharedState,
    scope: CredentialScope,
    operation: FreshCredentialOperation,
    result: &T,
) -> AppResult<(Vec<u8>, [u8; 12])> {
    let plaintext = serde_json::to_vec(result)
        .map_err(|error| AppError::internal(format!("serialize credential result: {error}")))?;
    if plaintext.len() > MAX_RECOVERY_PLAINTEXT_BYTES {
        return Err(AppError::internal(
            "credential recovery result is too large",
        ));
    }
    let cipher = Aes256Gcm::new_from_slice(&recovery_key(&st.config.jwt_secret))
        .map_err(|_| AppError::internal("initialize credential recovery encryption"))?;
    let mut nonce = [0u8; 12];
    rand::fill(&mut nonce);
    let nonce_value: Nonce<U12> = nonce.into();
    let ciphertext = cipher
        .encrypt(
            &nonce_value,
            Payload {
                msg: &plaintext,
                aad: &operation_aad(
                    scope,
                    operation.operation_id,
                    operation.expected_revision,
                    operation.result_revision,
                    &operation.request_hash,
                ),
            },
        )
        .map_err(|_| AppError::internal("encrypt credential recovery result"))?;
    Ok((ciphertext, nonce))
}

fn decrypt_result<T: DeserializeOwned>(
    st: &SharedState,
    scope: CredentialScope,
    operation_id: Uuid,
    expected_revision: i64,
    result_revision: i64,
    request_hash: &[u8; 32],
    ciphertext: &[u8],
    nonce: &[u8],
) -> AppResult<T> {
    let nonce_value = Nonce::<U12>::try_from(nonce)
        .map_err(|_| AppError::internal("invalid credential recovery nonce"))?;
    let cipher = Aes256Gcm::new_from_slice(&recovery_key(&st.config.jwt_secret))
        .map_err(|_| AppError::internal("initialize credential recovery encryption"))?;
    let plaintext = cipher
        .decrypt(
            &nonce_value,
            Payload {
                msg: ciphertext,
                aad: &operation_aad(
                    scope,
                    operation_id,
                    expected_revision,
                    result_revision,
                    request_hash,
                ),
            },
        )
        .map_err(|_| AppError::unavailable("credential recovery result cannot be decrypted"))?;
    serde_json::from_slice(&plaintext)
        .map_err(|error| AppError::internal(format!("decode credential recovery result: {error}")))
}

/// Delete the requested expired record, then opportunistically remove at most
/// one bounded batch. SKIP LOCKED prevents unrelated team rotations from
/// convoying on the same cleanup rows.
async fn purge_expired_operations(
    connection: &mut sqlx::PgConnection,
    operation_id: Uuid,
) -> AppResult<()> {
    sqlx::query(
        r#"DELETE FROM "PlayerCredentialOperations"
            WHERE operation_id = $1 AND expires_at <= clock_timestamp()"#,
    )
    .bind(operation_id)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"DELETE FROM "PlayerCredentialOperations" operation
            USING (
              SELECT operation_id FROM "PlayerCredentialOperations"
               WHERE expires_at <= clock_timestamp()
               ORDER BY expires_at
               LIMIT $1 FOR UPDATE SKIP LOCKED
            ) expired
            WHERE operation.operation_id = expired.operation_id"#,
    )
    .bind(EXPIRED_PURGE_BATCH)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

#[cfg(test)]
pub(crate) async fn current_revision<'e, E>(
    executor: E,
    participation_id: i32,
    game_id: i32,
    kind: CredentialKind,
    challenge_id: i32,
) -> AppResult<i64>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query_scalar(
        r#"SELECT COALESCE(
               (SELECT revision FROM "PlayerCredentialRevisions"
                 WHERE participation_id = $1 AND credential_kind = $4
                   AND challenge_id = $3),
               CASE $4
                 WHEN 'AdToken' THEN CASE WHEN EXISTS (
                   SELECT 1 FROM "AdTeamApiTokens" WHERE participation_id = $1
                 ) THEN 1 ELSE 0 END
                 WHEN 'AdSsh' THEN CASE WHEN EXISTS (
                   SELECT 1 FROM "AdSshKeys" WHERE participation_id = $1
                 ) THEN 1 ELSE 0 END
                 WHEN 'KothApi' THEN COALESCE((
                   SELECT generation::BIGINT FROM "KothApiTeamTokens"
                    WHERE game_id = $2 AND challenge_id = $3
                      AND participation_id = $1
                 ), 0)
                 ELSE 0
               END,
               0
           )::BIGINT"#,
    )
    .bind(participation_id)
    .bind(game_id)
    .bind(challenge_id)
    .bind(kind.as_str())
    .fetch_one(executor)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

async fn ensure_revision_row(
    connection: &mut sqlx::PgConnection,
    scope: CredentialScope,
) -> AppResult<()> {
    sqlx::query(
        r#"INSERT INTO "PlayerCredentialRevisions"
               (participation_id, credential_kind, challenge_id, revision)
           VALUES (
               $1, $4, $3,
               CASE $4
                 WHEN 'AdToken' THEN CASE WHEN EXISTS (
                   SELECT 1 FROM "AdTeamApiTokens" WHERE participation_id = $1
                 ) THEN 1 ELSE 0 END
                 WHEN 'AdSsh' THEN CASE WHEN EXISTS (
                   SELECT 1 FROM "AdSshKeys" WHERE participation_id = $1
                 ) THEN 1 ELSE 0 END
                 WHEN 'KothApi' THEN COALESCE((
                   SELECT generation::BIGINT FROM "KothApiTeamTokens"
                    WHERE game_id = $2 AND challenge_id = $3
                      AND participation_id = $1
                 ), 0)
                 ELSE 0
               END
           )
           ON CONFLICT DO NOTHING"#,
    )
    .bind(scope.participation_id)
    .bind(scope.game_id)
    .bind(scope.challenge_id)
    .bind(scope.kind.as_str())
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

pub(crate) async fn reserve<T: DeserializeOwned>(
    st: &SharedState,
    connection: &mut sqlx::PgConnection,
    scope: CredentialScope,
    request: CredentialMutationRequest,
    request_binding: &[u8],
) -> AppResult<CredentialReservation<T>> {
    if request.operation_id.is_nil()
        || !(0..=MAX_CREDENTIAL_REVISION).contains(&request.expected_revision)
    {
        return Err(AppError::bad_request(
            "operationId must be opaque and expectedRevision must be a valid revision",
        ));
    }
    // The shipped KotH token generation column is INTEGER. Keep its durable
    // fence in the same representable domain until that public model is
    // deliberately migrated to BIGINT.
    if scope.kind == CredentialKind::KothApi && request.expected_revision >= i64::from(i32::MAX) {
        return Err(AppError::conflict(
            "KotH credential revision space is exhausted",
        ));
    }
    purge_expired_operations(connection, request.operation_id).await?;
    ensure_revision_row(connection, scope).await?;

    let current: i64 = sqlx::query_scalar(
        r#"SELECT revision FROM "PlayerCredentialRevisions"
            WHERE participation_id = $1 AND credential_kind = $2
              AND challenge_id = $3 FOR UPDATE"#,
    )
    .bind(scope.participation_id)
    .bind(scope.kind.as_str())
    .bind(scope.challenge_id)
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let expected_request_hash = request_hash(scope, request_binding);

    if let Some(row) = sqlx::query_as::<_, StoredOperationRow>(
        r#"SELECT participation_id, game_id, actor_user_id, credential_kind,
                  challenge_id, expected_revision, request_hash, result_revision,
                  result_ciphertext, result_nonce
             FROM "PlayerCredentialOperations"
            WHERE operation_id = $1 AND expires_at > clock_timestamp()
            FOR UPDATE"#,
    )
    .bind(request.operation_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    {
        if row.participation_id != scope.participation_id
            || row.game_id != scope.game_id
            || row.actor_user_id != scope.actor_user_id
            || row.credential_kind != scope.kind.as_str()
            || row.challenge_id != scope.challenge_id
            || row.expected_revision != request.expected_revision
            || row.request_hash.as_slice() != expected_request_hash.as_slice()
        {
            return Err(AppError::conflict(
                "credential operation ID belongs to another request",
            ));
        }
        let (Some(result_revision), Some(ciphertext), Some(nonce)) = (
            row.result_revision,
            row.result_ciphertext.as_deref(),
            row.result_nonce.as_deref(),
        ) else {
            return Err(AppError::conflict(
                "credential operation is still in progress",
            ));
        };
        if current != result_revision {
            return Err(AppError::conflict(
                "credential operation was superseded by a newer credential",
            ));
        }
        let result = decrypt_result(
            st,
            scope,
            request.operation_id,
            request.expected_revision,
            result_revision,
            &expected_request_hash,
            ciphertext,
            nonce,
        )?;
        sqlx::query(
            r#"UPDATE "PlayerCredentialOperations"
                  SET disclosure_count = disclosure_count + 1,
                      last_disclosed_at = clock_timestamp()
                WHERE operation_id = $1 AND result_revision = $2"#,
        )
        .bind(request.operation_id)
        .bind(result_revision)
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(CredentialReservation::Recovered(result));
    }

    if current != request.expected_revision {
        return Err(AppError::conflict(format!(
            "credential changed; expected revision {}, current revision is {current}",
            request.expected_revision
        )));
    }

    let recovery_expires_at: DateTime<Utc> = sqlx::query_scalar(
        r#"INSERT INTO "PlayerCredentialOperations"
               (operation_id, participation_id, game_id, actor_user_id,
                credential_kind, challenge_id, expected_revision, request_hash)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
           RETURNING expires_at"#,
    )
    .bind(request.operation_id)
    .bind(scope.participation_id)
    .bind(scope.game_id)
    .bind(scope.actor_user_id)
    .bind(scope.kind.as_str())
    .bind(scope.challenge_id)
    .bind(request.expected_revision)
    .bind(expected_request_hash.as_slice())
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| {
        if crate::utils::error::is_unique_violation(&error) {
            AppError::conflict("credential operation already exists")
        } else {
            AppError::internal(error.to_string())
        }
    })?;

    // Compatibility triggers advance revisions for legacy writers and old
    // replicas. This transaction owns the explicit expectedRevision CAS in
    // complete(), so suppress only this exact credential scope until commit.
    let managed_scope = format!(
        "{}:{}:{}",
        scope.participation_id,
        scope.kind.as_str(),
        scope.challenge_id
    );
    sqlx::query_scalar::<_, String>(
        "SELECT set_config('rsctf.player_credential_revision_managed', $1, TRUE)",
    )
    .bind(managed_scope)
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    Ok(CredentialReservation::Fresh(FreshCredentialOperation {
        operation_id: request.operation_id,
        expected_revision: request.expected_revision,
        result_revision: request.expected_revision + 1,
        recovery_expires_at,
        request_hash: expected_request_hash,
    }))
}

pub(crate) async fn complete<T: Serialize>(
    st: &SharedState,
    connection: &mut sqlx::PgConnection,
    scope: CredentialScope,
    operation: FreshCredentialOperation,
    result: &T,
) -> AppResult<()> {
    let (ciphertext, nonce) = encrypt_result(st, scope, operation, result)?;
    let revised = sqlx::query(
        r#"UPDATE "PlayerCredentialRevisions"
              SET revision = $4, updated_at = clock_timestamp()
            WHERE participation_id = $1 AND credential_kind = $2
              AND challenge_id = $3 AND revision = $5"#,
    )
    .bind(scope.participation_id)
    .bind(scope.kind.as_str())
    .bind(scope.challenge_id)
    .bind(operation.result_revision)
    .bind(operation.expected_revision)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if revised.rows_affected() != 1 {
        return Err(AppError::conflict(
            "credential revision changed during mutation",
        ));
    }
    let completed = sqlx::query(
        r#"UPDATE "PlayerCredentialOperations"
              SET result_revision = $2, result_ciphertext = $3,
                  result_nonce = $4, completed_at = clock_timestamp(),
                  disclosure_count = 1, last_disclosed_at = clock_timestamp()
            WHERE operation_id = $1 AND participation_id = $5
              AND game_id = $6 AND actor_user_id = $7
              AND credential_kind = $8 AND challenge_id = $9
              AND expected_revision = $10 AND request_hash = $11
              AND completed_at IS NULL"#,
    )
    .bind(operation.operation_id)
    .bind(operation.result_revision)
    .bind(ciphertext)
    .bind(nonce.as_slice())
    .bind(scope.participation_id)
    .bind(scope.game_id)
    .bind(scope.actor_user_id)
    .bind(scope.kind.as_str())
    .bind(scope.challenge_id)
    .bind(operation.expected_revision)
    .bind(operation.request_hash.as_slice())
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if completed.rows_affected() != 1 {
        return Err(AppError::conflict(
            "credential operation changed during mutation",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::extract::FromRequest;
    use axum::http::{header, Request};

    use super::{
        operation_aad, private_credential_response, recovery_key, request_hash, CredentialKind,
        CredentialMutationInput, CredentialScope,
    };

    fn scope() -> CredentialScope {
        CredentialScope {
            participation_id: 7,
            game_id: 9,
            challenge_id: 11,
            actor_user_id: uuid::Uuid::from_u128(10),
            kind: CredentialKind::KothApi,
        }
    }

    #[test]
    fn recovery_ciphertext_is_bound_to_request_actor_scope_and_revision() {
        let scope = scope();
        let operation = uuid::Uuid::from_u128(12);
        let first_hash = request_hash(scope, b"first request");
        let aad = operation_aad(scope, operation, 3, 4, &first_hash);
        assert_ne!(aad, operation_aad(scope, operation, 4, 5, &first_hash));
        assert_ne!(
            aad,
            operation_aad(
                CredentialScope {
                    actor_user_id: uuid::Uuid::from_u128(1),
                    ..scope
                },
                operation,
                3,
                4,
                &first_hash,
            )
        );
        assert_ne!(
            first_hash,
            request_hash(scope, b"second request"),
            "the same operation ID cannot be rebound to changed mutation input"
        );
    }

    #[test]
    fn recovery_key_is_domain_derived() {
        let secret = "0123456789abcdef0123456789abcdef";
        assert_ne!(recovery_key(secret).as_slice(), secret.as_bytes());
        assert_ne!(
            recovery_key(secret),
            recovery_key("different deployment secret")
        );
    }

    #[tokio::test]
    async fn credential_mutations_reject_missing_operation_identity() {
        let request = Request::builder().uri("/").body(Body::empty()).unwrap();
        let rejection = CredentialMutationInput::from_request(request, &())
            .await
            .expect_err("empty mutations must not receive a server-generated ID");
        assert!(rejection.status().is_client_error());

        let request = Request::builder()
            .uri("/")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"operationId":"00000000-0000-4000-8000-000000000001","expectedRevision":3}"#,
            ))
            .unwrap();
        let parsed = CredentialMutationInput::from_request(request, &())
            .await
            .expect("a stable operation identity is accepted");
        assert_eq!(parsed.0.expected_revision, 3);
    }

    #[test]
    fn one_time_credential_responses_cannot_be_cached() {
        let response = private_credential_response(serde_json::json!({ "secret": "sensitive" }));
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "private, no-store"
        );
        assert_eq!(response.headers().get(header::PRAGMA).unwrap(), "no-cache");
    }
}

#[cfg(test)]
#[path = "credential_operations_tests.rs"]
mod postgres_tests;
