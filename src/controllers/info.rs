//! Ported from RSCTF `Controllers/InfoController.cs`.
//!
//! Global information APIs: client config, posts, and captcha info.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::Router;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use serde::Serialize;
use std::collections::HashMap;
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::models::data::{config, post, user};
use crate::services::captcha::CaptchaSettings;
use crate::utils::codec;
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

/// Mirrors RSCTF `PostInfoModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostInfoModel {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub is_pinned: bool,
    pub tags: Option<Vec<String>>,
    pub author_avatar: Option<String>,
    pub author_name: Option<String>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub time: chrono::DateTime<chrono::Utc>,
}

/// Mirrors RSCTF `PostDetailModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostDetailModel {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub content: String,
    pub is_pinned: bool,
    pub tags: Option<Vec<String>>,
    pub author_avatar: Option<String>,
    pub author_name: Option<String>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub time: chrono::DateTime<chrono::Utc>,
}

/// An OAuth provider is available to the client when its `RSCTF_<P>_CLIENT_ID`
/// and `RSCTF_<P>_CLIENT_SECRET` env vars are both set (mirrors RSCTF's
/// `ClientConfig.EnableGoogleAuth`/`EnableDiscordAuth` = credentials configured).
fn oauth_configured(provider: &str) -> bool {
    let id = std::env::var(format!("RSCTF_{provider}_CLIENT_ID")).unwrap_or_default();
    let secret = std::env::var(format!("RSCTF_{provider}_CLIENT_SECRET")).unwrap_or_default();
    !id.trim().is_empty() && !secret.trim().is_empty()
}

fn effective_port_mapping(configured: String, backend_requires_proxy: bool) -> String {
    if backend_requires_proxy {
        "PlatformProxy".to_string()
    } else {
        configured
    }
}

/// Mirrors RSCTF `ClientConfig`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientConfig {
    pub title: String,
    pub slogan: String,
    pub footer_info: Option<String>,
    pub custom_theme: Option<String>,
    pub api_public_key: Option<String>,
    pub logo_url: Option<String>,
    pub port_mapping: String,
    pub default_lifetime: i32,
    pub extension_duration: i32,
    pub renewal_window: i32,
    pub enable_browser_fingerprint: bool,
    pub enable_google_auth: bool,
    pub enable_discord_auth: bool,
}

/// Mirrors RSCTF `ClientCaptchaInfoModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCaptchaInfoModel {
    #[serde(rename = "type")]
    pub type_: String,
    pub site_key: Option<String>,
}

/// Mirrors RSCTF `HashPowChallenge`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HashPowChallenge {
    pub id: String,
    pub challenge: String,
    pub difficulty: i32,
}

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/config", get(get_client_config))
        .route("/api/posts", get(get_posts))
        .route("/api/posts/latest", get(get_latest_posts))
        .route("/api/posts/{id}", get(get_post))
        .route("/api/captcha", get(get_captcha))
        .route("/api/captcha/powchallenge", get(get_pow_challenge))
}

/// `GET /api/Config` — client-facing site configuration.
pub async fn get_client_config(
    State(st): State<SharedState>,
) -> AppResult<RequestResponse<ClientConfig>> {
    // Base values come from the in-process config; the `Configs` key/value
    // table can override the mutable globals at runtime.
    let mut title = st.config.global.title.clone();
    let mut slogan = st.config.global.slogan.clone();
    let mut footer_info = st.config.global.footer_info.clone();
    let mut custom_theme: Option<String> = None;
    let mut logo_hash: Option<String> = None;
    let mut enable_browser_fingerprint = false;
    // Container port-mapping mode advertised to the client (`ContainerPortMappingType`):
    // `Default` = direct host:port, `PlatformProxy` = wsrx-proxied. The client gates
    // wsrx on `config.portMapping === PlatformProxy` (InstanceEntry.tsx).
    let mut port_mapping = "Default".to_string();
    // Container lifetime trio the client reads for the instance UI
    // (ContainerPolicy). Defaults mirror RSCTF's ContainerPolicy defaults
    // (120 / 120 / 10 minutes); the stored keys override them at runtime.
    let mut default_lifetime = 120;
    let mut extension_duration = 120;
    let mut renewal_window = 10;

    let rows = config::Entity::find().all(&st.db).await?;
    for row in rows {
        let Some(value) = row.value else { continue };
        match row.config_key.as_str() {
            "GlobalConfig:Title" => title = value,
            "GlobalConfig:Slogan" => slogan = value,
            "GlobalConfig:FooterInfo" => footer_info = Some(value),
            "GlobalConfig:CustomTheme" => custom_theme = Some(value),
            "GlobalConfig:LogoHash" => logo_hash = Some(value),
            "ContainerProvider:PortMappingType" if !value.is_empty() => port_mapping = value,
            "ContainerPolicy:DefaultLifetime" => {
                if let Ok(v) = value.parse() {
                    default_lifetime = v;
                }
            }
            "ContainerPolicy:ExtensionDuration" => {
                if let Ok(v) = value.parse() {
                    extension_duration = v;
                }
            }
            "ContainerPolicy:RenewalWindow" => {
                if let Ok(v) = value.parse() {
                    renewal_window = v;
                }
            }
            // Persisted as lowercase `bool::to_string()` (matching admin config).
            "AccountPolicy:EnableBrowserFingerprint" => {
                enable_browser_fingerprint = value == "true";
            }
            _ => {}
        }
    }

    // A remote worker never exposes a player-reachable host address. Its
    // container entry is therefore a proxy UUID regardless of the mutable
    // direct-port preference stored for local backends. Advertising `Default`
    // here would make the client display that UUID literally instead of
    // connecting it through `/api/proxy/{id}`.
    port_mapping = effective_port_mapping(port_mapping, st.containers.requires_proxy());

    let logo_url = logo_hash
        .filter(|h| !h.is_empty())
        .map(|h| format!("/assets/{h}/logo"));

    Ok(RequestResponse::ok(ClientConfig {
        title,
        slogan,
        footer_info,
        custom_theme,
        api_public_key: None,
        logo_url,
        port_mapping,
        default_lifetime,
        extension_duration,
        renewal_window,
        enable_browser_fingerprint,
        enable_google_auth: oauth_configured("GOOGLE"),
        enable_discord_auth: oauth_configured("DISCORD"),
    }))
}

/// `GET /api/Posts` — all posts, pinned first then newest.
pub async fn get_posts(
    State(st): State<SharedState>,
) -> AppResult<RequestResponse<Vec<PostInfoModel>>> {
    let posts = load_ordered_posts(&st).await?;
    let authors = load_authors(&st, &posts).await?;
    let data = posts.into_iter().map(|p| to_info(p, &authors)).collect();
    Ok(RequestResponse::ok(data))
}

/// `GET /api/Posts/Latest` — the 20 most recent posts (pinned first).
pub async fn get_latest_posts(
    State(st): State<SharedState>,
) -> AppResult<RequestResponse<Vec<PostInfoModel>>> {
    let mut posts = load_ordered_posts(&st).await?;
    posts.truncate(20);
    let authors = load_authors(&st, &posts).await?;
    let data = posts.into_iter().map(|p| to_info(p, &authors)).collect();
    Ok(RequestResponse::ok(data))
}

/// `GET /api/Posts/{id}` — a single post with full content.
pub async fn get_post(
    State(st): State<SharedState>,
    Path(id): Path<String>,
) -> AppResult<RequestResponse<PostDetailModel>> {
    let post = post::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Post not found"))?;

    let (author_name, author_avatar) = match post.author_id {
        Some(uid) => match user::Entity::find_by_id(uid).one(&st.db).await? {
            Some(u) => (u.user_name.clone(), u.avatar_url()),
            None => (None, None),
        },
        None => (None, None),
    };

    Ok(RequestResponse::ok(PostDetailModel {
        id: post.id,
        title: post.title,
        summary: post.summary,
        content: post.content,
        is_pinned: post.is_pinned,
        tags: parse_tags(post.tags),
        author_avatar,
        author_name,
        time: post.update_time_utc,
    }))
}

/// `GET /api/captcha` — the client captcha configuration (RSCTF
/// `InfoController.GetClientCaptchaInfo` -> `CaptchaService.ClientInfo`). Read
/// from the LIVE `CaptchaConfig:*` settings so the widget the client renders
/// matches the provider the server verifies against; the `provider`/`siteKey`
/// come straight from the admin config (independent of the `UseCaptcha`
/// enforcement toggle, mirroring RSCTF's `ClientInfo(Config)`).
pub async fn get_captcha(State(st): State<SharedState>) -> RequestResponse<ClientCaptchaInfoModel> {
    let settings = CaptchaSettings::load(&st.db).await;
    // RSCTF `InfoController` (line 148): advertise the captcha provider to the
    // client ONLY when AccountPolicy.UseCaptcha is enabled. Otherwise the
    // login/register captcha widget still renders — and for HashPow it grinds a
    // (possibly very expensive) proof-of-work — even though captcha is turned off.
    let (type_, site_key) = if settings.use_captcha {
        (settings.provider, settings.site_key)
    } else {
        ("None".to_string(), None)
    };
    RequestResponse::ok(ClientCaptchaInfoModel { type_, site_key })
}

/// `GET /api/captcha/powchallenge` — proof-of-work challenge.
///
/// Ports RSCTF `InfoController.PowChallenge`. When the configured captcha
/// provider is `HashPow`, mint a fresh random challenge plus a short-lived
/// cache entry (5-minute sliding window, matching RSCTF) so the paired
/// verification step can later confirm the client's nonce, and return the
/// `HashPowChallenge` shape the client expects. For any other provider
/// (notably `None`) RSCTF has no PoW to issue and returns `404 NotFound`,
/// so we do the same.
///
pub async fn get_pow_challenge(
    State(st): State<SharedState>,
) -> AppResult<RequestResponse<HashPowChallenge>> {
    // The provider + difficulty come from the LIVE captcha config (the same
    // source the verify step reads), so the client solves the PoW at exactly the
    // difficulty the server later checks against.
    let settings = CaptchaSettings::load(&st.db).await;
    let difficulty = if settings.provider == "HashPow" {
        settings.difficulty
    } else {
        // "None"/Turnstile: no PoW challenge to issue — RSCTF returns 404.
        return Err(AppError::not_found("PoW challenge is not available"));
    };

    // RSCTF: 8 random challenge bytes (returned as lowercase hex) keyed by a
    // 12-char random hex id. We store the hex challenge string itself so the
    // verifier hashes exactly what the client was handed.
    let id = codec::random_hex(6); // 6 bytes -> 12 hex chars
    let challenge = codec::random_hex(8); // 8 bytes -> 16 hex chars

    // CacheKey.HashPow(id) => "_HP_{id}"; 5-minute expiry.
    st.cache
        .set(
            &format!("_HP_{id}"),
            challenge.as_bytes(),
            Some(std::time::Duration::from_secs(5 * 60)),
        )
        .await;

    Ok(RequestResponse::ok(HashPowChallenge {
        id,
        challenge,
        difficulty: difficulty as i32,
    }))
}

// --- helpers ---

async fn load_ordered_posts(st: &SharedState) -> AppResult<Vec<post::Model>> {
    // Pinned posts first, then by newest update time.
    Ok(post::Entity::find()
        .order_by_desc(post::Column::IsPinned)
        .order_by_desc(post::Column::UpdateTimeUtc)
        .all(&st.db)
        .await?)
}

async fn load_authors(
    st: &SharedState,
    posts: &[post::Model],
) -> AppResult<HashMap<Uuid, user::Model>> {
    let ids: Vec<Uuid> = posts.iter().filter_map(|p| p.author_id).collect();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let users = user::Entity::find()
        .filter(user::Column::Id.is_in(ids))
        .all(&st.db)
        .await?;
    Ok(users.into_iter().map(|u| (u.id, u)).collect())
}

fn to_info(post: post::Model, authors: &HashMap<Uuid, user::Model>) -> PostInfoModel {
    let author = post.author_id.and_then(|id| authors.get(&id));
    PostInfoModel {
        id: post.id,
        title: post.title,
        summary: post.summary,
        is_pinned: post.is_pinned,
        tags: parse_tags(post.tags),
        author_avatar: author.and_then(|u| u.avatar_url()),
        author_name: author.and_then(|u| u.user_name.clone()),
        time: post.update_time_utc,
    }
}

fn parse_tags(tags: Option<serde_json::Value>) -> Option<Vec<String>> {
    tags.and_then(|v| serde_json::from_value(v).ok())
}

#[cfg(test)]
mod tests {
    use super::effective_port_mapping;

    #[test]
    fn proxy_required_backend_overrides_direct_port_preference() {
        assert_eq!(
            effective_port_mapping("Default".to_string(), true),
            "PlatformProxy"
        );
    }

    #[test]
    fn local_backend_keeps_the_configured_port_mapping() {
        assert_eq!(
            effective_port_mapping("Default".to_string(), false),
            "Default"
        );
        assert_eq!(
            effective_port_mapping("PlatformProxy".to_string(), false),
            "PlatformProxy"
        );
    }
}
