//! Password hashing, Ed25519 game-signature keys, and constant-time comparison.

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use ed25519_dalek::{Signer, SigningKey};

use crate::utils::codec::{base64_decode, base64_encode, random_bytes};
use crate::utils::error::AppError;

/// Generate a game signature keypair (Ed25519), base64-encoded — matching RSCTF's
/// `Game.PublicKey`/`PrivateKey` (a 32-byte seed as the private key + the derived
/// 32-byte public key), consumed by [`game_sign`] / `GameRepository.GetToken`.
pub fn generate_game_keypair() -> (String, String) {
    let seed: [u8; 32] = random_bytes(32)
        .try_into()
        .expect("random_bytes(32) yields 32 bytes");
    let sk = SigningKey::from_bytes(&seed);
    (
        base64_encode(sk.verifying_key().as_bytes()),
        base64_encode(&seed),
    )
}

/// Sign `data` with a game's base64 Ed25519 private key.
///
/// Invalid persisted key material is a data-integrity error. It must not silently
/// switch algorithms because the public verifier accepts Ed25519 signatures only.
pub fn game_sign(private_key_b64: &str, data: &str) -> Result<String, AppError> {
    let bytes = base64_decode(private_key_b64)
        .ok_or_else(|| AppError::internal("invalid game signing key encoding"))?;
    let seed = <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| AppError::internal("invalid game signing key length"))?;
    let signature = SigningKey::from_bytes(&seed).sign(data.as_bytes());
    Ok(base64_encode(&signature.to_bytes()))
}

/// Hash a password with Argon2id (RSCTF uses ASP.NET Identity's PBKDF2; this
/// port intentionally upgrades to Argon2).
pub fn hash_password(password: &str) -> Result<String, AppError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| AppError::internal(format!("password hash: {e}")))
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// Argon2id is memory-hard: a single hash/verify burns ~tens of ms of pure CPU.
/// On async handlers (login, register, password change/reset) that must run on a
/// blocking pool — inline on a tokio worker it parks that worker for the whole
/// hash, so a burst of logins/registrations (exactly what a CTF sees at kickoff)
/// starves the runtime and inflates every in-flight request's latency. These
/// wrappers offload to `spawn_blocking`; call them instead of the sync versions
/// from any async request path.
/// Cap concurrent Argon2 hashes/verifies. Each pins ~19 MiB while running; on the
/// default 512-thread blocking pool a register/login flood could run hundreds at once
/// (≈ several GiB of peak, and more retained arenas). Argon2 is CPU-bound, so bounding
/// to the core count costs no throughput and caps peak memory.
static ARGON2_GATE: std::sync::LazyLock<tokio::sync::Semaphore> = std::sync::LazyLock::new(|| {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    tokio::sync::Semaphore::new(cores)
});

pub async fn hash_password_async(password: String) -> Result<String, AppError> {
    let _permit = ARGON2_GATE.acquire().await;
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|e| AppError::internal(format!("password hash task: {e}")))?
}

pub async fn verify_password_async(password: String, hash: String) -> bool {
    let _permit = ARGON2_GATE.acquire().await;
    tokio::task::spawn_blocking(move || verify_password(&password, &hash))
        .await
        .unwrap_or(false)
}

/// Constant-time string comparison for flag/secret checking.
pub fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    use super::*;

    #[test]
    fn game_sign_uses_ed25519_and_rejects_invalid_keys() {
        let (public_key, private_key) = generate_game_keypair();
        let signature = game_sign(&private_key, "team-token").unwrap();

        let public_key: [u8; 32] = base64_decode(&public_key).unwrap().try_into().unwrap();
        let signature: [u8; 64] = base64_decode(&signature).unwrap().try_into().unwrap();
        assert!(VerifyingKey::from_bytes(&public_key)
            .unwrap()
            .verify(b"team-token", &Signature::from_bytes(&signature))
            .is_ok());
        assert!(game_sign("not-base64", "team-token").is_err());
        assert!(game_sign(&base64_encode(&[0_u8; 31]), "team-token").is_err());
    }
}
