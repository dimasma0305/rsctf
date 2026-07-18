//! Narrow validation helpers for attacker-controlled archive metadata.

use std::io::Read;
use std::path::{Component, PathBuf};

/// Return a ZIP entry's path only when its raw name is already the exact,
/// portable canonical spelling of the enclosed path.
///
/// ZIP 8 accepts internal parent components such as `dir/../file` when they do
/// not escape the archive root. Extraction and review paths intentionally reject
/// those aliases, along with backslashes, repeated separators, and `.` segments,
/// so one file has exactly one unambiguous name.
pub(crate) fn canonical_zip_entry_path<R: Read + ?Sized>(
    entry: &::zip::read::ZipFile<'_, R>,
) -> Option<PathBuf> {
    let raw_name = entry.name();
    if raw_name.contains('\\') {
        return None;
    }

    let enclosed = entry.enclosed_name()?;
    let mut components = Vec::new();
    for component in enclosed.components() {
        let Component::Normal(component) = component else {
            return None;
        };
        components.push(component.to_str()?);
    }
    if components.is_empty() {
        return None;
    }

    let canonical = components.join("/");
    let expected_name = if entry.is_dir() {
        format!("{canonical}/")
    } else {
        canonical.clone()
    };
    if raw_name != expected_name || enclosed != PathBuf::from(&canonical) {
        return None;
    }
    Some(enclosed)
}
