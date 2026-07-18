//! Credential delivery handlers split from `users.rs`.

use super::users::{may_bulk_recredential, CRED_CACHE_PREFIX};
use super::*;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialSendItem {
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub user_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialSendRequest {
    #[serde(default)]
    pub items: Vec<CredentialSendItem>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialSendResult {
    pub email: String,
    pub user_name: String,
    pub sent: bool,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailSendResult {
    pub sent: usize,
    pub failed: usize,
    pub results: Vec<CredentialSendResult>,
}

/// Email cached import credentials without ever resetting the stored password.
pub async fn send_credentials(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Json(req): Json<CredentialSendRequest>,
) -> AppResult<Json<EmailSendResult>> {
    let sender = crate::services::mail::MailSender::from_env();
    let mailer_ready = sender.is_configured();
    let platform = st.config.global.title.as_str();
    let mut results = Vec::with_capacity(req.items.len());
    let (mut sent, mut failed) = (0usize, 0usize);

    for item in &req.items {
        let email = item.email.trim().to_lowercase();
        let mut fail = |err: &str, results: &mut Vec<CredentialSendResult>| {
            failed += 1;
            results.push(CredentialSendResult {
                email: email.clone(),
                user_name: item.user_name.clone(),
                sent: false,
                error: Some(err.to_string()),
            });
        };

        if !mailer_ready {
            fail("no SMTP configured - nothing sent (dry run)", &mut results);
            continue;
        }

        let cache_key = format!("{CRED_CACHE_PREFIX}{}", email.to_uppercase());
        if user::Entity::find()
            .filter(user::Column::NormalizedEmail.eq(email.to_uppercase()))
            .one(&st.db)
            .await?
            .is_some_and(|target| !may_bulk_recredential(target.role))
        {
            st.cache.remove(&cache_key).await;
            fail(
                "administrator credentials cannot be sent from the import cache",
                &mut results,
            );
            continue;
        }

        let Some(password) = st
            .cache
            .get(&cache_key)
            .await
            .and_then(|value| String::from_utf8(value.to_vec()).ok())
        else {
            fail(
                "credentials expired or not cached - reset the user's password to re-issue",
                &mut results,
            );
            continue;
        };

        let subject = format!("Your {platform} account credentials");
        let body = format!(
            "<p>Hello,</p>\
             <p>An account has been created for you on <b>{platform}</b>.</p>\
             <p><b>Username:</b> {user}<br/><b>Password:</b> {pass}</p>\
             <p>Please sign in and change your password.</p>",
            user = html_escape(&item.user_name),
            pass = html_escape(&password),
        );

        match sender.send(&email, &subject, &body).await {
            Ok(()) => {
                st.cache.remove(&cache_key).await;
                sent += 1;
                results.push(CredentialSendResult {
                    email: email.clone(),
                    user_name: item.user_name.clone(),
                    sent: true,
                    error: None,
                });
            }
            Err(error) => fail(&format!("delivery failed: {error}"), &mut results),
        }
    }

    Ok(Json(EmailSendResult {
        sent,
        failed,
        results,
    }))
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
