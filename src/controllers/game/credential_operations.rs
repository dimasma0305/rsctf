//! Exactly-once player credential mutations with short-lived encrypted recovery.

use aes_gcm::aead::consts::U12;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use axum::body::{Body, Bytes};
use axum::extract::{FromRequest, Request};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

const MAX_CREDENTIAL_REVISION: i64 = 9_007_199_254_740_990;

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

/// Backwards-compatible request extractor. New clients send a revision-fenced
/// JSON body; legacy empty requests still work but cannot recover across a lost
/// response until they upgrade.
pub struct CredentialMutationInput(pub(crate) Option<CredentialMutationRequest>);

impl<S> FromRequest<S> for CredentialMutationInput
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let headers = request.headers().clone();
        let bytes = Bytes::from_request(request, state)
            .await
            .map_err(|rejection| rejection.into_response())?;
        if bytes.is_empty() && headers.get(header::CONTENT_TYPE).is_none() {
            return Ok(Self(None));
        }
        let mut json_request = Request::new(Body::from(bytes));
        *json_request.headers_mut() = headers;
        Json::<CredentialMutationRequest>::from_request(json_request, state)
            .await
            .map(|Json(request)| Self(Some(request)))
            .map_err(|rejection| rejection.into_response())
    }
}

pub(crate) enum CredentialReservation<T> {
    Recovered(T),
    Fresh {
        operation_id: Uuid,
        expected_revision: i64,
        result_revision: i64,
    },
}

type StoredOperationRow = (
    i32,
    i32,
    Uuid,
    String,
    i32,
    i64,
    Option<i64>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
);

fn recovery_key(secret: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"rsctf:player-credential-recovery:v1\0");
    digest.update(secret.as_bytes());
    digest.finalize().into()
}

fn operation_aad(
    scope: CredentialScope,
    operation_id: Uuid,
    expected_revision: i64,
    result_revision: i64,
) -> Vec<u8> {
    format!(
        "v1:{}:{}:{}:{}:{}:{}:{}",
        scope.participation_id,
        scope.game_id,
        scope.actor_user_id,
        scope.kind.as_str(),
        scope.challenge_id,
        operation_id,
        expected_revision.max(result_revision - 1),
    )
    .into_bytes()
}

fn encrypt_result<T: Serialize>(
    st: &SharedState,
    scope: CredentialScope,
    operation_id: Uuid,
    expected_revision: i64,
    result_revision: i64,
    result: &T,
) -> AppResult<(Vec<u8>, [u8; 12])> {
    let plaintext = serde_json::to_vec(result)
        .map_err(|error| AppError::internal(format!("serialize credential result: {error}")))?;
    if plaintext.len() > 256 * 1024 {
        return Err(AppError::internal(
            "credential recovery result is too large",
        ));
    }
    let cipher = Aes256Gcm::new_from_slice(&recovery_key(&st.config.jwt_secret))
        .map_err(|_| AppError::internal("initialize credential recovery encryption"))?;
    let nonce: [u8; 12] = rand::random();
    let nonce_value: Nonce<U12> = nonce.into();
    let ciphertext = cipher
        .encrypt(
            &nonce_value,
            Payload {
                msg: &plaintext,
                aad: &operation_aad(scope, operation_id, expected_revision, result_revision),
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
                aad: &operation_aad(scope, operation_id, expected_revision, result_revision),
            },
        )
        .map_err(|_| AppError::unavailable("credential recovery result cannot be decrypted"))?;
    serde_json::from_slice(&plaintext)
        .map_err(|error| AppError::internal(format!("decode credential recovery result: {error}")))
}

async fn purge_expired_operations(
    connection: &mut sqlx::PgConnection,
    operation_id: Option<Uuid>,
) -> AppResult<()> {
    if let Some(operation_id) = operation_id {
        sqlx::query(
            r#"DELETE FROM "PlayerCredentialOperations"
                WHERE operation_id = $1 AND expires_at <= clock_timestamp()"#,
        )
        .bind(operation_id)
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    sqlx::query(
        r#"DELETE FROM "PlayerCredentialOperations"
            WHERE operation_id IN (
              SELECT operation_id FROM "PlayerCredentialOperations"
               WHERE expires_at <= clock_timestamp()
               ORDER BY expires_at LIMIT 127
            )"#,
    )
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

pub(crate) async fn current_revision<'e, E>(
    executor: E,
    participation_id: i32,
    kind: CredentialKind,
    challenge_id: i32,
) -> AppResult<i64>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query_scalar(
        r#"SELECT revision FROM "PlayerCredentialRevisions"
            WHERE participation_id = $1 AND credential_kind = $2
              AND challenge_id = $3"#,
    )
    .bind(participation_id)
    .bind(kind.as_str())
    .bind(challenge_id)
    .fetch_optional(executor)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
    .map(|revision| revision.unwrap_or(0))
}

/// Advance a credential generation for non-secret upload/revoke operations.
/// Callers retain the same roster write fence as secret generation, so this
/// cannot race a revision-fenced one-time response.
pub(crate) async fn advance_revision(
    connection: &mut sqlx::PgConnection,
    participation_id: i32,
    kind: CredentialKind,
    challenge_id: i32,
) -> AppResult<i64> {
    sqlx::query(
        r#"INSERT INTO "PlayerCredentialRevisions"
               (participation_id, credential_kind, challenge_id, revision)
           VALUES ($1, $2, $3, 0)
           ON CONFLICT DO NOTHING"#,
    )
    .bind(participation_id)
    .bind(kind.as_str())
    .bind(challenge_id)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query_scalar(
        r#"UPDATE "PlayerCredentialRevisions"
              SET revision = revision + 1, updated_at = clock_timestamp()
            WHERE participation_id = $1 AND credential_kind = $2
              AND challenge_id = $3 AND revision < 9007199254740991
          RETURNING revision"#,
    )
    .bind(participation_id)
    .bind(kind.as_str())
    .bind(challenge_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::conflict("credential revision limit reached"))
}

pub(crate) async fn reserve<T: DeserializeOwned>(
    st: &SharedState,
    connection: &mut sqlx::PgConnection,
    scope: CredentialScope,
    request: Option<CredentialMutationRequest>,
) -> AppResult<CredentialReservation<T>> {
    if request.is_some_and(|request| {
        request.operation_id.is_nil()
            || !(0..=MAX_CREDENTIAL_REVISION).contains(&request.expected_revision)
    }) {
        return Err(AppError::bad_request(
            "operationId must be opaque and expectedRevision must be a valid revision",
        ));
    }
    purge_expired_operations(connection, request.map(|request| request.operation_id)).await?;

    sqlx::query(
        r#"INSERT INTO "PlayerCredentialRevisions"
               (participation_id, credential_kind, challenge_id, revision)
           VALUES ($1, $2, $3, 0)
           ON CONFLICT DO NOTHING"#,
    )
    .bind(scope.participation_id)
    .bind(scope.kind.as_str())
    .bind(scope.challenge_id)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

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

    let operation_id = request
        .map(|request| request.operation_id)
        .unwrap_or_else(Uuid::new_v4);
    let expected_revision = request
        .map(|request| request.expected_revision)
        .unwrap_or(current);

    if let Some(row) = sqlx::query_as::<_, StoredOperationRow>(
        r#"SELECT participation_id, game_id, actor_user_id, credential_kind,
                  challenge_id, expected_revision, result_revision,
                  result_ciphertext, result_nonce
             FROM "PlayerCredentialOperations"
            WHERE operation_id = $1 AND expires_at > clock_timestamp()
            FOR UPDATE"#,
    )
    .bind(operation_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    {
        if row.0 != scope.participation_id
            || row.1 != scope.game_id
            || row.2 != scope.actor_user_id
            || row.3 != scope.kind.as_str()
            || row.4 != scope.challenge_id
            || row.5 != expected_revision
        {
            return Err(AppError::conflict(
                "credential operation ID belongs to another request",
            ));
        }
        let (Some(result_revision), Some(ciphertext), Some(nonce)) =
            (row.6, row.7.as_deref(), row.8.as_deref())
        else {
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
            operation_id,
            expected_revision,
            result_revision,
            ciphertext,
            nonce,
        )?;
        sqlx::query(
            r#"UPDATE "PlayerCredentialOperations"
                  SET disclosure_count = disclosure_count + 1,
                      last_disclosed_at = clock_timestamp()
                WHERE operation_id = $1"#,
        )
        .bind(operation_id)
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(CredentialReservation::Recovered(result));
    }

    if current != expected_revision {
        return Err(AppError::conflict(format!(
            "credential changed; expected revision {expected_revision}, current revision is {current}"
        )));
    }

    sqlx::query(
        r#"INSERT INTO "PlayerCredentialOperations"
               (operation_id, participation_id, game_id, actor_user_id,
                credential_kind, challenge_id, expected_revision)
           VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
    )
    .bind(operation_id)
    .bind(scope.participation_id)
    .bind(scope.game_id)
    .bind(scope.actor_user_id)
    .bind(scope.kind.as_str())
    .bind(scope.challenge_id)
    .bind(expected_revision)
    .execute(&mut *connection)
    .await
    .map_err(|error| {
        if crate::utils::error::is_unique_violation(&error) {
            AppError::conflict("credential operation already exists")
        } else {
            AppError::internal(error.to_string())
        }
    })?;

    Ok(CredentialReservation::Fresh {
        operation_id,
        expected_revision,
        result_revision: expected_revision + 1,
    })
}

pub(crate) async fn complete<T: Serialize>(
    st: &SharedState,
    connection: &mut sqlx::PgConnection,
    scope: CredentialScope,
    operation_id: Uuid,
    expected_revision: i64,
    result_revision: i64,
    result: &T,
) -> AppResult<()> {
    let (ciphertext, nonce) = encrypt_result(
        st,
        scope,
        operation_id,
        expected_revision,
        result_revision,
        result,
    )?;
    let revised = sqlx::query(
        r#"UPDATE "PlayerCredentialRevisions"
              SET revision = $4, updated_at = clock_timestamp()
            WHERE participation_id = $1 AND credential_kind = $2
              AND challenge_id = $3 AND revision = $5"#,
    )
    .bind(scope.participation_id)
    .bind(scope.kind.as_str())
    .bind(scope.challenge_id)
    .bind(result_revision)
    .bind(expected_revision)
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
            WHERE operation_id = $1 AND completed_at IS NULL"#,
    )
    .bind(operation_id)
    .bind(result_revision)
    .bind(ciphertext)
    .bind(nonce.as_slice())
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
    use super::{operation_aad, CredentialKind, CredentialScope};

    #[test]
    fn recovery_ciphertext_is_bound_to_exact_actor_scope_and_revision() {
        let scope = CredentialScope {
            participation_id: 7,
            game_id: 9,
            challenge_id: 11,
            actor_user_id: uuid::Uuid::nil(),
            kind: CredentialKind::KothApi,
        };
        let operation = uuid::Uuid::from_u128(12);
        let aad = operation_aad(scope, operation, 3, 4);
        assert_ne!(aad, operation_aad(scope, operation, 4, 5));
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
            )
        );
    }
}
