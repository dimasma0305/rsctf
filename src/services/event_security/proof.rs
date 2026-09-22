use base64::Engine;
use chrono::Utc;
use hmac::{Hmac, KeyInit, Mac};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::utils::error::{AppError, AppResult};

pub const VPN_PROOF_HEADER: &str = "x-rsctf-vpn-proof";
/// Cookie carrying a short-lived attachment download grant. A browser download
/// cannot attach the proof header, so the grant travels as an `HttpOnly`
/// cookie scoped to the exact `/assets/{hash}` path instead of the URL.
pub const ASSET_GRANT_COOKIE: &str = "RSCTF_AssetGrant";
pub const CHALLENGE_TTL_SECONDS: i64 = 60;
pub const PROOF_TTL_SECONDS: i64 = 30;
/// Long enough for a browser to start a large download after one click; the
/// live peer, roster, stamp, and policy revision are still rechecked on every
/// request, so expiry only bounds how long an idle grant remains usable.
pub const ASSET_GRANT_TTL_SECONDS: i64 = 300;

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

/// A proof re-scoped to one content hash for a browser-native download. The
/// asset route accepts it only as transport evidence: every ownership, roster,
/// division, and monitor rule stays exactly where it is today.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VpnAssetGrantClaims {
    pub purpose: String,
    pub user_id: Uuid,
    pub game_id: i32,
    pub participation_id: i32,
    pub content_hash: String,
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

pub fn issue_asset_grant(
    secret: &str,
    proof: &VpnProofClaims,
    content_hash: &str,
) -> AppResult<(String, VpnAssetGrantClaims)> {
    let now = Utc::now().timestamp();
    let claims = VpnAssetGrantClaims {
        purpose: "asset-grant".to_string(),
        user_id: proof.user_id,
        game_id: proof.game_id,
        participation_id: proof.participation_id,
        content_hash: content_hash.to_ascii_lowercase(),
        peer_id: proof.peer_id,
        peer_generation: proof.peer_generation,
        policy_revision: proof.policy_revision,
        security_stamp_hash: proof.security_stamp_hash.clone(),
        issued_at: now,
        expires_at: now + ASSET_GRANT_TTL_SECONDS,
    };
    Ok((encode(secret, b"asset-grant", &claims)?, claims))
}

pub fn verify_asset_grant(secret: &str, token: &str) -> AppResult<VpnAssetGrantClaims> {
    let claims: VpnAssetGrantClaims = decode(secret, b"asset-grant", token)?;
    let now = Utc::now().timestamp();
    if claims.purpose != "asset-grant"
        || claims.issued_at > now + 5
        || claims.expires_at < now
        || claims.expires_at - claims.issued_at != ASSET_GRANT_TTL_SECONDS
    {
        return Err(AppError::Unauthorized);
    }
    Ok(claims)
}

/// `Set-Cookie` value for one download grant. The path binds the cookie to
/// both asset routes of exactly this hash, and `SameSite=Strict` keeps a
/// cross-site page from triggering a grant-bearing download.
pub fn asset_grant_cookie(token: &str, content_hash: &str, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!(
        "{ASSET_GRANT_COOKIE}={token}; Path=/assets/{content_hash}; HttpOnly; SameSite=Strict{secure}; Max-Age={ASSET_GRANT_TTL_SECONDS}"
    )
}

/// Grant tokens presented in a `Cookie` header, bounded so a hostile client
/// cannot make the asset route verify an unbounded number of signatures.
pub fn asset_grant_cookie_values(cookies: &str) -> impl Iterator<Item = &str> {
    cookies
        .split(';')
        .filter_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name == ASSET_GRANT_COOKIE && !value.is_empty()).then_some(value)
        })
        .take(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "event-vpn-test-key-0123456789abcdef";

    fn proof_claims() -> VpnProofClaims {
        let now = Utc::now().timestamp();
        VpnProofClaims {
            purpose: "proof".to_string(),
            user_id: Uuid::new_v4(),
            game_id: 7,
            participation_id: 9,
            peer_id: Uuid::new_v4(),
            peer_generation: 2,
            policy_revision: 4,
            security_stamp_hash: stamp_hash("stamp"),
            issued_at: now,
            expires_at: now + PROOF_TTL_SECONDS,
        }
    }

    #[test]
    fn asset_grants_inherit_the_proof_subject_for_one_hash() {
        let proof = proof_claims();
        let hash = "A".repeat(64);
        let (token, claims) = issue_asset_grant(KEY, &proof, &hash).unwrap();
        let verified = verify_asset_grant(KEY, &token).unwrap();
        assert_eq!(verified.content_hash, "a".repeat(64));
        assert_eq!(verified.peer_id, proof.peer_id);
        assert_eq!(verified.peer_generation, 2);
        assert_eq!(verified.policy_revision, 4);
        assert_eq!(verified.participation_id, 9);
        assert_eq!(verified.security_stamp_hash, proof.security_stamp_hash);
        assert_eq!(
            claims.expires_at - claims.issued_at,
            ASSET_GRANT_TTL_SECONDS
        );
    }

    #[test]
    fn asset_grants_are_purpose_bound_tamper_evident_and_expire() {
        let proof = proof_claims();
        let (grant, _) = issue_asset_grant(KEY, &proof, &"b".repeat(64)).unwrap();
        // A proof or challenge token is never a download grant and vice versa.
        let (proof_token, _) = issue_proof(
            KEY,
            proof.user_id,
            7,
            9,
            proof.peer_id,
            2,
            4,
            proof.security_stamp_hash.clone(),
        )
        .unwrap();
        assert!(verify_asset_grant(KEY, &proof_token).is_err());
        assert!(verify_proof(KEY, &grant).is_err());
        assert!(verify_asset_grant("another-secret-0123456789abcdefghij", &grant).is_err());

        let mut tampered = grant.clone().into_bytes();
        tampered[6] ^= 1;
        assert!(verify_asset_grant(KEY, &String::from_utf8(tampered).unwrap()).is_err());

        let expired = VpnAssetGrantClaims {
            issued_at: Utc::now().timestamp() - ASSET_GRANT_TTL_SECONDS - 10,
            expires_at: Utc::now().timestamp() - 10,
            ..verify_asset_grant(KEY, &grant).unwrap()
        };
        let expired = encode(KEY, b"asset-grant", &expired).unwrap();
        assert!(verify_asset_grant(KEY, &expired).is_err());

        let stretched = VpnAssetGrantClaims {
            expires_at: Utc::now().timestamp() + 86_400,
            ..verify_asset_grant(KEY, &grant).unwrap()
        };
        let stretched = encode(KEY, b"asset-grant", &stretched).unwrap();
        assert!(verify_asset_grant(KEY, &stretched).is_err());
    }

    #[test]
    fn grant_cookie_is_path_scoped_http_only_and_parsed_boundedly() {
        let hash = "c".repeat(64);
        let cookie = asset_grant_cookie("token.sig", &hash, true);
        assert!(cookie.starts_with("RSCTF_AssetGrant=token.sig; "));
        assert!(cookie.contains(&format!("; Path=/assets/{hash}")));
        assert!(cookie.contains("; HttpOnly"));
        assert!(cookie.contains("; SameSite=Strict"));
        assert!(cookie.contains("; Secure"));
        assert!(cookie.ends_with("; Max-Age=300"));
        assert!(!asset_grant_cookie("token.sig", &hash, false).contains("Secure"));

        let header = "RSCTF_Token=session; RSCTF_AssetGrant=; RSCTF_AssetGrant=one; \
                      other=x; RSCTF_AssetGrant=two; RSCTF_AssetGrant=three; \
                      RSCTF_AssetGrant=four; RSCTF_AssetGrant=five";
        assert_eq!(
            asset_grant_cookie_values(header).collect::<Vec<_>>(),
            vec!["one", "two", "three", "four"]
        );
    }

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
