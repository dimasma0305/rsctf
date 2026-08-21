//! Shared validation for deterministic challenge variants and solve receipts.

use crate::models::internal::configs::AppConfig;
use crate::utils::enums::{ChallengeType, ChallengeVariantMode, SolveReceiptMode};
use crate::utils::error::{AppError, AppResult};

pub fn validate_challenge_provenance_policy(
    challenge_type: ChallengeType,
    variant_mode: ChallengeVariantMode,
    generator_image: Option<&str>,
    generator_digest: Option<&str>,
    receipt_mode: SolveReceiptMode,
    verifier_identity: Option<&str>,
    config: &AppConfig,
) -> AppResult<()> {
    validate_challenge_provenance_modes(
        challenge_type,
        variant_mode,
        receipt_mode,
        verifier_identity,
        config,
    )?;
    validate_generator_reference(variant_mode, generator_image, generator_digest)
}

pub(crate) fn validate_challenge_provenance_modes(
    challenge_type: ChallengeType,
    variant_mode: ChallengeVariantMode,
    receipt_mode: SolveReceiptMode,
    verifier_identity: Option<&str>,
    config: &AppConfig,
) -> AppResult<()> {
    if challenge_type.uses_ad_engine()
        && (variant_mode != ChallengeVariantMode::Disabled
            || receipt_mode != SolveReceiptMode::Disabled)
    {
        return Err(AppError::bad_request(
            "Challenge variants and solve receipts apply only to Jeopardy challenges",
        ));
    }
    if variant_mode == ChallengeVariantMode::PerParticipation {
        super::validate_credential_key(&config.event_vpn_credential_key)?;
    }
    if receipt_mode != SolveReceiptMode::Disabled
        && !verifier_identity.is_some_and(|identity| (1..=128).contains(&identity.len()))
    {
        return Err(AppError::bad_request(
            "Solve receipts require a 1 to 128 character verifier identity",
        ));
    }
    if receipt_mode != SolveReceiptMode::Disabled
        && (config.solve_receipt_issuer_token.len() < 32
            || config
                .solve_receipt_issuer_token
                .chars()
                .any(char::is_whitespace))
    {
        return Err(AppError::bad_request(
            "Solve receipts require RSCTF_SOLVE_RECEIPT_ISSUER_TOKEN",
        ));
    }
    Ok(())
}

fn validate_generator_reference(
    variant_mode: ChallengeVariantMode,
    generator_image: Option<&str>,
    generator_digest: Option<&str>,
) -> AppResult<()> {
    match (generator_image, generator_digest) {
        (None, None) if variant_mode == ChallengeVariantMode::Disabled => {}
        (Some(image), Some(digest))
            if crate::services::challenge_images::is_repository_digest(image)
                && image.ends_with(digest)
                && digest.starts_with("sha256:")
                && digest.len() == 71
                && digest[7..]
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) => {}
        _ => {
            return Err(AppError::bad_request(
                "Variant generation requires one matching immutable image@sha256 digest and digest field",
            ));
        }
    }
    if variant_mode == ChallengeVariantMode::PerParticipation
        && (generator_image.is_none() || generator_digest.is_none())
    {
        return Err(AppError::bad_request(
            "Per-participation variants require a generator image",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured() -> AppConfig {
        let mut config = AppConfig::default();
        config.event_vpn_credential_key = "variant-seed-key-0123456789abcdef".to_string();
        config.solve_receipt_issuer_token = "receipt-issuer-key-0123456789abcdef".to_string();
        config
    }

    #[test]
    fn accepts_a_pinned_jeopardy_generator_and_receipt_issuer() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let image = format!("ghcr.io/example/generator@{digest}");
        assert!(validate_challenge_provenance_policy(
            ChallengeType::StaticAttachment,
            ChallengeVariantMode::PerParticipation,
            Some(&image),
            Some(&digest),
            SolveReceiptMode::Required,
            Some("example-verifier-v1"),
            &configured(),
        )
        .is_ok());
    }

    #[test]
    fn rejects_mutable_images_and_ad_engine_challenges() {
        assert!(validate_challenge_provenance_policy(
            ChallengeType::StaticAttachment,
            ChallengeVariantMode::PerParticipation,
            Some("ghcr.io/example/generator:latest"),
            Some("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            SolveReceiptMode::Disabled,
            None,
            &configured(),
        )
        .is_err());
        assert!(validate_challenge_provenance_policy(
            ChallengeType::AttackDefense,
            ChallengeVariantMode::Disabled,
            None,
            None,
            SolveReceiptMode::Required,
            Some("example-verifier-v1"),
            &configured(),
        )
        .is_err());
    }
}
