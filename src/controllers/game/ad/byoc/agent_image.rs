//! Immutable BYOC agent image selection.

/// Official server images receive the agent digest built by the same workflow
/// as immutable runtime metadata. Keeping it out of Rust compilation lets an
/// identical source tree reuse the expensive release-build layer when only the
/// companion image digest changes.
pub(super) fn default_byoc_agent_image() -> Option<(String, bool)> {
    let configured = std::env::var("RSCTF_DEFAULT_BYOC_AGENT_IMAGE").ok();
    let multiarch = std::env::var("RSCTF_DEFAULT_BYOC_AGENT_MULTIARCH").ok();
    default_byoc_agent_image_from(configured.as_deref(), multiarch.as_deref())
}

pub(super) fn default_byoc_agent_image_from(
    configured: Option<&str>,
    multiarch: Option<&str>,
) -> Option<(String, bool)> {
    let image = configured?.trim();
    if image.is_empty() {
        return None;
    }
    Some((image.to_owned(), multiarch != Some("true")))
}

pub(super) fn immutable_agent_image(value: &str) -> Option<String> {
    let value = value.trim();
    let (repository, digest) = value.rsplit_once("@sha256:")?;
    if repository.is_empty()
        || repository.chars().any(char::is_whitespace)
        || digest.len() != 64
        || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    Some(format!(
        "{repository}@sha256:{}",
        digest.to_ascii_lowercase()
    ))
}
