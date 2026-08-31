//! Tolerant request DTOs owned by the account HTTP surface.

use serde::Deserialize;

/// Credentials plus the optional browser fingerprint collected by the SPA.
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
    /// A session bearer alone cannot redirect future recovery mail.
    #[serde(default)]
    pub password: String,
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
}
