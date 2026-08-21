use base64::Engine;
use chrono::Utc;
use hmac::{Hmac, KeyInit, Mac};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::utils::error::{AppError, AppResult};

pub const VPN_PROOF_HEADER: &str = "x-rsctf-vpn-proof";
pub const CHALLENGE_TTL_SECONDS: i64 = 60;
pub const PROOF_TTL_SECONDS: i64 = 30;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VpnChallengeClaims {
    pub purpose: String,
    pub user_id: Uuid,
    pub game_id: i32,
    pub participation_id: i32,
    pub security_stamp_hash: String,
    pub nonce: Uuid,
    pub issued_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VpnProofClaims {
    pub purpose: String,
    pub user_id: Uuid,
    pub game_id: i32,
    pub participation_id: i32,
    pub peer_id: Uuid,
    pub peer_generation: i32,
    pub policy_revision: i64,
    pub security_stamp_hash: String,
    pub issued_at: i64,
    pub expires_at: i64,
}

fn signing_key(secret: &str, purpose: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"rsctf:event-vpn:v1\0");
    digest.update(purpose);
    digest.update(b"\0");
    digest.update(secret.as_bytes());
    digest.finalize().into()
}

pub fn stamp_hash(stamp: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"rsctf:event-vpn:stamp:v1\0");
    digest.update(stamp.as_bytes());
    hex::encode(digest.finalize())
}

fn encode<T: Serialize>(secret: &str, purpose: &[u8], claims: &T) -> AppResult<String> {
    super::validate_credential_key(secret)?;
    let payload = serde_json::to_vec(claims)
        .map_err(|error| AppError::internal(format!("encode VPN proof: {error}")))?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload);
    let mut mac = Hmac::<Sha256>::new_from_slice(&signing_key(secret, purpose))
        .map_err(|_| AppError::internal("initialize VPN proof signer"))?;
    mac.update(payload.as_bytes());
    let signature =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    Ok(format!("{payload}.{signature}"))
}

fn decode<T: DeserializeOwned>(secret: &str, purpose: &[u8], token: &str) -> AppResult<T> {
    super::validate_credential_key(secret)?;
    if token.len() > 4096 {
        return Err(AppError::Unauthorized);
    }
    let (payload, signature) = token.split_once('.').ok_or(AppError::Unauthorized)?;
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| AppError::Unauthorized)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&signing_key(secret, purpose))
        .map_err(|_| AppError::Unauthorized)?;
    mac.update(payload.as_bytes());
    mac.verify_slice(&signature)
        .map_err(|_| AppError::Unauthorized)?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| AppError::Unauthorized)?;
    serde_json::from_slice(&payload).map_err(|_| AppError::Unauthorized)
}

pub fn issue_challenge(
    secret: &str,
    user_id: Uuid,
    game_id: i32,
    participation_id: i32,
    security_stamp: &str,
) -> AppResult<(String, VpnChallengeClaims)> {
    let now = Utc::now().timestamp();
    let claims = VpnChallengeClaims {
        purpose: "challenge".to_string(),
        user_id,
        game_id,
        participation_id,
        security_stamp_hash: stamp_hash(security_stamp),
        nonce: Uuid::new_v4(),
        issued_at: now,
        expires_at: now + CHALLENGE_TTL_SECONDS,
    };
    Ok((encode(secret, b"challenge", &claims)?, claims))
}

pub fn verify_challenge(secret: &str, token: &str) -> AppResult<VpnChallengeClaims> {
    let claims: VpnChallengeClaims = decode(secret, b"challenge", token)?;
    let now = Utc::now().timestamp();
    if claims.purpose != "challenge"
        || claims.issued_at > now + 5
        || claims.expires_at < now
        || claims.expires_at - claims.issued_at != CHALLENGE_TTL_SECONDS
    {
        return Err(AppError::Unauthorized);
    }
    Ok(claims)
}

#[allow(clippy::too_many_arguments)]
pub fn issue_proof(
    secret: &str,
    user_id: Uuid,
    game_id: i32,
    participation_id: i32,
    peer_id: Uuid,
    peer_generation: i32,
    policy_revision: i64,
    security_stamp_hash: String,
) -> AppResult<(String, VpnProofClaims)> {
    let now = Utc::now().timestamp();
    let claims = VpnProofClaims {
        purpose: "proof".to_string(),
        user_id,
        game_id,
        participation_id,
        peer_id,
        peer_generation,
        policy_revision,
        security_stamp_hash,
        issued_at: now,
        expires_at: now + PROOF_TTL_SECONDS,
    };
    Ok((encode(secret, b"proof", &claims)?, claims))
}

pub fn verify_proof(secret: &str, token: &str) -> AppResult<VpnProofClaims> {
    let claims: VpnProofClaims = decode(secret, b"proof", token)?;
    let now = Utc::now().timestamp();
    if claims.purpose != "proof"
        || claims.issued_at > now + 5
        || claims.expires_at < now
        || claims.expires_at - claims.issued_at != PROOF_TTL_SECONDS
    {
        return Err(AppError::Unauthorized);
    }
    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "event-vpn-test-key-0123456789abcdef";

    #[test]
    fn tokens_are_purpose_bound_short_lived_and_tamper_evident() {
        let user = Uuid::new_v4();
        let (challenge, challenge_claims) = issue_challenge(KEY, user, 7, 9, "stamp").unwrap();
        assert_eq!(
            verify_challenge(KEY, &challenge).unwrap().nonce,
            challenge_claims.nonce
        );
        assert!(verify_proof(KEY, &challenge).is_err());
        let mut tampered = challenge.into_bytes();
        tampered[4] ^= 1;
        assert!(verify_challenge(KEY, &String::from_utf8(tampered).unwrap()).is_err());

        let (proof, claims) =
            issue_proof(KEY, user, 7, 9, Uuid::new_v4(), 2, 4, stamp_hash("stamp")).unwrap();
        assert_eq!(verify_proof(KEY, &proof).unwrap().peer_id, claims.peer_id);
    }
}
