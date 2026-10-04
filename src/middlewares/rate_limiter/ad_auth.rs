//! Admission helpers for Attack & Defense bearer-token authentication.

use axum::http::Method;

use super::{check_async, Policy};

pub(super) fn supports_bearer(method: &Method, path: &str) -> bool {
    let normalized = path.to_ascii_lowercase();
    let segments: Vec<&str> = normalized
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let valid_game = segments
        .get(2)
        .and_then(|value| value.parse::<i32>().ok())
        .is_some_and(|game_id| game_id > 0);
    if !valid_game || segments.get(0..2) != Some(&["api", "game"]) {
        return false;
    }
    matches!(
        (method, segments.as_slice()),
        (&Method::POST, ["api", "game", _, "ad", "submit"])
            | (&Method::GET, ["api", "game", _, "ad", "targets"])
            | (&Method::GET, ["api", "game", _, "ad", "koth", "token"])
            | (&Method::GET, ["api", "game", _, "ad", "koth", "hills"])
    )
}

pub(super) async fn admit(token: &str, ip: &str) -> Result<(), u64> {
    let token_key = format!("ad-auth:{}", crate::services::ad::api_token::hash(token));
    let (token_result, source_result) = tokio::join!(
        check_async(Policy::AdAuthTokenAdmission, token_key),
        check_async(Policy::AdAuthSourceAdmission, ip.to_owned()),
    );
    token_result.and(source_result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authenticates_bearers_only_on_dual_auth_routes() {
        for path in [
            "/api/Game/7/Ad/Submit",
            "/api/Game/7/Ad/Targets",
            "/api/game/7/ad/targets",
            "/api/Game/7/Ad/Koth/Token",
            "/api/Game/7/Ad/Koth/Hills",
        ] {
            let method = if path.ends_with("Submit") {
                Method::POST
            } else {
                Method::GET
            };
            assert!(supports_bearer(&method, path), "{path}");
        }
        for (method, path) in [
            (Method::GET, "/api/Game/7/Ad/Submit"),
            (Method::GET, "/api/game/7/details"),
            (Method::GET, "/api/admin/users"),
            (Method::GET, "/api/Game/nope/Ad/Targets"),
            (Method::POST, "/api/Game/7/Ad/Token"),
        ] {
            assert!(!supports_bearer(&method, path), "{path}");
        }
    }
}
