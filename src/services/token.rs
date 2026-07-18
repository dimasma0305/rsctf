//! Ported from RSCTF `Services/Token/TokenService.cs` — JWT issuing/verifying.

use chrono::Utc;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use sea_orm::ActiveEnum;
use serde::{Deserialize, Serialize};
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

pub struct TokenService {
    encoding: EncodingKey,
    decoding: DecodingKey,
    ttl_secs: i64,
}

impl TokenService {
    pub fn new(secret: &str, ttl_secs: i64) -> Self {
        Self {
            encoding: EncodingKey::from_secret(secret.as_bytes()),
            decoding: DecodingKey::from_secret(secret.as_bytes()),
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
        let claims = Claims {
            sub: id.to_string(),
            role: role.into_value(),
            name: name.to_string(),
            stamp: security_stamp.to_string(),
            iat: now,
            exp: now + self.ttl_secs,
        };
        encode(&Header::default(), &claims, &self.encoding)
            .map_err(|e| AppError::internal(format!("jwt encode: {e}")))
    }

    pub fn verify(&self, token: &str) -> Result<Claims, AppError> {
        decode::<Claims>(token, &self.decoding, &Validation::default())
            .map(|d| d.claims)
            .map_err(|_| AppError::Unauthorized)
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
}
