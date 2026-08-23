//! Ported from RSCTF `Services/Token/TokenService.cs` — JWT issuing/verifying.

use chrono::{DateTime, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use sea_orm::ActiveEnum;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::utils::enums::Role;
use crate::utils::error::AppError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// User id (UUID string).
    pub sub: String,
    /// Numeric `Role` value.
    pub role: i16,
    /// User name, for convenience/audit.
    pub name: String,
    /// Identity security stamp at issuance. Changing credentials/logout rotates
    /// the database value, invalidating every previously issued session.
    pub stamp: String,
    pub iat: i64,
    pub exp: i64,
}

const PROXY_CAPABILITY_PURPOSE: &str = "rsctf-proxy-v1";
const PROXY_CAPABILITY_TTL_SECS: i64 = 2 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProxyCapabilityClaims {
    sub: String,
    stamp: String,
    container: String,
    preview: bool,
    purpose: String,
    iat: i64,
    exp: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProxyCapabilityIdentity {
    pub user_id: Uuid,
    pub security_stamp: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IssuedProxyCapability {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

pub struct TokenService {
    encoding: EncodingKey,
    decoding: DecodingKey,
    proxy_encoding: EncodingKey,
    proxy_decoding: DecodingKey,
    ttl_secs: i64,
}

impl TokenService {
    pub fn new(secret: &str, ttl_secs: i64) -> Self {
        // Keep proxy capabilities cryptographically separated from ordinary
        // login JWTs even though both derive from the deployment's one stable
        // secret. Their required claim shapes also differ, so neither token can
        // be reinterpreted as the other credential class.
        let mut proxy_key = Sha256::new();
        proxy_key.update(b"rsctf:proxy-capability:key:v1\0");
        proxy_key.update(secret.as_bytes());
        let proxy_key = proxy_key.finalize();
        Self {
            encoding: EncodingKey::from_secret(secret.as_bytes()),
            decoding: DecodingKey::from_secret(secret.as_bytes()),
            proxy_encoding: EncodingKey::from_secret(proxy_key.as_slice()),
            proxy_decoding: DecodingKey::from_secret(proxy_key.as_slice()),
            ttl_secs,
        }
    }

    pub fn issue(
        &self,
        id: Uuid,
        role: Role,
        name: &str,
        security_stamp: &str,
    ) -> Result<String, AppError> {
        let now = Utc::now().timestamp();
        let exp = now
            .checked_add(self.ttl_secs)
            .ok_or_else(|| AppError::internal("JWT expiry is outside the supported range"))?;
        let claims = Claims {
            sub: id.to_string(),
            role: role.into_value(),
            name: name.to_string(),
            stamp: security_stamp.to_string(),
            iat: now,
            exp,
        };
        encode(&Header::default(), &claims, &self.encoding)
            .map_err(|e| AppError::internal(format!("jwt encode: {e}")))
    }

    pub fn verify(&self, token: &str) -> Result<Claims, AppError> {
        decode::<Claims>(token, &self.decoding, &Validation::default())
            .map(|d| d.claims)
            .map_err(|_| AppError::Unauthorized)
    }

    pub(crate) fn issue_proxy_capability(
        &self,
        user_id: Uuid,
        security_stamp: &str,
        container_id: Uuid,
        preview: bool,
    ) -> Result<IssuedProxyCapability, AppError> {
        let now = Utc::now().timestamp();
        let exp = now.checked_add(PROXY_CAPABILITY_TTL_SECS).ok_or_else(|| {
            AppError::internal("proxy capability expiry is outside the supported range")
        })?;
        let claims = ProxyCapabilityClaims {
            sub: user_id.to_string(),
            stamp: security_stamp.to_owned(),
            container: container_id.to_string(),
            preview,
            purpose: PROXY_CAPABILITY_PURPOSE.to_owned(),
            iat: now,
            exp,
        };
        let token = encode(&Header::default(), &claims, &self.proxy_encoding)
            .map_err(|error| AppError::internal(format!("proxy capability encode: {error}")))?;
        let expires_at = DateTime::from_timestamp(exp, 0)
            .ok_or_else(|| AppError::internal("proxy capability expiry is invalid"))?;
        Ok(IssuedProxyCapability { token, expires_at })
    }

    pub(crate) fn verify_proxy_capability(
        &self,
        token: &str,
        container_id: Uuid,
        preview: bool,
    ) -> Result<ProxyCapabilityIdentity, AppError> {
        if token.len() > crate::middlewares::privilege_authentication::MAX_SESSION_TOKEN_BYTES {
            return Err(AppError::Unauthorized);
        }
        let claims =
            decode::<ProxyCapabilityClaims>(token, &self.proxy_decoding, &Validation::default())
                .map_err(|_| AppError::Unauthorized)?
                .claims;
        if claims.purpose != PROXY_CAPABILITY_PURPOSE
            || claims.preview != preview
            || Uuid::parse_str(&claims.container).ok() != Some(container_id)
        {
            return Err(AppError::Unauthorized);
        }
        let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
        if claims.stamp.is_empty() {
            return Err(AppError::Unauthorized);
        }
        Ok(ProxyCapabilityIdentity {
            user_id,
            security_stamp: claims.stamp,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_roundtrip_carries_security_stamp() {
        let service = TokenService::new("0123456789abcdef0123456789abcdef", 60);
        let id = Uuid::new_v4();
        let token = service.issue(id, Role::User, "alice", "stamp-1").unwrap();
        let claims = service.verify(&token).unwrap();
        assert_eq!(claims.sub, id.to_string());
        assert_eq!(claims.stamp, "stamp-1");
    }

    #[test]
    fn session_issue_rejects_expiry_overflow() {
        let service = TokenService::new("0123456789abcdef0123456789abcdef", i64::MAX);
        assert!(service
            .issue(Uuid::new_v4(), Role::User, "alice", "stamp-1")
            .is_err());
    }

    #[test]
    fn proxy_capability_is_exactly_bound_and_not_a_session() {
        let service = TokenService::new("0123456789abcdef0123456789abcdef", 60);
        let user_id = Uuid::new_v4();
        let container_id = Uuid::new_v4();
        let capability = service
            .issue_proxy_capability(user_id, "stamp-1", container_id, false)
            .unwrap();
        let token = capability.token;

        assert!(capability.expires_at > Utc::now());

        assert_eq!(
            service
                .verify_proxy_capability(&token, container_id, false)
                .unwrap(),
            ProxyCapabilityIdentity {
                user_id,
                security_stamp: "stamp-1".to_owned(),
            }
        );
        assert!(service
            .verify_proxy_capability(&token, Uuid::new_v4(), false)
            .is_err());
        assert!(service
            .verify_proxy_capability(&token, container_id, true)
            .is_err());
        assert!(service.verify(&token).is_err());

        let session = service
            .issue(user_id, Role::User, "alice", "stamp-1")
            .unwrap();
        assert!(service
            .verify_proxy_capability(&session, container_id, false)
            .is_err());
    }
}
