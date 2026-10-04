//! Canonical source identities and encrypted transient Git credentials.

use aes_gcm::aead::consts::U12;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::utils::error::{AppError, AppResult};

const MAX_GIT_TOKEN_BYTES: usize = 4 * 1024;
pub(super) type EncryptedTokenParts = (Option<Vec<u8>>, Option<Vec<u8>>);

pub(super) fn normalized_github_url(raw: &str) -> AppResult<String> {
    let mut url =
        reqwest::Url::parse(raw).map_err(|_| AppError::bad_request("invalid repository URL"))?;
    url.set_query(None);
    let path = url.path().trim_end_matches('/').trim_end_matches(".git");
    let normalized_path = format!("{path}.git");
    url.set_path(&normalized_path);
    let normalized = url.to_string();
    if normalized.len() > 2_048 {
        return Err(AppError::bad_request(
            "repository URL may be at most 2048 bytes",
        ));
    }
    Ok(normalized)
}

pub(super) fn token_key(secret: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"rsctf:challenge-import:git-token:v1\0");
    digest.update(secret.as_bytes());
    digest.finalize().into()
}

pub(super) fn token_aad(job_id: Uuid, game_id: i32, actor_user_id: Uuid) -> Vec<u8> {
    format!("v1:{job_id}:{game_id}:{actor_user_id}").into_bytes()
}

pub(super) fn encrypt_token(
    secret: &str,
    job_id: Uuid,
    game_id: i32,
    actor_user_id: Uuid,
    token: &str,
) -> AppResult<EncryptedTokenParts> {
    if token.is_empty() {
        return Ok((None, None));
    }
    if token.len() > MAX_GIT_TOKEN_BYTES {
        return Err(AppError::bad_request(format!(
            "GitHub token may be at most {MAX_GIT_TOKEN_BYTES} bytes"
        )));
    }
    let cipher = Aes256Gcm::new_from_slice(&token_key(secret))
        .map_err(|_| AppError::internal("initialize import token encryption"))?;
    let nonce: [u8; 12] = rand::random();
    let nonce_value: Nonce<U12> = nonce.into();
    let ciphertext = cipher
        .encrypt(
            &nonce_value,
            Payload {
                msg: token.as_bytes(),
                aad: &token_aad(job_id, game_id, actor_user_id),
            },
        )
        .map_err(|_| AppError::internal("encrypt import token"))?;
    Ok((Some(ciphertext), Some(nonce.to_vec())))
}
