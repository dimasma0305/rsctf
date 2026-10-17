//! Narrow validation helpers for attacker-controlled archive metadata.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// Return a ZIP entry's path only when its raw name is already the exact,
/// portable canonical spelling of the enclosed path.
///
/// ZIP accepts internal parent components such as `dir/../file` when they do
/// not escape the archive root. Extraction and review paths intentionally reject
/// those aliases, along with backslashes, repeated separators, and `.` segments,
/// so one file has exactly one unambiguous name.
pub(crate) fn canonical_zip_entry_path<R: Read + ?Sized>(
    entry: &::zip::read::ZipFile<'_, R>,
) -> Option<PathBuf> {
    let raw_name = entry.name().ok()?;
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
    if raw_name != expected_name || enclosed != Path::new(&canonical) {
        return None;
    }
    Some(enclosed)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn decoded_zip_names_keep_unicode_and_reject_path_aliases() {
        for (name, expected) in [
            ("src/a.txt", Some(PathBuf::from("src/a.txt"))),
            ("src/é.txt", Some(PathBuf::from("src/é.txt"))),
            ("src/../a.txt", None),
            ("src//a.txt", None),
            ("src\\a.txt", None),
            ("./a.txt", None),
        ] {
            let mut writer = ::zip::ZipWriter::new(Cursor::new(Vec::new()));
            writer
                .start_file(name, ::zip::write::SimpleFileOptions::default())
                .unwrap();
            let bytes = writer.finish().unwrap().into_inner();
            let mut archive = ::zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
            assert_eq!(
                canonical_zip_entry_path(&archive.by_index(0).unwrap()),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn legacy_cp437_names_remain_supported() {
        let mut writer = ::zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file("a.txt", ::zip::write::SimpleFileOptions::default())
            .unwrap();
        let mut bytes = writer.finish().unwrap().into_inner();
        for (signature, name) in [(b"PK\x03\x04", 30), (b"PK\x01\x02", 46)] {
            let offset = bytes.windows(4).position(|part| part == signature).unwrap();
            bytes[offset + name] = 0x82; // é in the legacy ZIP filename encoding.
        }
        let mut archive = ::zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let entry = archive.by_index(0).unwrap();
        assert_eq!(
            canonical_zip_entry_path(&entry),
            Some(PathBuf::from("é.txt"))
        );
    }
}
