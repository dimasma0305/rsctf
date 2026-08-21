//! One-time browser-fingerprint challenge issuance and validation.

use std::collections::BTreeMap;

use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::Sha256;

use super::{database_error, hash_value, PolicyFlags, IDENTITY_HASH_DOMAIN};
use crate::app_state::SharedState;
use crate::models::internal::configs::AppConfig;
use crate::utils::error::{AppError, AppResult};

const CHALLENGE_TTL_SECONDS: i64 = 120;
const MAX_PROOF_BYTES: usize = 16 * 1024;
const REQUIRED_SIGNALS: &[&str] = &[
    "lie_count",
    "headless_rating",
    "platform_consistent",
    "ua_consistent",
    "webgl_consistent",
];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FingerprintProof {
    version: u8,
    fingerprint: String,
    nonce: String,
    signal_order: Vec<String>,
    signals: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FingerprintChallenge {
    pub nonce: String,
    pub required_signals: Vec<String>,
    pub expires_in_seconds: i32,
}

fn challenge_signature(config: &AppConfig, token: &str) -> Vec<u8> {
    hash_value(
        config.identity_hash_key.as_bytes(),
        "FingerprintChallengeNonce",
        token,
    )
}

fn signed_challenge_nonce(config: &AppConfig, token: &str) -> String {
    let signature =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(challenge_signature(config, token));
    format!("{token}.{signature}")
}

fn verified_challenge_hash(config: &AppConfig, nonce: &str) -> Option<Vec<u8>> {
    let (token, encoded_signature) = nonce.rsplit_once('.')?;
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded_signature)
        .ok()?;
    let mut mac = Hmac::<Sha256>::new_from_slice(config.identity_hash_key.as_bytes()).ok()?;
    mac.update(IDENTITY_HASH_DOMAIN);
    mac.update(b"FingerprintChallengeNonce");
    mac.update(b"\0");
    mac.update(token.as_bytes());
    mac.verify_slice(&signature).ok()?;
    Some(signature)
}

fn validate_required_signal(name: &str, value: &str) -> bool {
    match name {
        "lie_count" => value.parse::<u32>().is_ok(),
        "headless_rating" => value.parse::<u8>().is_ok_and(|rating| rating <= 100),
        "platform_consistent" | "ua_consistent" | "webgl_consistent" => {
            matches!(value, "0" | "1")
        }
        _ => false,
    }
}

fn validate_proof_fields(
    proof: &FingerprintProof,
    fingerprint: &str,
    expected_signals: &[String],
) -> bool {
    proof.version == 1
        && proof.fingerprint == fingerprint
        && proof.signal_order == expected_signals
        && proof.signals.len() == expected_signals.len()
        && expected_signals.iter().all(|signal| {
            proof
                .signals
                .get(signal)
                .is_some_and(|value| validate_required_signal(signal, value))
        })
}

fn proof_required(policy: PolicyFlags, proof_present: bool) -> bool {
    policy.fingerprint_required() || proof_present
}

pub(super) async fn consume_challenge(
    pool: &sqlx::PgPool,
    nonce_hash: &[u8],
) -> AppResult<Vec<String>> {
    sqlx::query_scalar::<_, Vec<String>>(
        r#"UPDATE "FingerprintChallenges"
              SET consumed_at_utc = NOW()
            WHERE nonce_hash = $1
              AND consumed_at_utc IS NULL
              AND expires_at_utc >= NOW()
        RETURNING required_signals"#,
    )
    .bind(nonce_hash)
    .fetch_optional(pool)
    .await
    .map_err(database_error)?
    .ok_or_else(|| AppError::bad_request("Fingerprint challenge expired or was reused."))
}

/// Issue a signed, server-stored, one-time challenge. This proves freshness and
/// payload consistency; it does not make a browser fingerprint an unforgeable
/// device identifier.
pub async fn issue_fingerprint_challenge(st: &SharedState) -> AppResult<FingerprintChallenge> {
    let token = crate::utils::codec::random_token(32);
    let nonce = signed_challenge_nonce(st.config.as_ref(), &token);
    let required_signals = REQUIRED_SIGNALS
        .iter()
        .map(|signal| (*signal).to_string())
        .collect::<Vec<_>>();
    let nonce_hash = challenge_signature(st.config.as_ref(), &token);
    sqlx::query(
        r#"WITH cleanup AS (
               DELETE FROM "FingerprintChallenges"
                WHERE expires_at_utc < NOW() - INTERVAL '1 day'
           )
           INSERT INTO "FingerprintChallenges"
                (nonce_hash, required_signals, created_at_utc, expires_at_utc,
                 consumed_at_utc)
           VALUES ($1, $2, NOW(), NOW() + INTERVAL '120 seconds', NULL)"#,
    )
    .bind(&nonce_hash)
    .bind(&required_signals)
    .execute(st.pg())
    .await
    .map_err(database_error)?;
    Ok(FingerprintChallenge {
        nonce,
        required_signals,
        expires_in_seconds: CHALLENGE_TTL_SECONDS as i32,
    })
}

/// Validate and atomically consume a fingerprint proof. A submitted
/// fingerprint is never retained unless it is well formed and bound to a live
/// one-time server challenge.
pub async fn validate_fingerprint_submission(
    st: &SharedState,
    policy: PolicyFlags,
    fingerprint: Option<&str>,
    proof_json: Option<&str>,
) -> AppResult<Option<String>> {
    let fingerprint = fingerprint.map(str::trim).filter(|value| !value.is_empty());
    let proof_json = proof_json.map(str::trim).filter(|value| !value.is_empty());
    // Backwards compatibility: old clients may still send the legacy raw field
    // while collection is disabled. Ignore it unless a proof is also supplied;
    // unproved values are never persisted.
    if !proof_required(policy, proof_json.is_some()) {
        return Ok(None);
    }
    let fingerprint = fingerprint
        .filter(|value| super::valid_browser_fingerprint(value))
        .ok_or_else(|| AppError::bad_request("A valid browser fingerprint is required."))?;
    let proof_json = proof_json
        .filter(|value| value.len() <= MAX_PROOF_BYTES)
        .ok_or_else(|| AppError::bad_request("A valid fingerprint proof is required."))?;
    let proof: FingerprintProof = serde_json::from_str(proof_json)
        .map_err(|_| AppError::bad_request("A valid fingerprint proof is required."))?;
    let nonce_hash = verified_challenge_hash(st.config.as_ref(), &proof.nonce)
        .ok_or_else(|| AppError::bad_request("A valid fingerprint proof is required."))?;
    let required_signals = consume_challenge(st.pg(), &nonce_hash).await?;
    if !validate_proof_fields(&proof, fingerprint, &required_signals) {
        return Err(AppError::bad_request(
            "Fingerprint proof does not match the submitted fingerprint.",
        ));
    }
    Ok(Some(fingerprint.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn proof(fingerprint: &str) -> FingerprintProof {
        let signal_order = REQUIRED_SIGNALS
            .iter()
            .map(|signal| (*signal).to_string())
            .collect::<Vec<_>>();
        let signals = BTreeMap::from([
            ("lie_count".to_string(), "0".to_string()),
            ("headless_rating".to_string(), "0".to_string()),
            ("platform_consistent".to_string(), "1".to_string()),
            ("ua_consistent".to_string(), "1".to_string()),
            ("webgl_consistent".to_string(), "1".to_string()),
        ]);
        FingerprintProof {
            version: 1,
            fingerprint: fingerprint.to_string(),
            nonce: "unused".to_string(),
            signal_order,
            signals,
        }
    }

    #[test]
    fn proof_is_bound_to_fingerprint_and_required_signal_order() {
        let required = REQUIRED_SIGNALS
            .iter()
            .map(|signal| (*signal).to_string())
            .collect::<Vec<_>>();
        assert!(validate_proof_fields(&proof(FP), FP, &required));
        assert!(!validate_proof_fields(
            &proof("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            FP,
            &required
        ));
        let mut reordered = required.clone();
        reordered.swap(0, 1);
        assert!(!validate_proof_fields(&proof(FP), FP, &reordered));
    }

    #[test]
    fn proof_rejects_invalid_signal_values() {
        let required = REQUIRED_SIGNALS
            .iter()
            .map(|signal| (*signal).to_string())
            .collect::<Vec<_>>();
        let mut invalid = proof(FP);
        invalid
            .signals
            .insert("headless_rating".to_string(), "101".to_string());
        assert!(!validate_proof_fields(&invalid, FP, &required));
    }

    #[test]
    fn disabled_policy_does_not_require_legacy_unproved_fingerprint() {
        let policy = PolicyFlags::default();
        assert!(!proof_required(policy, false));
        assert!(proof_required(policy, true));
        assert!(proof_required(
            PolicyFlags {
                enable_browser_fingerprint: true,
                ..PolicyFlags::default()
            },
            false
        ));
    }
}
