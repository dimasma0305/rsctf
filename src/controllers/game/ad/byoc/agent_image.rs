//! Immutable BYOC agent image selection.

/// Official server images compile in the agent digest built by the same
/// workflow. A direct source build has no safe fallback: silently emitting an
/// older digest would pair an ACK-requiring server with an ACK-less agent.
pub(super) fn default_byoc_agent_image() -> Option<(&'static str, bool)> {
    let image = option_env!("RSCTF_DEFAULT_BYOC_AGENT_IMAGE")
        .unwrap_or("")
        .trim();
    if image.is_empty() {
        return None;
    }
    let multiarch = option_env!("RSCTF_DEFAULT_BYOC_AGENT_MULTIARCH") == Some("true");
    Some((image, !multiarch))
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
