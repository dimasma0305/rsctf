//! Validated ZIP-to-tar conversion for Docker build contexts.

use std::io::{Cursor, Read};

const MAX_ENTRIES: usize = 2_048;
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_COMPRESSION_RATIO: u64 = 200;
const MAX_PATH_COMPONENTS: usize = 32;

/// Repack a ZIP archive's selected regular files into a POSIX ustar tarball.
/// Pending submissions retain their complete reviewed archive and record a
/// relative context subtree; that prefix is stripped without replacing the
/// provenance blob.
pub(super) fn zip_bytes_to_tar(
    zip_bytes: &[u8],
    context_subdir: Option<&str>,
) -> Result<Vec<u8>, String> {
    if zip_bytes.len() > super::MAX_BUILD_ARCHIVE_BLOB_BYTES {
        return Err("compressed archive is too large".to_string());
    }
    let context_components = context_subdir
        .filter(|value| *value != ".")
        .map(|value| {
            if value.is_empty() || value.trim() != value || value.contains('\\') {
                return Err("build context subdirectory is invalid".to_string());
            }
            let components: Vec<_> = std::path::Path::new(value)
                .components()
                .map(|component| match component {
                    std::path::Component::Normal(component) => component
                        .to_str()
                        .ok_or_else(|| "build context subdirectory is not UTF-8".to_string()),
                    _ => Err("build context subdirectory is invalid".to_string()),
                })
                .collect::<Result<_, _>>()?;
            if components.is_empty() || components.join("/") != value {
                return Err("build context subdirectory is not canonical".to_string());
            }
            Ok(components)
        })
        .transpose()?;
    let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes))
        .map_err(|error| format!("invalid ZIP: {error}"))?;
    if archive.len() > MAX_ENTRIES {
        return Err("too many entries".to_string());
    }

    let mut out = Vec::new();
    let mut total = 0u64;
    let mut source_names = std::collections::BTreeSet::new();
    let mut output_names = std::collections::BTreeSet::new();
    let mut has_dockerfile = false;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("ZIP read error: {error}"))?;
        let raw_name = entry.name().to_string();
        if raw_name.contains('\\') {
            return Err("entry path contains a backslash".to_string());
        }
        let is_directory = entry.is_dir();
        let path = entry
            .enclosed_name()
            .ok_or_else(|| "entry path escapes the build context".to_string())?;
        let mut components = Vec::new();
        for component in path.components() {
            let std::path::Component::Normal(component) = component else {
                return Err("entry path is not canonical".to_string());
            };
            components.push(
                component
                    .to_str()
                    .ok_or_else(|| "entry path is not valid UTF-8".to_string())?,
            );
        }
        if components.is_empty() || components.len() > MAX_PATH_COMPONENTS {
            return Err("entry path is empty or too deep".to_string());
        }
        let source_name = components.join("/");
        let canonical_name = if is_directory {
            format!("{source_name}/")
        } else {
            source_name.clone()
        };
        if raw_name != canonical_name {
            return Err("entry path is not canonical".to_string());
        }
        if !source_names.insert(source_name) {
            return Err("duplicate entry path".to_string());
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err("archive contains a symbolic link".to_string());
        }
        if is_directory {
            continue;
        }
        if entry.size() > MAX_FILE_BYTES {
            return Err("entry is too large".to_string());
        }
        let compressed = entry.compressed_size().max(1);
        if entry.size() > compressed.saturating_mul(MAX_COMPRESSION_RATIO) {
            return Err("entry compression ratio is too high".to_string());
        }
        let output_components = if let Some(prefix) = context_components.as_ref() {
            if !components.starts_with(prefix) || components.len() <= prefix.len() {
                continue;
            }
            &components[prefix.len()..]
        } else {
            &components[..]
        };
        let name = output_components.join("/");
        if !output_names.insert(name.clone()) {
            return Err("duplicate build-context entry path".to_string());
        }
        has_dockerfile |= name == "Dockerfile";
        let remaining = MAX_TOTAL_BYTES.saturating_sub(total);
        let max_read = remaining.min(MAX_FILE_BYTES);
        let mut data = Vec::new();
        entry
            .by_ref()
            .take(max_read + 1)
            .read_to_end(&mut data)
            .map_err(|error| format!("ZIP read error: {error}"))?;
        if data.len() as u64 > max_read {
            return Err("archive expands beyond the size limit".to_string());
        }
        let actual_size = data.len() as u64;
        if actual_size != entry.size() {
            return Err("entry size does not match ZIP metadata".to_string());
        }
        if actual_size > compressed.saturating_mul(MAX_COMPRESSION_RATIO) {
            return Err("entry compression ratio is too high".to_string());
        }
        total = total.saturating_add(actual_size);
        write_tar_entry(&mut out, &name, &data)?;
    }
    if !has_dockerfile {
        return Err("selected build context is missing Dockerfile".to_string());
    }
    out.extend_from_slice(&[0u8; 1024]);
    Ok(out)
}

fn write_tar_entry(out: &mut Vec<u8>, name: &str, data: &[u8]) -> Result<(), String> {
    let mut header = [0u8; 512];
    let put = |header: &mut [u8; 512], offset: usize, value: &str| {
        let bytes = value.as_bytes();
        header[offset..offset + bytes.len()].copy_from_slice(bytes);
    };

    let (name_field, prefix_field) = split_ustar_path(name)?;
    header[..name_field.len()].copy_from_slice(name_field);
    if let Some(prefix) = prefix_field {
        header[345..345 + prefix.len()].copy_from_slice(prefix);
    }
    put(&mut header, 100, "0000644\0");
    put(&mut header, 108, "0000000\0");
    put(&mut header, 116, "0000000\0");
    put(&mut header, 124, &format!("{:011o}\0", data.len()));
    put(&mut header, 136, "00000000000\0");
    header[156] = b'0';
    put(&mut header, 257, "ustar\0");
    put(&mut header, 263, "00");
    header[148..156].fill(b' ');
    let checksum: u32 = header.iter().map(|&byte| byte as u32).sum();
    put(&mut header, 148, &format!("{checksum:06o}\0 "));

    out.extend_from_slice(&header);
    out.extend_from_slice(data);
    let remainder = data.len() % 512;
    if remainder != 0 {
        out.extend(std::iter::repeat_n(0u8, 512 - remainder));
    }
    Ok(())
}

fn split_ustar_path(name: &str) -> Result<(&[u8], Option<&[u8]>), String> {
    let bytes = name.as_bytes();
    if bytes.is_empty() {
        return Err("empty tar entry path".to_string());
    }
    if bytes.len() <= 100 {
        return Ok((bytes, None));
    }
    let split = bytes
        .iter()
        .enumerate()
        .filter(|(index, byte)| {
            **byte == b'/'
                && *index > 0
                && *index <= 155
                && bytes.len().saturating_sub(*index + 1) <= 100
                && *index + 1 < bytes.len()
        })
        .map(|(index, _)| index)
        .next_back()
        .ok_or_else(|| "entry path cannot be represented in ustar".to_string())?;
    Ok((&bytes[split + 1..], Some(&bytes[..split])))
}
