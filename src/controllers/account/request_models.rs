//! Account request models whose wire contract differs from the legacy shared
//! request module.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Credentials plus the optional browser-fingerprint proof collected by the SPA.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginModel {
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub fingerprint_proof: Option<String>,
    #[serde(default)]
    pub challenge: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailChangeModel {
    #[serde(default)]
    pub new_mail: String,
    #[serde(default)]
    pub password: String,
    /// Stable identity retained when the client did not receive a response.
    pub operation_id: Uuid,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountVerifyModel {
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub email: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordResetModel {
    /// Stable identity retained when the client did not receive a response.
    pub operation_id: Uuid,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub r_token: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryModel {
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub challenge: Option<String>,
    /// Stable identity retained when the client did not receive a response.
    pub operation_id: Uuid,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct EmailChangeTicket {
    pub user_id: Uuid,
    pub new_email: String,
    pub security_stamp: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mail_operation_ids_use_the_camel_case_wire_contract() {
        let operation_id = Uuid::new_v4();
        let recovery: RecoveryModel = serde_json::from_value(serde_json::json!({
            "email": "player@example.test",
            "operationId": operation_id
        }))
        .unwrap();
        assert_eq!(recovery.operation_id, operation_id);

        let missing = serde_json::from_value::<RecoveryModel>(serde_json::json!({
            "email": "player@example.test"
        }));
        assert!(missing.is_err());
    }
}
