//! Ported from RSCTF `Models/Request/Account/*`. JSON is camelCase.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
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
