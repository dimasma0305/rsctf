//! Anonymous-only metadata: never project private event or player information.
use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{header, Request, StatusCode, Uri},
    middleware::Next,
    response::{Html, IntoResponse, Response},
};
use percent_encoding::percent_decode_str;
use url::Url;

use crate::app_state::SharedState;

const DEFAULT_DESCRIPTION: &str =
    "Practice cybersecurity, join capture-the-flag competitions, and learn with your team.";
const START: &str = "<!-- rsctf-seo:start -->";
const END: &str = "<!-- rsctf-seo:end -->";
const PUBLIC_PAGES: &[(&str, &str)] = &[
    ("/", ""),
    ("/games", "Games"),
    ("/posts", "News"),
    ("/about", "About"),
    ("/guide", "Player guide"),
];
const SITEMAP_LIMIT: i64 = 1000;

#[derive(Debug)]
struct Metadata {
    title: String,
    description: String,
    canonical: Option<String>,
    image: Option<String>,
    indexable: bool,
    status: StatusCode,
}

fn text(value: &str, limit: usize) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(limit)
        .collect()
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

// Use the configured origin, never Host or forwarded headers from the request.
fn origin(state: &SharedState) -> Option<Url> {
    let mut url = Url::parse(state.config.public_url.as_deref()?).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Some(url)
}

fn absolute(base: &Option<Url>, path: &str) -> Option<String> {
    let mut url = base.clone()?;
    url.set_path(path);
    Some(url.to_string())
}

fn detail_path(kind: &str, id: &str) -> String {
    let mut url = Url::parse("https://unused.invalid").expect("constant URL");
    url.path_segments_mut()
        .expect("constant base")
        .clear()
        .push(kind)
        .push(id);
    url.path().to_owned()
}

async fn metadata(state: &SharedState, uri: &Uri) -> Result<Metadata, sqlx::Error> {
    let values = sqlx::query_as::<_, (String, Option<String>)>(
        r#"SELECT config_key, LEFT(value, 512) FROM "Configs" WHERE config_key = ANY($1)"#,
    )
    .bind([
        "GlobalConfig:Title",
        "GlobalConfig:Description",
        "GlobalConfig:LogoHash",
    ])
    .fetch_all(state.pg())
    .await?;
    let get = |key: &str| {
        values
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.as_deref())
            .filter(|v| !v.trim().is_empty())
    };
    let title = text(
        get("GlobalConfig:Title").unwrap_or(&state.config.global.title),
        120,
    );
    let platform = format!("{}::CTF", if title.is_empty() { "RS" } else { &title });
    let base = origin(state);
    let logo = get("GlobalConfig:LogoHash")
        .map(|hash| format!("/assets/{hash}/logo"))
        .unwrap_or_else(|| "/favicon.png".into());
    let mut result = Metadata {
        title: platform.clone(),
        description: text(
            get("GlobalConfig:Description").unwrap_or(DEFAULT_DESCRIPTION),
            320,
        ),
        canonical: None,
        image: absolute(&base, &logo),
        indexable: false,
        status: StatusCode::OK,
    };
    let path = uri.path().trim_end_matches('/');
    let path = if path.is_empty() { "/" } else { path };
    if let Some((canonical, title)) = PUBLIC_PAGES.iter().find(|(p, _)| *p == path) {
        result.indexable = true;
        result.canonical = absolute(&base, canonical);
        if !title.is_empty() {
            result.title = format!("{title} - {platform}");
        }
        return Ok(result);
    }
    let segments: Vec<_> = path.split('/').collect();
    let detail = match segments.as_slice() {
        ["", "games", id] => {
            if let Ok(id) = id.parse::<i32>() {
                sqlx::query_as::<_, (String, String, Option<String>)>(r#"SELECT LEFT(title, 120), LEFT(summary, 320), poster_hash FROM "Games" WHERE id = $1 AND hidden = FALSE"#)
                    .bind(id).fetch_optional(state.pg()).await?
                    .map(|(title, summary, poster)| (title, summary, poster.map(|hash| format!("/assets/{hash}/poster")), format!("/games/{id}")))
            } else {
                None
            }
        }
        ["", "posts", id] => {
            let decoded = percent_decode_str(id).decode_utf8().ok();
            if let Some(id) = decoded.filter(|id| id.len() == 8) {
                sqlx::query_as::<_, (String, String)>(
                    r#"SELECT LEFT(title, 120), LEFT(summary, 320) FROM "Posts" WHERE id = $1"#,
                )
                .bind(id.as_ref())
                .fetch_optional(state.pg())
                .await?
                .map(|(title, summary)| (title, summary, None, detail_path("posts", &id)))
            } else {
                None
            }
        }
        _ => {
            // Valid application workspaces remain usable, but cannot be indexed.
            let private = [
                "/admin",
                "/account",
                "/teams",
                "/challenges",
                "/donations",
                "/games/",
                "/posts/",
            ]
            .iter()
            .any(|prefix| {
                path == *prefix || path.starts_with(&format!("{}/", prefix.trim_end_matches('/')))
            });
            if !private {
                result.status = StatusCode::NOT_FOUND;
            }
            return Ok(result);
        }
    };
    if let Some((title, summary, image, canonical)) = detail {
        result.title = format!("{} - {platform}", text(&title, 120));
        if !summary.trim().is_empty() {
            result.description = text(&summary, 320);
        }
        if let Some(image) = image {
            result.image = absolute(&base, &image);
        }
        result.canonical = absolute(&base, &canonical);
        result.indexable = true;
    } else {
        result.status = StatusCode::NOT_FOUND;
    }
    Ok(result)
}

fn render(template: &str, meta: &Metadata) -> String {
    let robots = if meta.indexable {
        "index, follow"
    } else {
        "noindex, nofollow"
    };
    let title = escape(&meta.title);
    let description = escape(&meta.description);
    let mut head = format!("{START}\n<title>{title}</title>\n<meta name=\"description\" content=\"{description}\">\n<meta name=\"robots\" content=\"{robots}\">\n<meta property=\"og:type\" content=\"website\">\n<meta property=\"og:title\" content=\"{title}\">\n<meta property=\"og:description\" content=\"{description}\">\n<meta name=\"twitter:card\" content=\"summary\">\n<meta name=\"twitter:title\" content=\"{title}\">\n<meta name=\"twitter:description\" content=\"{description}\">\n");
    if let Some(url) = &meta.canonical {
        let url = escape(url);
        head.push_str(&format!("<link rel=\"canonical\" href=\"{url}\">\n<meta property=\"og:url\" content=\"{url}\">\n"));
    }
    if let Some(url) = &meta.image {
        let url = escape(url);
        head.push_str(&format!("<meta property=\"og:image\" content=\"{url}\">\n<meta name=\"twitter:image\" content=\"{url}\">\n"));
    }
    head.push_str(END);
    let mut html = template.to_owned();
    if let Some((start, end)) = template.find(START).zip(template.find(END)) {
        if end > start {
            html.replace_range(start..end + END.len(), &head);
        }
    }
    html
}

pub(super) async fn page(state: SharedState, template: Arc<String>, uri: Uri) -> Response {
    let meta = match metadata(&state, &uri).await {
        Ok(meta) => meta,
        Err(error) => {
            tracing::warn!(%error, "SEO metadata unavailable");
            Metadata {
                title: "RS::CTF".into(),
                description: DEFAULT_DESCRIPTION.into(),
                canonical: None,
                image: None,
                indexable: false,
                status: StatusCode::SERVICE_UNAVAILABLE,
            }
        }
    };
    let mut response = (meta.status, Html(render(&template, &meta))).into_response();
    // Hidden-event changes must not leave stale public previews in shared caches.
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    if !meta.indexable {
        response
            .headers_mut()
            .insert("x-robots-tag", "noindex, nofollow".parse().unwrap());
    }
    response
}

pub(super) async fn robots(State(state): State<SharedState>) -> Response {
    // Do not Disallow private HTML: crawlers must fetch it to see noindex.
    // Public API reads must also stay crawlable so JavaScript rendering works.
    let mut body = "User-agent: *\nDisallow: /hub/\n".to_owned();
    if let Some(url) = absolute(&origin(&state), "/sitemap.xml") {
        body.push_str(&format!("Sitemap: {url}\n"));
    }
    (
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

pub(super) async fn exclude_api_indexing(request: Request<Body>, next: Next) -> Response {
    let api = request.uri().path() == "/api" || request.uri().path().starts_with("/api/");
    let mut response = next.run(request).await;
    if api {
        response
            .headers_mut()
            .insert("x-robots-tag", "noindex, nofollow".parse().unwrap());
    }
    response
}

pub(super) async fn sitemap(State(state): State<SharedState>) -> Response {
    let Some(base) = origin(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Set RSCTF_PUBLIC_URL to enable the sitemap.",
        )
            .into_response();
    };
    match sitemap_xml(state.pg(), base).await {
        Ok(xml) => (
            [
                (header::CONTENT_TYPE, "application/xml; charset=utf-8"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            xml,
        )
            .into_response(),
        Err(error) => {
            tracing::warn!(%error, "sitemap unavailable");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "Sitemap temporarily unavailable.",
            )
                .into_response()
        }
    }
}

async fn sitemap_xml(pool: &sqlx::PgPool, base: Url) -> Result<String, sqlx::Error> {
    let games = sqlx::query_scalar::<_, i32>(
        r#"SELECT id FROM "Games" WHERE hidden = FALSE ORDER BY id DESC LIMIT $1"#,
    )
    .bind(SITEMAP_LIMIT)
    .fetch_all(pool)
    .await?;
    let posts =
        sqlx::query_scalar::<_, String>(r#"SELECT id FROM "Posts" ORDER BY id DESC LIMIT $1"#)
            .bind(SITEMAP_LIMIT)
            .fetch_all(pool)
            .await?;
    let paths = PUBLIC_PAGES
        .iter()
        .map(|(path, _)| path.to_string())
        .chain(games.into_iter().map(|id| format!("/games/{id}")))
        .chain(posts.iter().map(|id| detail_path("posts", id)));
    let mut xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n".to_owned();
    let base = Some(base);
    for path in paths {
        if let Some(url) = absolute(&base, &path) {
            xml.push_str(&format!("<url><loc>{}</loc></url>\n", escape(&url)));
        }
    }
    xml.push_str("</urlset>\n");
    Ok(xml)
}

#[cfg(test)]
mod tests;
