//! AI chat share-link validation.
//!
//! The server never fetches a submitted link. A link is accepted only when its
//! normalized form matches an enabled provider pattern: a built-in matcher
//! defined here (so a release can correct it) or an admin-defined custom regex.
//! Patterns are matched against the normalized URL, anchored as `^(?:…)$`, and
//! must stay compatible with JavaScript `u` regexes so the browser's live
//! matcher and this authoritative check agree.

use regex::{Regex, RegexBuilder};
use serde::Serialize;
use sqlx::PgPool;

use crate::utils::error::{AppError, AppResult};

pub const MAX_LINKS_PER_SOLVE: usize = 5;
pub const MAX_URL_LENGTH: usize = 2048;
pub const MAX_CUSTOM_PROVIDERS: i64 = 32;
pub const MAX_PATTERN_LENGTH: usize = 512;
pub const MAX_LABEL_LENGTH: usize = 64;
const REGEX_SIZE_LIMIT: usize = 256 * 1024;

/// Query strings are allowed after every built-in path. The normalized URL is
/// percent-encoded ASCII, so printable ASCII covers every valid query.
macro_rules! query {
    () => {
        r"(?:\?[!-~]*)?"
    };
}

pub struct BuiltinProvider {
    pub key: &'static str,
    pub label: &'static str,
    pub pattern: &'static str,
    pub examples: &'static [&'static str],
}

/// Share-link formats confirmed against each provider's public documentation
/// or live share pages at release time. Order decides the winner on overlap.
pub const BUILTIN_PROVIDERS: &[BuiltinProvider] = &[
    BuiltinProvider {
        key: "chatgpt",
        label: "ChatGPT",
        pattern: concat!(
            r"https://(?:chatgpt\.com|chat\.openai\.com)/share/[A-Za-z0-9-]{8,128}",
            query!()
        ),
        examples: &["https://chatgpt.com/share/6708d9f0-5b7c-8008-a2d4-3f2e1c0b9a77"],
    },
    BuiltinProvider {
        key: "claude",
        label: "Claude",
        pattern: concat!(r"https://claude\.ai/share/[A-Za-z0-9-]{8,128}", query!()),
        examples: &["https://claude.ai/share/2f1c9e4a-8b7d-4c3e-9f60-1a2b3c4d5e6f"],
    },
    BuiltinProvider {
        key: "gemini",
        label: "Gemini",
        pattern: concat!(
            r"https://(?:gemini\.google\.com/share|g\.co/gemini/share)/[A-Za-z0-9_-]{6,128}",
            query!()
        ),
        examples: &[
            "https://g.co/gemini/share/68a1b2c3d4e5",
            "https://gemini.google.com/share/68a1b2c3d4e5",
        ],
    },
    BuiltinProvider {
        key: "grok",
        label: "Grok",
        pattern: concat!(r"https://grok\.com/share/[A-Za-z0-9_-]{8,160}", query!()),
        examples: &["https://grok.com/share/bGVnYWN5_893dfa29-be71-4e9d-8b77-22dbf9f4c5b1"],
    },
    BuiltinProvider {
        key: "deepseek",
        label: "DeepSeek",
        pattern: concat!(
            r"https://chat\.deepseek\.com/share/[A-Za-z0-9_-]{6,128}",
            query!()
        ),
        examples: &["https://chat.deepseek.com/share/k3v9x2m7q1p8z4"],
    },
    BuiltinProvider {
        key: "perplexity",
        label: "Perplexity",
        pattern: concat!(
            r"https://(?:www\.)?perplexity\.ai/search/[A-Za-z0-9._~%-]{6,256}",
            query!()
        ),
        examples: &["https://www.perplexity.ai/search/how-does-rsa-padding-work-Qx7mB2kTRyW1n0s"],
    },
    BuiltinProvider {
        key: "kimi",
        label: "Kimi",
        pattern: concat!(
            r"https://(?:www\.)?kimi\.com/share/[A-Za-z0-9_-]{6,128}",
            query!()
        ),
        examples: &["https://www.kimi.com/share/d2k4m6n8p0r2t4v6"],
    },
    BuiltinProvider {
        key: "huggingchat",
        label: "HuggingChat",
        pattern: concat!(
            r"https://(?:hf\.co|huggingface\.co)/chat/r/[A-Za-z0-9_-]{4,64}",
            query!()
        ),
        examples: &["https://hf.co/chat/r/S3i2seZ"],
    },
];

pub fn builtin(key: &str) -> Option<&'static BuiltinProvider> {
    BUILTIN_PROVIDERS
        .iter()
        .find(|provider| provider.key == key)
}

/// One enabled provider, ready to match.
pub struct CompiledProvider {
    pub key: String,
    pub label: String,
    pub pattern: String,
    regex: Regex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchedLink {
    pub url: String,
    pub provider_key: String,
    pub provider_label: String,
}

/// Why a link was rejected; rendered as the user-facing reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkRejection {
    NotAUrl,
    NotHttps,
    Credentials,
    Port,
    NoHost,
    TooLong,
    UnsupportedProvider,
}

impl LinkRejection {
    pub fn message(self) -> &'static str {
        match self {
            Self::NotAUrl => "this is not a valid URL",
            Self::NotHttps => "only https:// links are accepted",
            Self::Credentials => "links must not contain a username or password",
            Self::Port => "links must not specify a port",
            Self::NoHost => "the link has no host name",
            Self::TooLong => "the link is longer than 2048 characters",
            Self::UnsupportedProvider => "this AI provider or link format is not accepted",
        }
    }
}

/// Normalize a submitted link: https only, no userinfo, no explicit port,
/// lowercase domain (the parser does this), no fragment. Path and query are
/// kept exactly as the parser serializes them.
pub fn normalize(input: &str) -> Result<String, LinkRejection> {
    let trimmed = input.trim();
    if trimmed.len() > MAX_URL_LENGTH * 2 {
        return Err(LinkRejection::TooLong);
    }
    let mut url = url::Url::parse(trimmed).map_err(|_| LinkRejection::NotAUrl)?;
    if url.scheme() != "https" {
        return Err(LinkRejection::NotHttps);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(LinkRejection::Credentials);
    }
    if url.port().is_some() {
        return Err(LinkRejection::Port);
    }
    match url.host() {
        Some(url::Host::Domain(_)) => {}
        Some(_) => return Err(LinkRejection::UnsupportedProvider),
        None => return Err(LinkRejection::NoHost),
    }
    url.set_fragment(None);
    let normalized = String::from(url);
    if normalized.len() > MAX_URL_LENGTH {
        return Err(LinkRejection::TooLong);
    }
    Ok(normalized)
}

fn compile(pattern: &str) -> Result<Regex, String> {
    RegexBuilder::new(&format!("^(?:{pattern})$"))
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .nest_limit(64)
        .build()
        .map_err(|error| match error {
            regex::Error::CompiledTooBig(_) => "the pattern is too complex".to_string(),
            regex::Error::Syntax(message) => {
                let first = message.lines().last().unwrap_or("invalid syntax").trim();
                format!("the pattern is not a valid regular expression ({first})")
            }
            _ => "the pattern is not a valid regular expression".to_string(),
        })
}

/// A custom pattern must begin with `https://`, an optional `(?:www\.)?`,
/// then an escaped literal host and `/`. This keeps every custom provider tied
/// to one site and prevents catch-all patterns.
fn literal_host_prefix(pattern: &str) -> Option<String> {
    let rest = pattern.strip_prefix("https://")?;
    let rest = rest.strip_prefix(r"(?:www\.)?").unwrap_or(rest);
    let end = rest.find('/')?;
    let host = &rest[..end];
    let labels = host.split(r"\.").collect::<Vec<_>>();
    if labels.len() < 2 {
        return None;
    }
    let valid_label = |label: &str| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    labels
        .iter()
        .all(|label| valid_label(label))
        .then(|| labels.join("."))
}

/// True when `pattern` contains an unescaped `|` outside every group and
/// bracket class. Such an alternation would let a later branch escape the
/// site-bound prefix, e.g. `https://poe\.com/s/x|https://.*`.
fn has_top_level_alternation(pattern: &str) -> bool {
    let (mut depth, mut in_class, mut escaped) = (0usize, false, false);
    for ch in pattern.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            '(' if !in_class => depth += 1,
            ')' if !in_class => depth = depth.saturating_sub(1),
            '|' if !in_class && depth == 0 => return true,
            _ => {}
        }
    }
    false
}

pub fn validate_custom_key(key: &str) -> AppResult<()> {
    let valid = !key.is_empty()
        && key.len() <= 40
        && key
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !key.starts_with('-');
    if !valid {
        return Err(AppError::bad_request(
            "Provider key must be 1-40 lowercase letters, digits, or hyphens and start with a letter or digit",
        ));
    }
    if builtin(key).is_some() {
        return Err(AppError::bad_request(
            "This key belongs to a built-in provider; choose another key",
        ));
    }
    Ok(())
}

pub fn validate_custom_label(label: &str) -> AppResult<String> {
    let label = label.trim();
    if label.is_empty() || label.chars().count() > MAX_LABEL_LENGTH {
        return Err(AppError::bad_request(
            "Provider label must be 1-64 characters",
        ));
    }
    if label.chars().any(char::is_control) {
        return Err(AppError::bad_request(
            "Provider label must not contain control characters",
        ));
    }
    Ok(label.to_string())
}

pub fn validate_custom_pattern(pattern: &str) -> AppResult<String> {
    let pattern = pattern.trim();
    if pattern.is_empty() || pattern.len() > MAX_PATTERN_LENGTH {
        return Err(AppError::bad_request(
            "Provider pattern must be 1-512 characters",
        ));
    }
    // Keep the syntax portable to the browser's `u` regexes: no inline flags,
    // lookaround, or named-group variants, and no POSIX bracket classes.
    if pattern
        .match_indices("(?")
        .any(|(index, _)| !pattern[index + 2..].starts_with(':'))
        || pattern.contains("[[:")
    {
        return Err(AppError::bad_request(
            "Provider pattern may use only (?:…) groups; inline flags, lookaround, and POSIX classes are not supported",
        ));
    }
    if pattern.starts_with('^') || pattern.ends_with('$') {
        return Err(AppError::bad_request(
            "Omit ^ and $: every pattern is matched against the whole normalized URL",
        ));
    }
    if has_top_level_alternation(pattern) {
        return Err(AppError::bad_request(
            "Wrap alternatives in a group, e.g. https://example\\.com/(?:a|b)/…; a top-level | would match other sites",
        ));
    }
    if literal_host_prefix(pattern).is_none() {
        return Err(AppError::bad_request(
            r"Provider pattern must start with https://, an escaped literal host such as chat\.example\.com, and /",
        ));
    }
    compile(pattern).map_err(|reason| AppError::bad_request(format!("Provider {reason}")))?;
    Ok(pattern.to_string())
}

#[derive(sqlx::FromRow)]
pub struct ProviderRow {
    pub provider_key: String,
    pub builtin: bool,
    pub label: Option<String>,
    pub pattern: Option<String>,
    pub enabled: bool,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

pub async fn load_rows(pool: &PgPool) -> AppResult<Vec<ProviderRow>> {
    sqlx::query_as::<_, ProviderRow>(
        r#"SELECT provider_key, builtin, label, pattern, enabled, updated_at
             FROM "AiChatProviders"
            ORDER BY provider_key
            LIMIT $1"#,
    )
    .bind(MAX_CUSTOM_PROVIDERS + BUILTIN_PROVIDERS.len() as i64)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Every enabled provider in matching order: built-ins first, then custom
/// providers by key. A stored custom pattern that no longer compiles is
/// skipped rather than failing every save.
pub fn enabled_providers(rows: &[ProviderRow]) -> Vec<CompiledProvider> {
    let mut providers = Vec::new();
    for provider in BUILTIN_PROVIDERS {
        let enabled = rows
            .iter()
            .find(|row| row.builtin && row.provider_key == provider.key)
            .is_none_or(|row| row.enabled);
        if enabled {
            if let Ok(regex) = compile(provider.pattern) {
                providers.push(CompiledProvider {
                    key: provider.key.to_string(),
                    label: provider.label.to_string(),
                    pattern: provider.pattern.to_string(),
                    regex,
                });
            }
        }
    }
    for row in rows.iter().filter(|row| !row.builtin && row.enabled) {
        let (Some(label), Some(pattern)) = (&row.label, &row.pattern) else {
            continue;
        };
        match compile(pattern) {
            Ok(regex) => providers.push(CompiledProvider {
                key: row.provider_key.clone(),
                label: label.clone(),
                pattern: pattern.clone(),
                regex,
            }),
            Err(reason) => {
                tracing::warn!(key = %row.provider_key, %reason, "skipping invalid AI chat provider")
            }
        }
    }
    providers
}

pub fn match_link(
    input: &str,
    providers: &[CompiledProvider],
) -> Result<MatchedLink, LinkRejection> {
    let url = normalize(input)?;
    providers
        .iter()
        .find(|provider| provider.regex.is_match(&url))
        .map(|provider| MatchedLink {
            url: url.clone(),
            provider_key: provider.key.clone(),
            provider_label: provider.label.clone(),
        })
        .ok_or(LinkRejection::UnsupportedProvider)
}

/// Validate a whole submission: bounded count, each link accepted, no
/// duplicates after normalization. Errors name the 1-based link number.
pub fn validate_submission(
    inputs: &[String],
    providers: &[CompiledProvider],
) -> AppResult<Vec<MatchedLink>> {
    if inputs.len() > MAX_LINKS_PER_SOLVE {
        return Err(AppError::bad_request(format!(
            "Attach at most {MAX_LINKS_PER_SOLVE} AI chat links per challenge"
        )));
    }
    let mut links: Vec<MatchedLink> = Vec::with_capacity(inputs.len());
    for (index, input) in inputs.iter().enumerate() {
        let link = match_link(input, providers).map_err(|rejection| {
            AppError::bad_request(format!("Link {}: {}", index + 1, rejection.message()))
        })?;
        if links.iter().any(|existing| existing.url == link.url) {
            return Err(AppError::bad_request(format!(
                "Link {}: this link is already listed",
                index + 1
            )));
        }
        links.push(link);
    }
    Ok(links)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtins() -> Vec<CompiledProvider> {
        enabled_providers(&[])
    }

    #[test]
    fn every_builtin_compiles_and_matches_its_examples() {
        let providers = builtins();
        assert_eq!(providers.len(), BUILTIN_PROVIDERS.len());
        for provider in BUILTIN_PROVIDERS {
            assert!(
                !provider.examples.is_empty(),
                "{} has no examples",
                provider.key
            );
            for example in provider.examples {
                let matched = match_link(example, &providers)
                    .unwrap_or_else(|reason| panic!("{example}: {reason:?}"));
                assert_eq!(matched.provider_key, provider.key, "{example}");
            }
        }
    }

    #[test]
    fn near_misses_and_unsafe_urls_are_rejected() {
        let providers = builtins();
        for (input, reason) in [
            (
                "http://chatgpt.com/share/6708d9f0-5b7c-8008",
                LinkRejection::NotHttps,
            ),
            (
                "https://chatgpt.com/c/6708d9f0-5b7c-8008",
                LinkRejection::UnsupportedProvider,
            ),
            (
                "https://chatgpt.com.evil.test/share/6708d9f0-5b7c",
                LinkRejection::UnsupportedProvider,
            ),
            (
                "https://evilchatgpt.com/share/6708d9f0-5b7c",
                LinkRejection::UnsupportedProvider,
            ),
            (
                "https://user:pw@claude.ai/share/2f1c9e4a-8b7d",
                LinkRejection::Credentials,
            ),
            (
                "https://claude.ai:8443/share/2f1c9e4a-8b7d",
                LinkRejection::Port,
            ),
            (
                "https://claude.ai/share/../../settings",
                LinkRejection::UnsupportedProvider,
            ),
            (
                "https://1.2.3.4/share/abcdefgh",
                LinkRejection::UnsupportedProvider,
            ),
            ("javascript:alert(1)", LinkRejection::NotHttps),
            ("not a url", LinkRejection::NotAUrl),
            (
                "https://claude.ai/share/short",
                LinkRejection::UnsupportedProvider,
            ),
        ] {
            assert_eq!(match_link(input, &providers), Err(reason), "{input}");
        }
    }

    #[test]
    fn normalization_lowercases_host_and_strips_fragment() {
        let providers = builtins();
        let matched = match_link(
            "  https://Claude.AI/share/2f1c9e4a-8b7d-4c3e#section  ",
            &providers,
        )
        .unwrap();
        assert_eq!(matched.url, "https://claude.ai/share/2f1c9e4a-8b7d-4c3e");
        assert_eq!(
            normalize("https://chatgpt.com:443/share/abc12345").unwrap(),
            "https://chatgpt.com/share/abc12345",
            "the default port is not an explicit port"
        );
    }

    #[test]
    fn submissions_are_bounded_and_deduplicated() {
        let providers = builtins();
        let link = "https://claude.ai/share/2f1c9e4a-8b7d-4c3e".to_string();
        let error =
            validate_submission(&[link.clone(), format!("{link}#x")], &providers).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Link 2: this link is already listed"),
            "{error}"
        );
        let error = validate_submission(&vec![link.clone(); 6], &providers).unwrap_err();
        assert!(error.to_string().contains("at most 5"), "{error}");
        let error = validate_submission(&[link, "https://example.test/share/x".into()], &providers)
            .unwrap_err();
        assert!(
            error.to_string().contains("Link 2: this AI provider"),
            "{error}"
        );
        assert!(validate_submission(&[], &providers).unwrap().is_empty());
    }

    #[test]
    fn disabled_builtins_and_custom_providers_change_the_matcher() {
        let now = chrono::Utc::now();
        let rows = vec![
            ProviderRow {
                provider_key: "chatgpt".into(),
                builtin: true,
                label: None,
                pattern: None,
                enabled: false,
                updated_at: now,
            },
            ProviderRow {
                provider_key: "copilot".into(),
                builtin: false,
                label: Some("Copilot".into()),
                pattern: Some(
                    r"https://copilot\.microsoft\.com/shares/[A-Za-z0-9_-]{6,128}".into(),
                ),
                enabled: true,
                updated_at: now,
            },
            ProviderRow {
                provider_key: "broken".into(),
                builtin: false,
                label: Some("Broken".into()),
                pattern: Some("https://x\\.test/(".into()),
                enabled: true,
                updated_at: now,
            },
        ];
        let providers = enabled_providers(&rows);
        assert!(providers.iter().all(|provider| provider.key != "chatgpt"));
        assert!(providers.iter().all(|provider| provider.key != "broken"));
        assert_eq!(
            match_link("https://chatgpt.com/share/6708d9f0-5b7c-8008", &providers),
            Err(LinkRejection::UnsupportedProvider)
        );
        assert_eq!(
            match_link("https://copilot.microsoft.com/shares/AbCdEf12", &providers)
                .unwrap()
                .provider_label,
            "Copilot"
        );
    }

    #[test]
    fn custom_patterns_must_be_site_bound_and_compilable() {
        assert!(validate_custom_pattern(
            r"https://copilot\.microsoft\.com/shares/[A-Za-z0-9]{6,64}"
        )
        .is_ok());
        assert!(validate_custom_pattern(r"https://(?:www\.)?poe\.com/s/[A-Za-z0-9]{6,64}").is_ok());
        assert!(
            validate_custom_pattern(r"https://poe\.com/(?:s|share)/[a-z|]{6,64}").is_ok(),
            "alternation inside a group or class is site-bound"
        );
        assert!(has_top_level_alternation(r"https://a\.com/x|y"));
        assert!(!has_top_level_alternation(r"https://a\.com/x\|y"));
        for bad in [
            r"https://.*",
            r".*",
            r"http://poe\.com/s/.*",
            r"https://poe.com/s/.*",
            r"https://poe\.com",
            r"^https://poe\.com/s/.*$",
            r"https://poe\.com/s/(unclosed",
            r"https://poe\.com/s/(?=lookahead)",
            r"https://poe\.com/s/(a)\1",
            r"https://poe\.com/s/(?i)abc",
            r"https://poe\.com/s/(?P<id>[a-z]+)",
            r"https://poe\.com/s/[[:alpha:]]+",
            r"https://poe\.com/s/x|https://.*",
            r"https://poe\.com/s/(?:a)|.*",
        ] {
            assert!(
                validate_custom_pattern(bad).is_err(),
                "{bad} should be rejected"
            );
        }
        assert!(
            validate_custom_pattern(&format!(r"https://poe\.com/{}", "a".repeat(600))).is_err()
        );
        assert!(validate_custom_key("copilot").is_ok());
        assert!(validate_custom_key("chatgpt").is_err());
        assert!(validate_custom_key("Bad Key").is_err());
        assert!(validate_custom_label("  ").is_err());
    }
}
