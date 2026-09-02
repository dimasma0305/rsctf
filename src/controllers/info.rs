//! Ported from RSCTF `Controllers/InfoController.cs`.
//!
//! Global information APIs: client config, posts, and captcha info.

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use chrono::{DateTime, Utc};
use sea_orm::EntityTrait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::app_state::SharedState;
use crate::models::data::{config, post, user};
use crate::utils::codec;
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::{ArrayResponse, RequestResponse};

const LATEST_POST_LIMIT: i64 = 20;
const DEFAULT_POST_PAGE_SIZE: u64 = 10;
const MAX_POST_PAGE_SIZE: u64 = 50;
const POST_FEED_CACHE_CONTROL: &str = "public, no-cache";

/// Select posts before touching the author table. Page/latest callers bind a
/// finite limit, keeping both the post projection and author lookup bounded;
/// only the compatibility endpoint deliberately binds no limit.
const ORDERED_POST_PAGE_SQL: &str = r#"
WITH selected AS MATERIALIZED (
    SELECT post.id, post.title, post.summary, post.is_pinned, post.tags,
           post.author_id, post.update_time_utc
      FROM "Posts" post
     ORDER BY post.is_pinned DESC, post.update_time_utc DESC, post.id DESC
    OFFSET $1 LIMIT $2
)
SELECT selected.id, selected.title, selected.summary, selected.is_pinned,
       selected.tags, author.avatar_hash AS author_avatar_hash,
       author.user_name AS author_name, selected.update_time_utc
  FROM selected
  LEFT JOIN "AspNetUsers" author ON author.id = selected.author_id
 ORDER BY selected.is_pinned DESC, selected.update_time_utc DESC, selected.id DESC
"#;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostPageParams {
    #[serde(default = "default_post_page_size")]
    count: u64,
    #[serde(default)]
    skip: u64,
}

fn default_post_page_size() -> u64 {
    DEFAULT_POST_PAGE_SIZE
}

impl PostPageParams {
    fn limit(&self) -> u64 {
        self.count.clamp(1, MAX_POST_PAGE_SIZE)
    }

    fn offset(&self) -> i64 {
        self.skip.min(i64::MAX as u64) as i64
    }
}

#[derive(Debug, sqlx::FromRow)]
struct PostInfoRow {
    id: String,
    title: String,
    summary: String,
    is_pinned: bool,
    tags: Option<serde_json::Value>,
    author_avatar_hash: Option<String>,
    author_name: Option<String>,
    update_time_utc: DateTime<Utc>,
}

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
    pub allow_register: bool,
    pub allow_password_registration: bool,
    pub email_confirmation_required: bool,
    pub allow_competition_history_purge: bool,
    pub enable_google_auth: bool,
    pub enable_discord_auth: bool,
    pub donations_enabled: bool,
    pub donation_provider: Option<crate::services::donations::DonationProvider>,
    pub donation_url: Option<String>,
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
    /// Absolute Unix time in milliseconds; the browser refreshes before this
    /// boundary instead of submitting an already-expired proof.
    pub expires_at: i64,
}

fn hashpow_challenge_response(challenge: HashPowChallenge) -> Response {
    let mut response = RequestResponse::ok(challenge).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/config", get(get_client_config))
        .route("/api/posts", get(get_posts))
        .route("/api/posts/latest", get(get_latest_posts))
        .route("/api/posts/page", get(get_posts_page))
        .route("/api/posts/{id}", get(get_post))
        .route("/api/captcha", get(get_captcha))
        .route(
            "/api/captcha/powchallenge",
            crate::middlewares::rate_limiter::limited(
                crate::middlewares::rate_limiter::Policy::PowIssuanceGlobal,
                crate::middlewares::rate_limiter::limited(
                    crate::middlewares::rate_limiter::Policy::PowIssuanceSource,
                    get(get_pow_challenge),
                ),
            ),
        )
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
    let mut allow_register = st.config.account.allow_register;
    let mut allow_password_registration = st.config.account.allow_password_registration;
    let mut email_confirmation_required = st.config.account.email_confirmation_required;
    // Container port-mapping mode advertised to the client (`ContainerPortMappingType`):
    // `Default` = direct host:port, `PlatformProxy` = wsrx-proxied. The client gates
    // wsrx on `config.portMapping === PlatformProxy` (InstanceEntry.tsx).
    let mut port_mapping = crate::controllers::admin::DEFAULT_CONTAINER_PORT_MAPPING.to_string();
    // Container lifetime trio the client reads for the instance UI
    // (ContainerPolicy). Defaults mirror RSCTF's ContainerPolicy defaults
    // (120 / 120 / 10 minutes); the stored keys override them at runtime.
    let mut default_lifetime = 120;
    let mut extension_duration = 120;
    let mut renewal_window = 10;

    let rows = config::Entity::find().all(&st.db).await?;
    let donation_values: BTreeMap<String, Option<String>> = rows
        .iter()
        .map(|row| (row.config_key.clone(), row.value.clone()))
        .collect();
    let (donations_enabled, donation_provider, donation_url) =
        crate::services::donations::public_config(&donation_values);
    for row in rows {
        let Some(value) = row.value else { continue };
        match row.config_key.as_str() {
            "GlobalConfig:Title" => title = value,
            "GlobalConfig:Slogan" => slogan = value,
            "GlobalConfig:FooterInfo" => footer_info = Some(value),
            "GlobalConfig:CustomTheme" => custom_theme = Some(value),
            "GlobalConfig:LogoHash" => logo_hash = Some(value),
            "ContainerProvider:PortMappingType" => {
                port_mapping =
                    crate::controllers::admin::normalized_container_port_mapping(Some(&value))
                        .to_string()
            }
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
            "AccountPolicy:AllowRegister" => {
                allow_register = value == "true";
            }
            "AccountPolicy:AllowPasswordRegistration" => {
                allow_password_registration = value == "true";
            }
            "AccountPolicy:EmailConfirmationRequired" => {
                email_confirmation_required = value == "true";
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
    let oauth = crate::services::oauth_config::OAuthSettings::load(st.pg()).await?;

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
        allow_register,
        allow_password_registration,
        email_confirmation_required,
        allow_competition_history_purge: st.config.allow_competition_history_purge,
        enable_google_auth: oauth.google_configured(),
        enable_discord_auth: oauth.discord_configured(),
        donations_enabled,
        donation_provider: donations_enabled.then_some(donation_provider),
        donation_url: donations_enabled.then_some(donation_url).flatten(),
    }))
}

/// `GET /api/Posts` — the legacy array response, pinned first then newest.
///
/// Compatibility requires both the raw array shape and the complete retained
/// history. New bounded consumers use `/api/posts/page` instead.
pub async fn get_posts(
    State(st): State<SharedState>,
) -> AppResult<RequestResponse<Vec<PostInfoModel>>> {
    let data = load_all_posts(st.pg()).await?;
    Ok(RequestResponse::ok(data))
}

/// `GET /api/Posts/Latest` — the 20 most recent posts (pinned first).
pub async fn get_latest_posts(
    State(st): State<SharedState>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let data = load_post_page(st.pg(), 0, LATEST_POST_LIMIT).await?;
    conditional_post_feed_response(&headers, &data)
}

/// `GET /api/Posts/Page?count=&skip=` — a bounded page plus its exact total.
pub async fn get_posts_page(
    State(st): State<SharedState>,
    Query(page): Query<PostPageParams>,
) -> AppResult<ArrayResponse<PostInfoModel>> {
    // Count and page are intentionally separate: the ordered query can stop at
    // the requested slice instead of a window count forcing it through every
    // retained post before LIMIT. Only the scalar count sees full history.
    let total = count_posts(st.pg()).await?;
    let data = load_post_page(st.pg(), page.offset(), page.limit() as i64).await?;
    Ok(ArrayResponse::new(data, total))
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
pub async fn get_captcha(
    State(st): State<SharedState>,
) -> AppResult<RequestResponse<ClientCaptchaInfoModel>> {
    let settings = st
        .captcha_settings
        .load(st.pg(), st.config.account.use_captcha)
        .await?;
    // RSCTF `InfoController` (line 148): advertise the captcha provider to the
    // client ONLY when AccountPolicy.UseCaptcha is enabled. Otherwise the
    // login/register captcha widget still renders — and for HashPow it grinds a
    // (possibly very expensive) proof-of-work — even though captcha is turned off.
    let (type_, site_key) = if settings.use_captcha {
        (settings.provider, settings.site_key)
    } else {
        ("None".to_string(), None)
    };
    Ok(RequestResponse::ok(ClientCaptchaInfoModel {
        type_,
        site_key,
    }))
}

/// `GET /api/captcha/powchallenge` — proof-of-work challenge.
///
/// When the configured captcha provider is `HashPow`, mint a signed,
/// self-contained challenge. Issuance creates no per-challenge cache entry;
/// verification writes only a bounded one-use marker after valid work. For any other provider
/// (notably `None`) RSCTF has no PoW to issue and returns `404 NotFound`,
/// so we do the same.
///
pub async fn get_pow_challenge(State(st): State<SharedState>) -> AppResult<Response> {
    // Anonymous requests share one short-lived, invalidatable settings read.
    // Verification reloads the authoritative policy, so a changed revision
    // invalidates every outstanding challenge even across replicas.
    let settings = st
        .captcha_settings
        .load(st.pg(), st.config.account.use_captcha)
        .await?;
    let issued = crate::services::captcha::issue_hashpow_challenge(
        &settings,
        st.config.jwt_secret.as_bytes(),
        Utc::now().timestamp(),
    )?;

    Ok(hashpow_challenge_response(HashPowChallenge {
        id: issued.id,
        challenge: issued.challenge,
        difficulty: issued.difficulty as i32,
        expires_at: issued.expires_at.saturating_mul(1_000),
    }))
}

// --- helpers ---

/// Compatibility-only full-history read for the legacy raw-array endpoint.
/// Interactive clients must use `load_post_page` through `/api/posts/page`.
async fn load_all_posts(pool: &sqlx::PgPool) -> AppResult<Vec<PostInfoModel>> {
    load_posts(pool, 0, None).await
}

async fn load_post_page(
    pool: &sqlx::PgPool,
    offset: i64,
    limit: i64,
) -> AppResult<Vec<PostInfoModel>> {
    load_posts(
        pool,
        offset.max(0),
        Some(limit.clamp(1, MAX_POST_PAGE_SIZE as i64)),
    )
    .await
}

async fn load_posts(
    pool: &sqlx::PgPool,
    offset: i64,
    limit: Option<i64>,
) -> AppResult<Vec<PostInfoModel>> {
    let rows = sqlx::query_as::<_, PostInfoRow>(ORDERED_POST_PAGE_SQL)
        .bind(offset.max(0))
        // PostgreSQL treats LIMIT NULL as no limit. Only the compatibility
        // endpoint passes None; page/latest callers always bind a finite cap.
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(format!("load posts: {error}")))?;
    Ok(rows.into_iter().map(PostInfoModel::from).collect())
}

async fn count_posts(pool: &sqlx::PgPool) -> AppResult<i64> {
    sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*)::bigint FROM "Posts""#)
        .fetch_one(pool)
        .await
        .map_err(|error| AppError::internal(format!("count posts: {error}")))
}

impl From<PostInfoRow> for PostInfoModel {
    fn from(row: PostInfoRow) -> Self {
        Self {
            id: row.id,
            title: row.title,
            summary: row.summary,
            is_pinned: row.is_pinned,
            tags: parse_tags(row.tags),
            author_avatar: row
                .author_avatar_hash
                .map(|hash| format!("/assets/{hash}/avatar")),
            author_name: row.author_name,
            time: row.update_time_utc,
        }
    }
}

fn conditional_post_feed_response(
    request_headers: &HeaderMap,
    data: &[PostInfoModel],
) -> AppResult<Response> {
    let body = serde_json::to_vec(data)
        .map_err(|error| AppError::internal(format!("serialize latest posts: {error}")))?;
    // Weak is deliberate: response compression may change the transferred
    // bytes while the JSON representation and its cache validity are equal.
    let etag = format!("W/\"{}\"", codec::sha256_hex(&body));
    let etag_header = HeaderValue::from_str(&etag)
        .map_err(|error| AppError::internal(format!("build posts ETag: {error}")))?;

    if request_headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| etag_list_matches(value, &etag))
    {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        response.headers_mut().insert(header::ETAG, etag_header);
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(POST_FEED_CACHE_CONTROL),
        );
        return Ok(response);
    }

    let mut response = Body::from(body).into_response();
    response.headers_mut().insert(header::ETAG, etag_header);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(POST_FEED_CACHE_CONTROL),
    );
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok(response)
}

fn etag_list_matches(value: &str, current: &str) -> bool {
    let current = current.strip_prefix("W/").unwrap_or(current);
    value.split(',').any(|candidate| {
        let candidate = candidate.trim();
        candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == current
    })
}

fn parse_tags(tags: Option<serde_json::Value>) -> Option<Vec<String>> {
    tags.and_then(|v| serde_json::from_value(v).ok())
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
    use axum::response::IntoResponse;

    use super::{
        conditional_post_feed_response, effective_port_mapping, etag_list_matches,
        hashpow_challenge_response, HashPowChallenge, PostInfoModel, PostPageParams,
        MAX_POST_PAGE_SIZE, POST_FEED_CACHE_CONTROL,
    };
    use crate::utils::shared::{ArrayResponse, RequestResponse};

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

    #[test]
    fn post_pages_clamp_requested_work() {
        assert_eq!(PostPageParams { count: 0, skip: 0 }.limit(), 1);
        assert_eq!(PostPageParams { count: 10, skip: 0 }.limit(), 10);
        assert_eq!(
            PostPageParams {
                count: u64::MAX,
                skip: u64::MAX,
            }
            .limit(),
            MAX_POST_PAGE_SIZE
        );
        assert_eq!(
            PostPageParams {
                count: 10,
                skip: u64::MAX,
            }
            .offset(),
            i64::MAX
        );
    }

    #[tokio::test]
    async fn legacy_posts_are_a_raw_array_while_bounded_pages_are_explicit() {
        let legacy = RequestResponse::ok(Vec::<PostInfoModel>::new()).into_response();
        assert_eq!(legacy.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(legacy.into_body(), 64).await.unwrap().as_ref(),
            b"[]"
        );

        let bounded = ArrayResponse::new(Vec::<PostInfoModel>::new(), 123).into_response();
        let body = to_bytes(bounded.into_body(), 128).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["data"], serde_json::json!([]));
        assert_eq!(value["length"], 0);
        assert_eq!(value["total"], 123);
    }

    #[tokio::test]
    async fn hashpow_issuance_is_never_browser_or_shared_cacheable() {
        let response = hashpow_challenge_response(HashPowChallenge {
            id: "signed-id".to_string(),
            challenge: "0011223344556677".to_string(),
            difficulty: 18,
            expires_at: 1_700_000_000_000,
        });
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "private, no-store"
        );
        assert_eq!(response.headers()[header::PRAGMA], "no-cache");

        let body = to_bytes(response.into_body(), 1_024).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["id"], "signed-id");
        assert_eq!(value["expiresAt"], 1_700_000_000_000_i64);
    }

    #[test]
    fn conditional_validator_accepts_weak_or_strong_equivalents() {
        assert!(etag_list_matches("W/\"feed\"", "W/\"feed\""));
        assert!(etag_list_matches("\"old\", \"feed\"", "W/\"feed\""));
        assert!(etag_list_matches("*", "W/\"feed\""));
        assert!(!etag_list_matches("\"other\"", "W/\"feed\""));
    }

    #[tokio::test]
    async fn unchanged_latest_feed_returns_no_body() {
        let data = Vec::<PostInfoModel>::new();
        let first = conditional_post_feed_response(&HeaderMap::new(), &data).unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(
            first.headers()[header::CACHE_CONTROL],
            POST_FEED_CACHE_CONTROL
        );
        let etag = first.headers()[header::ETAG].clone();
        assert_eq!(
            to_bytes(first.into_body(), 16).await.unwrap().as_ref(),
            b"[]"
        );

        let mut headers = HeaderMap::new();
        headers.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_bytes(etag.as_bytes()).unwrap(),
        );
        let unchanged = conditional_post_feed_response(&headers, &data).unwrap();
        assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(to_bytes(unchanged.into_body(), 16).await.unwrap().len(), 0);
    }
}

#[cfg(test)]
#[path = "info_tests.rs"]
mod database_tests;
