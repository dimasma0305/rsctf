//! SSH key endpoints (get/upload/generate/delete) + Ed25519/OpenSSH helpers.

use super::*;
use axum::response::Response;

const UPLOAD_SSH_KEY_BINDING: &[u8] = b"upload-ssh-key\0";
const GENERATE_SSH_KEY_BINDING: &[u8] = b"generate-ssh-key";
const DELETE_SSH_KEY_BINDING: &[u8] = b"delete-ssh-key";

/// Body for `POST /api/Game/{id}/Ad/Ssh/Key` (`AdSshKeyUploadModel`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdSshKeyUploadModel {
    #[serde(default)]
    pub public_key: String,
    pub operation_id: uuid::Uuid,
    pub expected_revision: i64,
}

/// `AdSshKeyInfoModel` — GET/POST `Ad/Ssh/Key` response (no plaintext).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdSshKeyInfoModel {
    pub exists: bool,
    pub algorithm: String,
    pub fingerprint: String,
    pub platform_generated: bool,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub last_used_at: Option<DateTime<Utc>>,
    pub jump_host: Option<String>,
    pub revision: i64,
}

/// Successful client-supplied key mutation, including the identity needed to
/// ignore an older response that arrives after a competing request.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdSshKeyMutationResultModel {
    #[serde(flatten)]
    pub key: AdSshKeyInfoModel,
    pub operation_id: uuid::Uuid,
    #[serde(with = "crate::utils::datetime::millis")]
    pub recovery_expires_at: DateTime<Utc>,
}

/// `AdSshKeyGeneratedModel` — server-generated keypair (private key once).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdSshKeyGeneratedModel {
    pub algorithm: String,
    pub public_key: String,
    pub private_key: String,
    pub fingerprint: String,
    pub operation_id: uuid::Uuid,
    pub revision: i64,
    #[serde(with = "crate::utils::datetime::millis")]
    pub created_at: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub recovery_expires_at: DateTime<Utc>,
}

/// `GET /api/Game/{id}/Ad/Ssh/Key` — metadata for the caller team's registered
/// SSH key (never the private half). `exists = false` when none is stored.
pub async fn get_ssh_key(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<AdSshKeyInfoModel>> {
    let part = resolve_participation(&st, &user, id).await?;
    // Read the public half and its durable revision in one statement snapshot;
    // a split read can otherwise pair an old key with a newer mutation fence.
    let row: (
        bool,
        String,
        String,
        bool,
        Option<DateTime<Utc>>,
        Option<DateTime<Utc>>,
        i64,
    ) = sqlx::query_as(
        r#"SELECT key.participation_id IS NOT NULL AS credential_exists,
                  COALESCE(key.algorithm, '') AS algorithm,
                  COALESCE(key.fingerprint, '') AS fingerprint,
                  COALESCE(key.platform_generated, FALSE) AS platform_generated,
                  key.created_at_utc,
                  key.last_used_at_utc,
                  COALESCE(revision.revision,
                           CASE WHEN key.participation_id IS NULL THEN 0 ELSE 1 END
                  )::BIGINT AS revision
             FROM (SELECT $1::INTEGER AS participation_id) scope
             LEFT JOIN "AdSshKeys" key
               ON key.participation_id = scope.participation_id
             LEFT JOIN "PlayerCredentialRevisions" revision
               ON revision.participation_id = scope.participation_id
              AND revision.credential_kind = 'AdSsh'
              AND revision.challenge_id = 0"#,
    )
    .bind(part.id)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let model = AdSshKeyInfoModel {
        exists: row.0,
        algorithm: row.1,
        fingerprint: row.2,
        platform_generated: row.3,
        created_at: row.4,
        last_used_at: row.5,
        jump_host: crate::services::ad_ssh::jump_host(),
        revision: row.6,
    };
    Ok(RequestResponse::ok(model))
}

/// `POST /api/Game/{id}/Ad/Ssh/Key` — register the client-supplied OpenSSH public
/// key. Parses `algorithm base64 [comment]`, derives the SHA256 fingerprint, and
/// upserts the row for the caller's participation (`platform_generated = false`).
pub async fn upload_ssh_key(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    axum::Json(model): axum::Json<AdSshKeyUploadModel>,
) -> AppResult<Response> {
    let part = resolve_participation(&st, &user, id).await?;
    let (algorithm, fingerprint) = parse_ssh_public_key(&model.public_key)?;
    let public_key = model.public_key.trim().to_string();
    let mut request_binding = Vec::with_capacity(UPLOAD_SSH_KEY_BINDING.len() + public_key.len());
    request_binding.extend_from_slice(UPLOAD_SSH_KEY_BINDING);
    request_binding.extend_from_slice(public_key.as_bytes());
    let request = crate::controllers::game::credential_operations::CredentialMutationRequest {
        operation_id: model.operation_id,
        expected_revision: model.expected_revision,
    };
    let mut roster = super::vpn::acquire_roster_access(&st, &user, &part).await?;
    let scope = crate::controllers::game::credential_operations::CredentialScope {
        participation_id: part.id,
        game_id: id,
        challenge_id: 0,
        actor_user_id: user.id,
        kind: crate::controllers::game::credential_operations::CredentialKind::AdSsh,
    };
    let reservation: crate::controllers::game::credential_operations::CredentialReservation<
        AdSshKeyMutationResultModel,
    > = crate::controllers::game::credential_operations::reserve(
        &st,
        roster.transaction_mut(),
        scope,
        request,
        &request_binding,
    )
    .await?;
    let operation = match reservation {
        crate::controllers::game::credential_operations::CredentialReservation::Recovered(
            result,
        ) => {
            let is_current: bool = sqlx::query_scalar(
                r#"SELECT EXISTS (
                     SELECT 1 FROM "AdSshKeys"
                      WHERE participation_id = $1 AND fingerprint = $2
                   )"#,
            )
            .bind(part.id)
            .bind(&result.key.fingerprint)
            .fetch_one(&mut **roster.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            if !is_current {
                return Err(AppError::conflict(
                    "credential operation no longer names the active SSH key",
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
    let now = Utc::now();

    sqlx::query(
        r#"INSERT INTO "AdSshKeys"
             (participation_id, algorithm, public_key, fingerprint,
              platform_generated, created_at_utc, last_used_at_utc)
           VALUES ($1, $2, $3, $4, FALSE, $5, NULL)
           ON CONFLICT (participation_id) DO UPDATE SET
             algorithm = EXCLUDED.algorithm,
             public_key = EXCLUDED.public_key,
             fingerprint = EXCLUDED.fingerprint,
             platform_generated = FALSE,
             created_at_utc = EXCLUDED.created_at_utc,
             last_used_at_utc = NULL"#,
    )
    .bind(part.id)
    .bind(&algorithm)
    .bind(&public_key)
    .bind(&fingerprint)
    .bind(now)
    .execute(&mut **roster.transaction_mut())
    .await
    .map_err(map_ssh_key_write_error)?;
    let result = AdSshKeyMutationResultModel {
        key: AdSshKeyInfoModel {
            exists: true,
            algorithm,
            fingerprint,
            platform_generated: false,
            created_at: Some(now),
            last_used_at: None,
            jump_host: crate::services::ad_ssh::jump_host(),
            revision: operation.result_revision,
        },
        operation_id: operation.operation_id,
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

/// `DELETE /api/Game/{id}/Ad/Ssh/Key` — remove the caller team's registered key.
pub async fn delete_ssh_key(
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
        kind: crate::controllers::game::credential_operations::CredentialKind::AdSsh,
    };
    let reservation: crate::controllers::game::credential_operations::CredentialReservation<
        crate::controllers::game::credential_operations::CredentialMutationAck,
    > = crate::controllers::game::credential_operations::reserve(
        &st,
        roster.transaction_mut(),
        scope,
        request,
        DELETE_SSH_KEY_BINDING,
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
    sqlx::query(r#"DELETE FROM "AdSshKeys" WHERE participation_id = $1"#)
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

/// `POST /api/Game/{id}/Ad/Ssh/Key/Generate` — server-side Ed25519 keypair. The
/// OpenSSH-format private key is returned once (not stored); the public key +
/// fingerprint are persisted (`platform_generated = true`). Injecting the key
/// into a running container is infra-gated and intentionally left out.
pub async fn generate_ssh_key(
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
        kind: crate::controllers::game::credential_operations::CredentialKind::AdSsh,
    };
    let reservation: crate::controllers::game::credential_operations::CredentialReservation<
        AdSshKeyGeneratedModel,
    > = crate::controllers::game::credential_operations::reserve(
        &st,
        roster.transaction_mut(),
        scope,
        request,
        GENERATE_SSH_KEY_BINDING,
    )
    .await?;
    let operation = match reservation {
        crate::controllers::game::credential_operations::CredentialReservation::Recovered(
            result,
        ) => {
            let is_current: bool = sqlx::query_scalar(
                r#"SELECT EXISTS (
                     SELECT 1 FROM "AdSshKeys"
                      WHERE participation_id = $1 AND fingerprint = $2
                   )"#,
            )
            .bind(part.id)
            .bind(&result.fingerprint)
            .fetch_one(&mut **roster.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            if !is_current {
                return Err(AppError::conflict(
                    "credential operation no longer names the active SSH key",
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
    let kp = generate_ed25519_keypair();
    let now = Utc::now();

    sqlx::query(
        r#"INSERT INTO "AdSshKeys"
             (participation_id, algorithm, public_key, fingerprint,
              platform_generated, created_at_utc, last_used_at_utc)
           VALUES ($1, 'ssh-ed25519', $2, $3, TRUE, $4, NULL)
           ON CONFLICT (participation_id) DO UPDATE SET
             algorithm = EXCLUDED.algorithm,
             public_key = EXCLUDED.public_key,
             fingerprint = EXCLUDED.fingerprint,
             platform_generated = TRUE,
             created_at_utc = EXCLUDED.created_at_utc,
             last_used_at_utc = NULL"#,
    )
    .bind(part.id)
    .bind(&kp.public_key)
    .bind(&kp.fingerprint)
    .bind(now)
    .execute(&mut **roster.transaction_mut())
    .await
    .map_err(map_ssh_key_write_error)?;
    let result = AdSshKeyGeneratedModel {
        algorithm: "ssh-ed25519".to_string(),
        public_key: kp.public_key,
        private_key: kp.private_key,
        fingerprint: kp.fingerprint,
        operation_id: operation.operation_id,
        revision: operation.result_revision,
        created_at: now,
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

const SSH_FINGERPRINT_INDEX: &str = "ux_adsshkeys_fingerprint";

/// A key can authenticate only one participation. PostgreSQL arbitrates concurrent
/// uploads through the fingerprint index; expose that expected collision without
/// leaking which team owns the key, while retaining internal handling for other DB
/// failures.
fn map_ssh_key_write_error(error: sqlx::Error) -> AppError {
    if is_ssh_fingerprint_conflict(&error) {
        AppError::conflict("This SSH public key is already registered")
    } else {
        AppError::internal(error.to_string())
    }
}

fn is_ssh_fingerprint_conflict(error: &sqlx::Error) -> bool {
    crate::utils::error::is_unique_violation(error)
        && error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::constraint)
            == Some(SSH_FINGERPRINT_INDEX)
}

/// A freshly-generated Ed25519 keypair in OpenSSH on-disk formats.
struct GeneratedSshKey {
    /// `ssh-ed25519 <b64(wireblob)> rsctf` — the `~/.ssh/*.pub` line.
    public_key: String,
    /// PEM `OPENSSH PRIVATE KEY` block (unencrypted) — returned once, never stored.
    private_key: String,
    /// `SHA256:<unpadded-b64>` fingerprint of the public wire blob.
    fingerprint: String,
}

/// Generate a fresh Ed25519 keypair (port of RSCTF `AdSshKeyUtils.GenerateEd25519`):
/// seed 32 random bytes → `SigningKey` → verifying key; encode the OpenSSH public
/// line, the OpenSSH private PEM, and the SHA256 fingerprint.
fn generate_ed25519_keypair() -> GeneratedSshKey {
    let mut seed = [0u8; 32];
    fill_random(&mut seed);
    let signing = SigningKey::from_bytes(&seed);
    let public = signing.verifying_key().to_bytes();

    let blob = ssh_ed25519_public_blob(&public);
    let public_key = format!(
        "ssh-ed25519 {} rsctf",
        base64::engine::general_purpose::STANDARD.encode(&blob)
    );
    let fingerprint = ssh_fingerprint(&blob);
    let private_key = encode_openssh_ed25519_private(&public, &seed, "rsctf");
    GeneratedSshKey {
        public_key,
        private_key,
        fingerprint,
    }
}

/// SSH wire encoding of an Ed25519 public key: string("ssh-ed25519") + string(pub).
fn ssh_ed25519_public_blob(public: &[u8]) -> Vec<u8> {
    let mut blob = Vec::new();
    push_ssh_field(&mut blob, b"ssh-ed25519");
    push_ssh_field(&mut blob, public);
    blob
}

/// Append an SSH wire field: 4-byte big-endian length prefix + bytes.
fn push_ssh_field(out: &mut Vec<u8>, data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(data);
}

/// OpenSSH-canonical fingerprint `SHA256:<unpadded-base64(sha256(blob))>` — matches
/// `ssh-keygen -lf` (RSCTF `AdSshKeyUtils.Fingerprint`).
fn ssh_fingerprint(blob: &[u8]) -> String {
    let digest = Sha256::digest(blob);
    format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest)
    )
}

/// Encode an unencrypted OpenSSH private key (PROTOCOL.key, cipher/kdf "none") for
/// an Ed25519 key from its 32-byte public key + 32-byte seed. Faithful port of
/// RSCTF `AdSshKeyUtils.EncodeOpenSshEd25519Private`, so the file drops straight
/// into `~/.ssh/` and works with stock `ssh`.
fn encode_openssh_ed25519_private(public: &[u8], seed: &[u8], comment: &str) -> String {
    let pub_blob = ssh_ed25519_public_blob(public);

    let mut priv_section = Vec::new();
    let mut check = [0u8; 4];
    fill_random(&mut check);
    priv_section.extend_from_slice(&check);
    priv_section.extend_from_slice(&check); // check1 == check2 (integrity marker)
    push_ssh_field(&mut priv_section, b"ssh-ed25519");
    push_ssh_field(&mut priv_section, public);
    // The OpenSSH ed25519 "private key" is seed || pub (64 bytes total).
    let mut seed_plus_pub = Vec::with_capacity(seed.len() + public.len());
    seed_plus_pub.extend_from_slice(seed);
    seed_plus_pub.extend_from_slice(public);
    push_ssh_field(&mut priv_section, &seed_plus_pub);
    push_ssh_field(&mut priv_section, comment.as_bytes());
    // Pad to an 8-byte boundary with 1, 2, 3, …
    let pad = (8 - (priv_section.len() % 8)) % 8;
    for i in 0..pad {
        priv_section.push((i + 1) as u8);
    }

    let mut body = Vec::new();
    body.extend_from_slice(b"openssh-key-v1\0");
    push_ssh_field(&mut body, b"none"); // ciphername
    push_ssh_field(&mut body, b"none"); // kdfname
    push_ssh_field(&mut body, b""); // kdfopts (empty)
    body.extend_from_slice(&1u32.to_be_bytes()); // number of keys
    push_ssh_field(&mut body, &pub_blob);
    push_ssh_field(&mut body, &priv_section);

    let b64 = base64::engine::general_purpose::STANDARD.encode(&body);
    let mut out = String::from("-----BEGIN OPENSSH PRIVATE KEY-----\n"); // gitleaks:allow
    for chunk in b64.as_bytes().chunks(70) {
        out.push_str(std::str::from_utf8(chunk).expect("base64 output is ASCII"));
        out.push('\n');
    }
    out.push_str("-----END OPENSSH PRIVATE KEY-----\n");
    out
}

/// Parse a single-line OpenSSH public key (`algorithm base64 [comment]`) and
/// return `(algorithm, fingerprint)`. Decodes the base64 wire blob and computes
/// its SHA256 fingerprint (the validation slice of RSCTF `AdSshKeyUtils.Parse`).
fn parse_ssh_public_key(input: &str) -> AppResult<(String, String)> {
    let mut parts = input.split_whitespace();
    let algorithm = parts
        .next()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| AppError::bad_request("Public key must be 'algorithm base64 [comment]'"))?;
    let b64 = parts
        .next()
        .ok_or_else(|| AppError::bad_request("Public key must be 'algorithm base64 [comment]'"))?;
    let blob = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|_| AppError::bad_request("Public key blob is not valid base64"))?;
    if blob.len() < 32 || blob.len() > 4096 {
        return Err(AppError::bad_request(
            "Public key blob has implausible length",
        ));
    }
    Ok((algorithm.to_string(), ssh_fingerprint(&blob)))
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::fmt;

    use sqlx::error::{DatabaseError, ErrorKind};

    use super::*;

    #[derive(Debug)]
    struct TestDatabaseError {
        code: &'static str,
        constraint: Option<&'static str>,
    }

    impl fmt::Display for TestDatabaseError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("test database error")
        }
    }

    impl std::error::Error for TestDatabaseError {}

    impl DatabaseError for TestDatabaseError {
        fn message(&self) -> &str {
            "test database error"
        }

        fn code(&self) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed(self.code))
        }

        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }

        fn constraint(&self) -> Option<&str> {
            self.constraint
        }

        fn kind(&self) -> ErrorKind {
            if self.code == "23505" {
                ErrorKind::UniqueViolation
            } else {
                ErrorKind::Other
            }
        }
    }

    fn database_error(code: &'static str, constraint: Option<&'static str>) -> sqlx::Error {
        sqlx::Error::Database(Box::new(TestDatabaseError { code, constraint }))
    }

    #[test]
    fn fingerprint_constraint_returns_safe_conflict() {
        let error = map_ssh_key_write_error(database_error("23505", Some(SSH_FINGERPRINT_INDEX)));

        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert_eq!(
            error.to_string(),
            "This SSH public key is already registered"
        );
    }

    #[test]
    fn unrelated_database_error_remains_internal() {
        let error =
            map_ssh_key_write_error(database_error("23505", Some("ux_adsshkeys_participation")));

        assert!(matches!(error, AppError::Internal(_)));
    }
}
