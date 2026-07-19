//! Ported from RSCTF `Models/Request/Account/*`. JSON is camelCase.

use serde::Deserialize;
use std::fmt;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterModel {
    pub user_name: String,
    pub password: String,
    pub email: String,
    /// Captcha token (`ModelWithCaptcha.challenge`); verified only when the live
    /// `AccountPolicy:UseCaptcha` is on. Absent/`null` on captcha-off deployments.
    #[serde(default)]
    pub challenge: Option<String>,
    /// Browser fingerprint (`^[a-f0-9]{64}$`); required only when the live
    /// `AccountPolicy:EnableBrowserFingerprint` is on.
    #[serde(default)]
    pub fingerprint: Option<String>,
    /// One-time deployment secret required only while the authoritative user
    /// table is empty. It is ignored after the bootstrap administrator exists.
    #[serde(default)]
    pub bootstrap_token: Option<String>,
}

impl fmt::Debug for RegisterModel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegisterModel")
            .field("user_name", &self.user_name)
            .field("password", &"<redacted>")
            .field("email", &self.email)
            .field("challenge", &self.challenge.as_ref().map(|_| "<redacted>"))
            .field("fingerprint", &self.fingerprint)
            .field(
                "bootstrap_token",
                &self.bootstrap_token.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileUpdateModel {
    pub user_name: Option<String>,
    pub bio: Option<String>,
    pub phone: Option<String>,
    pub real_name: Option<String>,
    pub std_number: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordChangeModel {
    pub old: String,
    pub new: String,
}

#[cfg(test)]
mod tests {
    use super::RegisterModel;

    #[test]
    fn register_model_accepts_camel_case_bootstrap_token_and_redacts_debug() {
        let model: RegisterModel = serde_json::from_str(
            r#"{
                "userName":"player",
                "password":"Password1",
                "email":"player@example.test",
                "bootstrapToken":"top-secret"
            }"#,
        )
        .unwrap();

        assert_eq!(model.bootstrap_token.as_deref(), Some("top-secret"));
        let debug = format!("{model:?}");
        assert!(!debug.contains("top-secret"));
        assert!(!debug.contains("Password1"));
    }

    #[test]
    fn register_model_keeps_bootstrap_token_optional() {
        let model: RegisterModel = serde_json::from_str(
            r#"{
                "userName":"player",
                "password":"Password1",
                "email":"player@example.test"
            }"#,
        )
        .unwrap();
        assert!(model.bootstrap_token.is_none());
    }
}
