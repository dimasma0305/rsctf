//! services/mail.rs — ported from RSCTF `Services/Mail/MailSender.cs`.
//!
//! RSCTF's `MailSender` is a hosted singleton with a bounded, retrying in-memory
//! queue and a background worker that (re)builds a MailKit `SmtpClient` on config
//! change. Here we model the essential, directly-usable surface: a `MailSender`
//! built from environment SMTP config plus an async `send` that transmits one HTML
//! message via lettre's `AsyncSmtpTransport<Tokio1Executor>` over rustls. If SMTP
//! is not configured we log a warning and no-op (return `Ok`) rather than erroring,
//! matching RSCTF's "not fatal unless email confirmation is required" posture.
//!
//! The confirm / change-email / reset-password / invite templates RSCTF renders
//! from its localized `MailSender_Template` are provided here as plain builder
//! functions returning `(subject, html_body)`.

use lettre::message::{header::ContentType, Mailbox, Message};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

use crate::utils::error::{AppError, AppResult};

/// SMTP configuration resolved from the process environment.
///
/// Mirrors the fields RSCTF reads from `EmailConfig`/`SmtpConfig`
/// (`SenderAddress`, `Smtp.Host`, `Smtp.Port`, `UserName`, `Password`).
#[derive(Clone, Debug)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    /// Sender mailbox, e.g. `RSCTF <noreply@example.com>` or a bare address.
    pub from: String,
}

/// Sends HTML mail over SMTP. Cheap to clone; the underlying lettre transport is
/// a connection-pooled `Arc` internally.
#[derive(Clone)]
pub struct MailSender {
    /// `None` when SMTP is unconfigured — every `send` becomes a logged no-op.
    inner: Option<Configured>,
}

#[derive(Clone)]
struct Configured {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl MailSender {
    /// Build a sender from `RSCTF_SMTP_HOST`, `RSCTF_SMTP_PORT`,
    /// `RSCTF_SMTP_USER`, `RSCTF_SMTP_PASS`, `RSCTF_MAIL_FROM`.
    ///
    /// If the host or sender address is missing, or any value fails to parse,
    /// the sender is constructed in the unconfigured (no-op) state and a warning
    /// is logged — never panics, so the rest of the app boots regardless.
    pub fn from_env() -> Self {
        match Self::config_from_env() {
            Some(cfg) => Self::new(cfg),
            None => {
                tracing::warn!(
                    "SMTP not configured (set RSCTF_SMTP_HOST / RSCTF_MAIL_FROM); \
                     outgoing mail will be dropped"
                );
                Self { inner: None }
            }
        }
    }

    /// Read + validate the environment into an [`SmtpConfig`], or `None` if the
    /// minimum required fields (host + from) are absent.
    fn config_from_env() -> Option<SmtpConfig> {
        let host = non_empty(std::env::var("RSCTF_SMTP_HOST").ok())?;
        let from = non_empty(std::env::var("RSCTF_MAIL_FROM").ok())?;

        // Default to the STARTTLS submission port; RSCTF likewise treats an
        // unset/zero port as invalid, but 587 is the sane fallback here.
        let port = std::env::var("RSCTF_SMTP_PORT")
            .ok()
            .and_then(|p| p.trim().parse::<u16>().ok())
            .filter(|p| *p > 0)
            .unwrap_or(587);

        Some(SmtpConfig {
            host,
            port,
            username: non_empty(std::env::var("RSCTF_SMTP_USER").ok()),
            password: non_empty(std::env::var("RSCTF_SMTP_PASS").ok()),
            from,
        })
    }

    /// Build a sender from an explicit config. Falls back to the no-op state if
    /// the sender address doesn't parse or the transport can't be constructed.
    pub fn new(cfg: SmtpConfig) -> Self {
        let from: Mailbox = match cfg.from.parse() {
            Ok(mb) => mb,
            Err(e) => {
                tracing::warn!(error = %e, address = %cfg.from,
                    "invalid RSCTF_MAIL_FROM address; mail disabled");
                return Self { inner: None };
            }
        };

        let transport = match build_transport(&cfg) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, host = %cfg.host,
                    "failed to build SMTP transport; mail disabled");
                return Self { inner: None };
            }
        };

        tracing::debug!(host = %cfg.host, port = cfg.port, "SMTP transport ready");
        Self {
            inner: Some(Configured { transport, from }),
        }
    }

    /// Whether this sender will actually deliver mail (vs. no-op).
    pub fn is_configured(&self) -> bool {
        self.inner.is_some()
    }

    /// Send a single HTML email. Returns `Ok(())` without contacting any server
    /// when SMTP is unconfigured (logged at warn), mirroring RSCTF's behavior of
    /// not failing user-facing flows just because mail is off.
    pub async fn send(&self, to: &str, subject: &str, html_body: &str) -> AppResult<()> {
        let Some(cfg) = self.inner.as_ref() else {
            tracing::warn!(to, subject, "SMTP unconfigured; dropping mail");
            return Ok(());
        };

        let to_mbox: Mailbox = to
            .parse()
            .map_err(|e| AppError::bad_request(format!("invalid recipient address {to}: {e}")))?;

        let email = Message::builder()
            .from(cfg.from.clone())
            .to(to_mbox)
            .subject(subject)
            .header(ContentType::TEXT_HTML)
            .body(html_body.to_string())
            .map_err(|e| AppError::internal(format!("failed to build message: {e}")))?;

        cfg.transport
            .send(email)
            .await
            .map_err(|e| AppError::internal(format!("SMTP send failed: {e}")))?;

        tracing::info!(to, subject, "mail sent");
        Ok(())
    }
}

/// Construct the lettre async transport for `cfg`.
///
/// Port 465 uses implicit TLS (`relay`); anything else uses opportunistic
/// STARTTLS (`starttls_relay`) — the same "implicit on 465, STARTTLS otherwise"
/// heuristic as RSCTF's `ResolveSecureSocket` Auto mode. rustls is selected via
/// the crate's `tokio1-rustls-tls` feature.
fn build_transport(cfg: &SmtpConfig) -> anyhow::Result<AsyncSmtpTransport<Tokio1Executor>> {
    let mut builder = if cfg.port == 465 {
        AsyncSmtpTransport::<Tokio1Executor>::relay(&cfg.host)?
    } else {
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&cfg.host)?
    }
    .port(cfg.port);

    if let (Some(user), Some(pass)) = (cfg.username.as_ref(), cfg.password.as_ref()) {
        builder = builder.credentials(Credentials::new(user.clone(), pass.clone()));
    }

    Ok(builder.build())
}

fn non_empty(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// Email templates
//
// RSCTF renders one localized HTML skeleton (`MailSender_Template`) with the
// placeholders {title} {information} {btnmsg} {url} {nowtime} {platform} etc.
// substituted per mail type. We reproduce that skeleton and expose one builder
// per mail RSCTF sends, each returning (subject, html_body).
// ---------------------------------------------------------------------------

/// The platform name used in subjects/footers when none is supplied.
const DEFAULT_PLATFORM: &str = "RSCTF";

/// Render the shared HTML skeleton. `information` is trusted HTML (caller-built);
/// all other fields are plain text substituted verbatim, matching RSCTF's
/// `StringBuilder.Replace` approach.
fn render_template(
    title: &str,
    information: &str,
    button_message: &str,
    url: &str,
    platform: &str,
) -> String {
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
    format!(
        r#"<!DOCTYPE html>
<html>
<head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1.0"></head>
<body style="margin:0;padding:0;background:#f4f5f7;font-family:-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;">
  <table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="background:#f4f5f7;padding:24px 0;">
    <tr><td align="center">
      <table role="presentation" width="480" cellpadding="0" cellspacing="0" style="background:#ffffff;border-radius:8px;overflow:hidden;">
        <tr><td style="background:#1971c2;padding:20px 32px;color:#ffffff;font-size:18px;font-weight:600;">{platform}</td></tr>
        <tr><td style="padding:32px;color:#1a1a1a;">
          <h1 style="margin:0 0 16px;font-size:20px;">{title}</h1>
          <div style="font-size:14px;line-height:1.6;color:#333;">{information}</div>
          <div style="text-align:center;margin:28px 0;">
            <a href="{url}" style="display:inline-block;background:#1971c2;color:#ffffff;text-decoration:none;padding:12px 28px;border-radius:6px;font-size:15px;font-weight:600;">{button_message}</a>
          </div>
          <p style="font-size:12px;color:#868e96;line-height:1.5;">If the button doesn't work, copy this link into your browser:<br><a href="{url}" style="color:#1971c2;word-break:break-all;">{url}</a></p>
        </td></tr>
        <tr><td style="padding:16px 32px;background:#f8f9fa;color:#868e96;font-size:12px;">Sent by {platform} at {now}. If you did not request this, you can ignore this email.</td></tr>
      </table>
    </td></tr>
  </table>
</body>
</html>"#
    )
}

/// Confirmation email sent at registration (`MailType.ConfirmEmail`).
pub fn confirm_email(email: &str, confirm_link: &str, platform: Option<&str>) -> (String, String) {
    let platform = platform.unwrap_or(DEFAULT_PLATFORM);
    let title = "Verify your email";
    let info = format!(
        "<p>Welcome to <strong>{platform}</strong>! Please verify the email address \
         <code>{email}</code> to activate your account.</p>\
         <p>Click the button below to confirm. This link is valid for a limited time \
         and can only be used once.</p>"
    );
    let body = render_template(title, &info, "Verify Email", confirm_link, platform);
    (format!("{title} - {platform}"), body)
}

/// Email-change confirmation sent to the new address (`MailType.ChangeEmail`).
pub fn change_email(
    new_email: &str,
    change_link: &str,
    platform: Option<&str>,
) -> (String, String) {
    let platform = platform.unwrap_or(DEFAULT_PLATFORM);
    let title = "Confirm your new email";
    let info = format!(
        "<p>A request was made to change your <strong>{platform}</strong> account email to \
         <code>{new_email}</code>.</p>\
         <p>Click the button below to confirm this change. If you did not request it, \
         ignore this email and your address will stay the same.</p>"
    );
    let body = render_template(title, &info, "Confirm Change", change_link, platform);
    (format!("{title} - {platform}"), body)
}

/// Password-reset email (`MailType.ResetPassword`).
pub fn reset_password(reset_link: &str, platform: Option<&str>) -> (String, String) {
    let platform = platform.unwrap_or(DEFAULT_PLATFORM);
    let title = "Reset your password";
    let info = format!(
        "<p>We received a request to reset the password for your <strong>{platform}</strong> \
         account.</p>\
         <p>Click the button below to choose a new password. This link is valid for a limited \
         time and can only be used once. If you did not request a reset, ignore this email.</p>"
    );
    let body = render_template(title, &info, "Reset Password", reset_link, platform);
    (format!("{title} - {platform}"), body)
}

/// Account-invite / credential email for operator-imported users.
///
/// Ports RSCTF's `SendCredentialsBatch` per-recipient body: it names the created
/// username and links to a one-time password-set flow.
pub fn invite(
    user_name: &str,
    set_password_link: &str,
    platform: Option<&str>,
) -> (String, String) {
    let platform = platform.unwrap_or(DEFAULT_PLATFORM);
    let title = "Set Your Password";
    let info = format!(
        "<p>An account has been created for you on <strong>{platform}</strong>.</p>\
         <p><strong>Username:</strong> <code>{user_name}</code></p>\
         <p>Click the button below to set your own password. This link is valid for a \
         limited time and can only be used once.</p>"
    );
    let body = render_template(title, &info, "Set My Password", set_password_link, platform);
    (format!("{title} - {platform}"), body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_empty_trims_and_filters() {
        assert_eq!(non_empty(Some("  x ".into())), Some("x".into()));
        assert_eq!(non_empty(Some("   ".into())), None);
        assert_eq!(non_empty(None), None);
    }

    #[test]
    fn unconfigured_sender_is_noop() {
        let s = MailSender { inner: None };
        assert!(!s.is_configured());
    }

    #[test]
    fn templates_substitute_fields() {
        let (subject, body) = confirm_email("a@b.c", "https://x/confirm?t=1", Some("MyCTF"));
        assert_eq!(subject, "Verify your email - MyCTF");
        assert!(body.contains("https://x/confirm?t=1"));
        assert!(body.contains("MyCTF"));
        assert!(body.contains("a@b.c"));

        let (subject, _) = reset_password("https://x/reset", None);
        assert_eq!(subject, "Reset your password - RSCTF");
    }
}
