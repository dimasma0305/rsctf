//! Reconstruct a team's source from its registered starter and immutable Git blobs.
use super::PatchManifest;
use crate::services::challenge_builds::{MAX_ENTRIES, MAX_FILE_BYTES, MAX_TOTAL_BYTES};
use anyhow::{ensure, Context};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Cursor, Read, Seek};
use std::os::unix::fs::OpenOptionsExt;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

pub(crate) struct Checkout {
    zip: ZipWriter<File>,
    remaining: BTreeMap<String, Vec<(String, u32)>>,
    expanded: u64,
}

pub(crate) type BlobRow = (String, bool, i32, Vec<u8>);

/// Unlink the export before writing private source. The last descriptor
/// cleans up after cancellation, errors, and process termination.
pub(crate) fn private_export_file() -> std::io::Result<File> {
    let path = std::env::temp_dir().join(format!("rsctf-patch-{}", uuid::Uuid::new_v4()));
    let file = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    std::fs::remove_file(path)?;
    Ok(file)
}

impl Checkout {
    pub fn start(
        target: PatchManifest,
        tree_id: &str,
        starter: &[u8],
        starter_id: &str,
        private_sizes: &BTreeMap<String, i32>,
    ) -> anyhow::Result<Self> {
        ensure!(
            target.0.len() <= MAX_ENTRIES
                && target.0.contains_key("Dockerfile")
                && target.tree_id() == tree_id,
            "invalid saved patch tree"
        );
        let mut remaining = BTreeMap::<String, Vec<_>>::new();
        for (path, entry) in target.0 {
            ensure!(
                matches!(entry.mode, 0o644 | 0o755)
                    && path.len() <= 256
                    && !path.contains(['\\', '\0'])
                    && path.split('/').all(|p| !p.is_empty()
                        && p != "."
                        && p != ".."
                        && !p.eq_ignore_ascii_case(".git")),
                "invalid saved patch entry"
            );
            remaining
                .entry(hex::encode(entry.digest))
                .or_default()
                .push((path, entry.mode));
        }
        let mut out = Self {
            zip: ZipWriter::new(private_export_file()?),
            remaining,
            expanded: 0,
        };
        let base = PatchManifest::from_zip(starter).map_err(anyhow::Error::msg)?;
        ensure!(base.tree_id() == starter_id, "starter tree changed");
        let mut archive = ZipArchive::new(Cursor::new(starter))?;
        for (oid, (name, size)) in super::revisions::starter_sources(base, &mut archive)? {
            if !out.remaining.contains_key(&oid)
                || private_sizes
                    .get(&oid)
                    .is_some_and(|n| (*n as usize) < size)
            {
                continue;
            }
            let member = archive.by_name(&name)?;
            let start = usize::try_from(
                member
                    .data_start()
                    .context("missing starter member offset")?,
            )?;
            let end = start
                .checked_add(usize::try_from(member.compressed_size())?)
                .context("starter offset overflow")?;
            let payload = starter
                .get(start..end)
                .context("invalid starter member extent")?;
            out.append(
                &oid,
                member.compression() == CompressionMethod::Deflated,
                member.size(),
                payload,
                member.crc32(),
            )?;
        }
        Ok(out)
    }

    pub fn needs(&self, oid: &str) -> bool {
        self.remaining.contains_key(oid)
    }

    pub fn append_blobs(&mut self, rows: Vec<BlobRow>) -> anyhow::Result<()> {
        for (oid, deflated, raw_size, payload) in rows {
            let size = u64::try_from(raw_size)?;
            ensure!(size <= MAX_FILE_BYTES, "oversized patch file");
            let mut digest = Sha256::new();
            digest.update(format!("blob {size}\0"));
            let mut crc = flate2::Crc::new();
            let mut reader = blob_reader(deflated, &payload).take(size + 1);
            let mut buffer = [0; 32 * 1024];
            let mut count = 0u64;
            loop {
                let n = reader.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                digest.update(&buffer[..n]);
                crc.update(&buffer[..n]);
                count += n as u64;
            }
            ensure!(
                size == count && hex::encode(digest.finalize()) == oid,
                "patch blob integrity check failed"
            );
            self.append(&oid, deflated, size, &payload, crc.sum())?;
        }
        Ok(())
    }

    fn append(
        &mut self,
        oid: &str,
        deflated: bool,
        size: u64,
        payload: &[u8],
        crc: u32,
    ) -> anyhow::Result<()> {
        let paths = self
            .remaining
            .remove(oid)
            .context("unexpected patch blob")?;
        self.expanded += size * paths.len() as u64;
        ensure!(
            self.expanded <= MAX_TOTAL_BYTES,
            "patch checkout exceeds expansion budget"
        );
        for (path, mode) in paths {
            // Never write more packed data than raw source to the spool. This
            // also handles valid but inefficient deflate representations.
            if deflated && payload.len() as u64 >= size {
                self.zip.start_file(
                    path,
                    zip::write::SimpleFileOptions::default()
                        .compression_method(CompressionMethod::Stored)
                        .unix_permissions(mode),
                )?;
                let written = std::io::copy(
                    &mut blob_reader(true, payload).take(size + 1),
                    &mut self.zip,
                )?;
                ensure!(written == size, "invalid patch file length");
            } else {
                copy_packed(&mut self.zip, &path, mode, deflated, size, payload, crc)?;
            }
        }
        Ok(())
    }

    pub fn finish(self) -> anyhow::Result<File> {
        ensure!(self.remaining.is_empty(), "missing private patch blob");
        let mut file = self.zip.finish()?;
        file.rewind()?;
        Ok(file)
    }
}

impl PatchManifest {
    pub(crate) fn blob_ids(&self) -> Vec<String> {
        self.0
            .values()
            .map(|entry| entry.digest)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(hex::encode)
            .collect()
    }
}

fn blob_reader(deflated: bool, payload: &[u8]) -> Box<dyn Read + '_> {
    if deflated {
        Box::new(flate2::read::DeflateDecoder::new(payload))
    } else {
        Box::new(payload)
    }
}

/// Adapt an already verified payload to the zip crate's raw-copy API. Only
/// the local header is synthesized (PKWARE APPNOTE 4.3.7); ZipWriter owns
/// the output directory, file modes, offsets, UTF-8 names, and finalization.
fn copy_packed(
    zip: &mut ZipWriter<File>,
    path: &str,
    mode: u32,
    deflated: bool,
    size: u64,
    payload: &[u8],
    crc: u32,
) -> anyhow::Result<()> {
    let mut header = [0u8; 30];
    header[..4].copy_from_slice(b"PK\x03\x04");
    // The streaming reader derives the origin system from the high byte.
    // Set Unix so the output central directory honors executable permissions.
    header[4] = 20;
    header[5] = 3;
    header[6..8].copy_from_slice(&0x0800u16.to_le_bytes());
    header[8] = if deflated { 8 } else { 0 };
    header[12] = 0x21; // January 1, 1980
    header[14..18].copy_from_slice(&crc.to_le_bytes());
    header[18..22].copy_from_slice(&u32::try_from(payload.len())?.to_le_bytes());
    header[22..26].copy_from_slice(&u32::try_from(size)?.to_le_bytes());
    header[26..28].copy_from_slice(&u16::try_from(path.len())?.to_le_bytes());
    let mut member = Read::chain(&header[..], path.as_bytes()).chain(payload);
    let source = zip::read::read_zipfile_from_stream(&mut member)?.context("missing ZIP member")?;
    zip.raw_copy_file_touch(source, zip::DateTime::default(), Some(mode))?;
    Ok(())
}

#[cfg(test)]
mod tests;
