use aes_gcm::aead::consts::U12;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use sha2::{Digest, Sha256};

use super::*;

fn admin_reset_key(secret: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"rsctf:admin-password-reset:v1\0");
    digest.update(secret.as_bytes());
    digest.finalize().into()
}

fn admin_reset_aad(operation_id: Uuid, user_id: Uuid) -> Vec<u8> {
    format!("v1:{operation_id}:{user_id}").into_bytes()
}

pub(super) fn encrypt_admin_reset(
    secret: &str,
    operation_id: Uuid,
    user_id: Uuid,
    password: &str,
) -> AppResult<(Vec<u8>, [u8; 12])> {
    let cipher = Aes256Gcm::new_from_slice(&admin_reset_key(secret))
        .map_err(|_| AppError::internal("initialize admin reset encryption"))?;
    let nonce: [u8; 12] = rand::random();
    let nonce_value: Nonce<U12> = nonce.into();
    let ciphertext = cipher
        .encrypt(
            &nonce_value,
            Payload {
                msg: password.as_bytes(),
                aad: &admin_reset_aad(operation_id, user_id),
            },
        )
        .map_err(|_| AppError::internal("encrypt admin reset result"))?;
    Ok((ciphertext, nonce))
}

pub(super) fn decrypt_admin_reset(
    secret: &str,
    operation_id: Uuid,
    user_id: Uuid,
    ciphertext: &[u8],
    nonce: &[u8],
) -> AppResult<String> {
    let cipher = Aes256Gcm::new_from_slice(&admin_reset_key(secret))
        .map_err(|_| AppError::internal("initialize admin reset encryption"))?;
    let nonce = Nonce::<U12>::try_from(nonce)
        .map_err(|_| AppError::internal("invalid admin reset nonce"))?;
    let plaintext = cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad: &admin_reset_aad(operation_id, user_id),
            },
        )
        .map_err(|_| AppError::unavailable("Password reset result cannot be recovered"))?;
    String::from_utf8(plaintext).map_err(|_| AppError::internal("invalid admin reset result"))
}
