//! Reverse-proxy IP + SMTP/captcha config diagnostics.

use super::*;
use axum::extract::ConnectInfo;
use std::net::SocketAddr;

// ─── Diagnostics ─────────────────────────────────────────────────────────────

/// RSCTF `MyIpInfoModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MyIpInfoModel {
    pub detected_ip: String,
    pub raw_connection_ip: String,
    pub forwarded_for: String,
    pub proxy_trusted: bool,
    pub trusted_networks: Vec<String>,
}

/// `GET /api/admin/MyIp` — show the real socket peer, the configured proxy
/// networks, and the exact client IP the request resolver uses. Forwarded
/// headers are reflected for troubleshooting but never imply trust by presence.
pub async fn my_ip(
    _admin: AdminUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> RequestResponse<MyIpInfoModel> {
    let raw_connection_ip = crate::services::anti_cheat::normalize_ip(peer.ip());
    let detected_ip = crate::services::anti_cheat::client_ip(&headers, Some(peer.ip()))
        .unwrap_or_else(|| raw_connection_ip.clone());
    let forwarded_for = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let proxy_trusted = crate::services::anti_cheat::is_trusted_proxy(peer.ip());

    RequestResponse::ok(MyIpInfoModel {
        raw_connection_ip,
        detected_ip,
        forwarded_for,
        proxy_trusted,
        trusted_networks: crate::services::anti_cheat::configured_trusted_proxy_cidrs(),
    })
}

// ─── Diagnostics: real captcha / email validation (RSCTF TestCaptcha/TestEmail) ──

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmtpTestConfig {
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailTestConfig {
    #[serde(default)]
    pub user_name: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub sender_address: Option<String>,
    #[serde(default)]
    pub smtp: Option<SmtpTestConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailTestModel {
    pub config: EmailTestConfig,
    #[serde(default)]
    pub recipient: String,
}

/// `POST /api/admin/email/test` — actually send a test email with the supplied
/// SMTP config (RSCTF `TestEmail`); a send failure surfaces as a 400 with the error.
pub async fn test_email(
    _admin: AdminUser,
    Json(m): Json<EmailTestModel>,
) -> AppResult<MessageResponse> {
    let recipient = m.recipient.trim();
    if !recipient.contains('@') {
        return Err(AppError::bad_request("A valid recipient is required"));
    }
    let smtp = m
        .config
        .smtp
        .and_then(|s| s.host.filter(|h| !h.trim().is_empty()).map(|h| (h, s.port)))
        .ok_or_else(|| AppError::bad_request("SMTP host is not configured"))?;
    let cfg = crate::services::mail::SmtpConfig {
        host: smtp.0,
        port: smtp.1.unwrap_or(587),
        username: m.config.user_name.filter(|s| !s.is_empty()),
        password: m.config.password.filter(|s| !s.is_empty()),
        from: m
            .config
            .sender_address
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "noreply@rsctf".to_string()),
    };
    crate::services::mail::MailSender::new(cfg)
        .send(
            recipient,
            "rsctf SMTP test",
            "<p>This is a test email from rsctf. If you received it, your SMTP settings are correct.</p>",
        )
        .await
        .map_err(|e| AppError::bad_request(format!("Email test failed: {e}")))?;
    Ok(MessageResponse::ok("Test email sent"))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptchaTestConfig {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub secret_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptchaTestModel {
    pub config: CaptchaTestConfig,
}

/// `POST /api/admin/captcha/test` — validate the captcha config (RSCTF
/// `TestCaptcha`). For Turnstile, probe siteverify with the secret: an invalid
/// secret is rejected; a valid secret returns an `invalid-input-response` (the
/// dummy token) which confirms the secret works.
pub async fn test_captcha(
    _admin: AdminUser,
    Json(m): Json<CaptchaTestModel>,
) -> AppResult<MessageResponse> {
    match m.config.provider.as_deref().unwrap_or("None") {
        "CloudflareTurnstile" => {
            let secret = m
                .config
                .secret_key
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| AppError::bad_request("Turnstile secret key is required"))?;
            let resp = crate::services::captcha::turnstile_client()
                .post("https://challenges.cloudflare.com/turnstile/v0/siteverify")
                .form(&[("secret", secret.as_str()), ("response", "rsctf-test")])
                .send()
                .await
                .map_err(|e| AppError::bad_request(format!("Turnstile unreachable: {e}")))?;
            let body: Value = resp
                .json()
                .await
                .map_err(|e| AppError::bad_request(format!("Turnstile response: {e}")))?;
            let bad_secret = body
                .get("error-codes")
                .and_then(|v| v.as_array())
                .is_some_and(|codes| {
                    codes
                        .iter()
                        .any(|c| c.as_str() == Some("invalid-input-secret"))
                });
            if bad_secret {
                return Err(AppError::bad_request("Turnstile secret key is invalid"));
            }
            Ok(MessageResponse::ok("Turnstile configuration is valid"))
        }
        // None / HashPow: nothing external to validate.
        _ => Ok(MessageResponse::ok("Captcha configuration accepted")),
    }
}
