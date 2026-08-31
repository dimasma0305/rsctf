use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request};
use sha2::{Digest, Sha256};

use super::Policy;

/// The client IP for partitioning, from sources a client cannot forge past a
/// trusted reverse proxy: `X-Real-IP`, else the rightmost `X-Forwarded-For`
/// hop, else the raw transport peer address.
fn client_ip(req: &Request) -> String {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    crate::services::anti_cheat::client_ip(req.headers(), peer)
        .unwrap_or_else(|| "unknown".to_string())
}

/// The fixed-size partition derived by global authentication and reused by
/// named route policies without hashing the same claims twice.
#[derive(Clone)]
pub(super) struct VerifiedSessionPartitionKey(pub(super) String);

pub(super) fn partition_key(policy: Policy, req: &Request) -> String {
    if matches!(
        policy,
        Policy::PowIssuanceGlobal | Policy::TeamSignatureGlobal | Policy::EventVpnMintGlobal
    ) {
        return match policy {
            Policy::PowIssuanceGlobal => "hashpow-issuance-global",
            Policy::TeamSignatureGlobal => "team-signature-global",
            Policy::EventVpnMintGlobal => "event-vpn-mint-global",
            _ => unreachable!(),
        }
        .to_string();
    }
    // Anonymous-facing credential and handshake budgets remain source scoped
    // even when a verified session extension is already present.
    if matches!(
        policy,
        Policy::Login
            | Policy::Register
            | Policy::GlobalIpBackstop
            | Policy::CredentialIpAdmission
            | Policy::PrivilegedHubAdmission
            | Policy::PublicHubAdmission
            | Policy::PowIssuanceSource
            | Policy::TeamSignatureSource
    ) {
        return client_ip(req);
    }
    if let Some(credential) = req
        .extensions()
        .get::<crate::services::ad::api_token::VerifiedTeamToken>()
    {
        return credential.partition_key.clone();
    }
    if let Some(key) = req.extensions().get::<VerifiedSessionPartitionKey>() {
        return key.0.clone();
    }
    req.extensions()
        .get::<crate::middlewares::privilege_authentication::VerifiedSessionClaims>()
        .map(|claims| session_partition_key(&claims.0))
        .unwrap_or_else(|| client_ip(req))
}

/// Fixed-size identity for a signed session. Binding the live-revocation stamp
/// creates a fresh budget generation after credential rotation without putting
/// account identifiers or attacker-sized claim strings in Redis keys.
pub(super) fn session_partition_key(claims: &crate::services::token::Claims) -> String {
    let mut digest = Sha256::new();
    digest.update(b"rsctf-rate-session-v1\0");
    digest.update((claims.sub.len() as u64).to_be_bytes());
    digest.update(claims.sub.as_bytes());
    digest.update((claims.stamp.len() as u64).to_be_bytes());
    digest.update(claims.stamp.as_bytes());
    format!("jwt:{}", hex::encode(digest.finalize()))
}
