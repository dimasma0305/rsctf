use crate::utils::error::{AppError, AppResult};

pub(super) fn validate_subpath(subpath: Option<&str>) -> AppResult<Option<std::path::PathBuf>> {
    let Some(subpath) = subpath.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let relative = std::path::Path::new(subpath);
    for component in relative.components() {
        match component {
            std::path::Component::Normal(_) | std::path::Component::CurDir => {}
            _ => return Err(AppError::bad_request("invalid subpath")),
        }
    }
    Ok(Some(relative.to_path_buf()))
}

/// Resolve a validated subpath after clone, following repository symlinks and
/// requiring the resulting directory to remain under the canonical checkout.
pub(super) fn resolve_subpath(
    base: &std::path::Path,
    subpath: Option<&std::path::Path>,
) -> AppResult<std::path::PathBuf> {
    let root = std::fs::canonicalize(base)
        .map_err(|error| AppError::internal(format!("canonicalize checkout: {error}")))?;
    let candidate = match subpath {
        Some(relative) => std::fs::canonicalize(base.join(relative))
            .map_err(|_| AppError::bad_request("repository subpath does not exist"))?,
        None => root.clone(),
    };
    if !candidate.starts_with(&root) {
        return Err(AppError::bad_request(
            "repository subpath escapes the checkout",
        ));
    }
    Ok(candidate)
}
