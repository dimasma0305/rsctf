//! Browser-visible OAuth outcomes. Only allowlisted codes cross this boundary;
//! provider responses, credentials, callback parameters and database errors do not.

use axum::http::header::{CACHE_CONTROL, SET_COOKIE};
use axum::http::HeaderValue;
use axum::response::{IntoResponse, Redirect, Response};

use crate::utils::error::{AppError, AppResult};

fn failure_code(error: &AppError) -> &'static str {
    match error {
        AppError::BadRequest(message) => match message.as_str() {
            "state was not initiated by this browser" => "oauth_state_mismatch",
            "invalid state" => "oauth_state_expired",
            "missing code" | "missing state" | "provider mismatch" | "unknown provider" => {
                "oauth_invalid_callback"
            }
            "provider not configured" => "oauth_not_configured",
            "token exchange failed" | "no access token" => "oauth_token_exchange",
            "userinfo failed" => "oauth_userinfo",
            "provider email is not verified" => "oauth_email_unverified",
            "no email" => "oauth_no_email",
            "registration is disabled" => "oauth_register_disabled",
            "email domain is not allowed" => "oauth_email_domain",
            _ => "oauth_failed",
        },
        AppError::Unauthorized => "oauth_email_conflict",
        AppError::Forbidden => "oauth_policy",
        _ => "oauth_failed",
    }
}

pub(super) fn callback_response(
    result: AppResult<Response>,
    provider: &str,
    secure: bool,
) -> Response {
    let mut response = match result {
        Ok(response) => response,
        Err(error) => {
            let reason = failure_code(&error);
            let provider = match provider.to_ascii_lowercase().as_str() {
                "discord" => "discord",
                "google" => "google",
                _ => "unknown",
            };
            // Deliberately never format `error`: provider URLs and database
            // details may contain information that must not reach the log.
            tracing::warn!(provider, reason, "OAuth callback rejected");
            Redirect::to(&format!("{}?error={reason}", super::LOGIN_PATH)).into_response()
        }
    };
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(value) = HeaderValue::from_str(&super::oauth_state_cookie("", 0, secure)) {
        // Set-Cookie is a repeated header. Replacing it here would discard the
        // session cookie issued by a successful callback.
        response.headers_mut().append(SET_COOKIE, value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middlewares::privilege_authentication::set_session_cookie;
    use axum::http::{header::LOCATION, StatusCode};

    #[test]
    fn successful_redirect_sets_session_and_only_clears_the_temporary_cookie() {
        let mut response = Redirect::to("/games/1").into_response();
        response.headers_mut().insert(
            SET_COOKIE,
            set_session_cookie("session-token", 3600, true)
                .parse()
                .unwrap(),
        );
        let response = callback_response(Ok(response), "discord", true);
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()[LOCATION], "/games/1");
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        let cookies: Vec<_> = response
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect();
        assert_eq!(cookies.len(), 2);
        assert!(cookies[0].starts_with("RSCTF_Token=session-token;"));
        assert!(cookies[0].contains("Max-Age=3600"));
        assert!(cookies[1].starts_with("RSCTF_OAuthState=;"));
        assert!(cookies[1].contains("Max-Age=0"));
    }

    #[test]
    fn rejected_callback_exposes_only_a_safe_reason_and_never_issues_a_session() {
        for (error, code) in [
            (
                AppError::bad_request("state was not initiated by this browser"),
                "oauth_state_mismatch",
            ),
            (
                AppError::bad_request("invalid state"),
                "oauth_state_expired",
            ),
            (
                AppError::bad_request("token exchange failed"),
                "oauth_token_exchange",
            ),
            (
                AppError::bad_request("provider email is not verified"),
                "oauth_email_unverified",
            ),
            (
                AppError::bad_request("registration is disabled"),
                "oauth_register_disabled",
            ),
            (
                AppError::internal("private provider response with credentials"),
                "oauth_failed",
            ),
            (
                AppError::bad_request("unrecognized private error"),
                "oauth_failed",
            ),
        ] {
            let response = callback_response(Err(error), "discord", true);
            assert_eq!(response.status(), StatusCode::SEE_OTHER);
            assert_eq!(
                response.headers()[LOCATION],
                format!("/account/login?error={code}")
            );
            assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
            let cookies: Vec<_> = response.headers().get_all(SET_COOKIE).iter().collect();
            assert_eq!(cookies.len(), 1);
            assert!(cookies[0]
                .to_str()
                .unwrap()
                .starts_with("RSCTF_OAuthState=;"));
        }
    }
}
