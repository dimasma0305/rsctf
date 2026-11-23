use super::{git_object, FileIdentity, PatchManifest};
use bytes::Bytes;
use std::collections::BTreeMap;
use std::io::Cursor;
use uuid::Uuid;

pub(crate) struct SourceBlob {
    pub oid: String,
    pub deflated: bool,
    pub raw_size: i32,
    pub data: Bytes,
    uses: usize,
    starter_bytes: Option<usize>,
}

pub(crate) struct SourceTree {
    pub oid: String,
    pub shared_changes: Vec<super::public_diff::PublishedChange>,
    /// Sorted (path, Git blob ID, permissions) tuples. Directory trees are
    /// implicit, exactly as in PatchManifest::tree_id.
    pub manifest: String,
    pub blobs: Vec<SourceBlob>,
    zip_header_bytes: usize,
}

/// A digest can occur under multiple starter paths with different encodings.
/// Use its smallest available representation for all restored paths.
pub(super) fn starter_sources(
    manifest: PatchManifest,
    zip: &mut zip::ZipArchive<Cursor<&[u8]>>,
) -> anyhow::Result<BTreeMap<String, (String, usize)>> {
    let mut sources = BTreeMap::<String, (String, usize)>::new();
    for (path, file) in manifest.0 {
        let member = zip.by_name(&path)?;
        let size = usize::try_from(member.compressed_size().min(member.size()))?;
        let entry = sources
            .entry(hex::encode(file.digest))
            .or_insert_with(|| (path.clone(), size));
        if size < entry.1 {
            *entry = (path, size);
        }
    }
    Ok(sources)
}

impl PatchManifest {
    pub(crate) fn from_record(record: &str) -> anyhow::Result<Self> {
        let entries: Vec<(String, String, u32)> = serde_json::from_str(record)?;
        let mut files = std::collections::BTreeMap::new();
        for (path, oid, mode) in entries {
            let mut digest = [0; 32];
            hex::decode_to_slice(oid, &mut digest)?;
            files.insert(path, FileIdentity { mode, digest });
        }
        Ok(Self(files))
    }
}

impl SourceTree {
    pub fn prepare(
        original: &[u8],
        candidate: Bytes,
        sources: &[super::ssh_sources::SshSource],
    ) -> anyhow::Result<Self> {
        let starter = PatchManifest::from_zip(original).map_err(anyhow::Error::msg)?;
        let manifest = PatchManifest::from_zip(&candidate).map_err(anyhow::Error::msg)?;
        // Only the trusted starter may select public paths. Team archives can
        // add arbitrary filenames and change their own registration file.
        let mut original_zip = zip::ZipArchive::new(Cursor::new(original))?;
        let mut zip = zip::ZipArchive::new(Cursor::new(&candidate[..]))?;
        let shared_changes =
            super::public_diff::changes(sources, &starter, &manifest, &mut original_zip, &mut zip)?;
        let sources = starter_sources(starter, &mut original_zip)?;
        let mut entries = Vec::with_capacity(manifest.0.len());
        let mut blobs = BTreeMap::<String, SourceBlob>::new();
        let mut zip_header_bytes = 22; // EOCD, plus the local and central headers per file.
        let mut packed_bytes = 0usize;
        for (path, file) in &manifest.0 {
            let oid = hex::encode(file.digest);
            entries.push((path, oid.clone(), file.mode));
            zip_header_bytes += 76 + 2 * path.len();
            // The manifest above validated the uncompressed bytes, CRC and both
            // ZIP headers. Retain only that member's compressed payload, without
            // unpacking it again or keeping the uploaded ZIP in the database.
            let member = zip.by_name(path)?;
            let start = usize::try_from(
                member
                    .data_start()
                    .ok_or_else(|| anyhow::anyhow!("missing ZIP member offset"))?,
            )?;
            let end = start
                .checked_add(usize::try_from(member.compressed_size())?)
                .filter(|end| *end <= candidate.len())
                .ok_or_else(|| anyhow::anyhow!("invalid ZIP member extent"))?;
            packed_bytes = packed_bytes
                .checked_add(end - start)
                .filter(|total| *total <= candidate.len())
                .ok_or_else(|| anyhow::anyhow!("ZIP member payloads exceed the archive size"))?;
            let deflated = match member.compression() {
                zip::CompressionMethod::Stored => false,
                zip::CompressionMethod::Deflated => true,
                _ => anyhow::bail!("unsupported ZIP compression"),
            };
            let raw_size = i32::try_from(member.size())?;
            let blob = blobs.entry(oid.clone()).or_insert_with(|| SourceBlob {
                starter_bytes: sources.get(&oid).map(|(_, size)| *size),
                oid,
                deflated,
                raw_size,
                data: candidate.slice(start..end),
                uses: 0,
            });
            blob.uses += 1;
            if end - start < blob.data.len() {
                blob.data = candidate.slice(start..end);
                blob.deflated = deflated;
            }
        }
        Ok(Self {
            oid: manifest.tree_id(),
            shared_changes,
            manifest: serde_json::to_string(&entries)?,
            blobs: blobs.into_values().collect(),
            zip_header_bytes,
        })
    }

    /// Usually only genuinely new file contents need storage. When reuse of
    /// an older, larger encoding would make a valid ZIP too big to resubmit,
    /// retain the smaller encodings supplied by this team as well.
    pub fn storage_plan(
        &self,
        saved: &BTreeMap<String, i32>,
        max_zip_bytes: usize,
    ) -> Vec<&SourceBlob> {
        let prior_size = |blob: &SourceBlob| {
            saved
                .get(&blob.oid)
                .map(|n| (*n as usize).min(blob.raw_size as usize))
                .into_iter()
                .chain(blob.starter_bytes)
                .min()
        };
        let compact_size = |blob: &SourceBlob| blob.data.len().min(blob.raw_size as usize);
        let projected = self.zip_header_bytes
            + self
                .blobs
                .iter()
                .map(|b| b.uses * prior_size(b).unwrap_or_else(|| compact_size(b)))
                .sum::<usize>();
        self.blobs
            .iter()
            .filter(|b| match prior_size(b) {
                None => true,
                Some(old) => projected > max_zip_bytes && compact_size(b) < old,
            })
            .collect()
    }
}

/// Canonical Git SHA-256 commit, using a stable platform identity and a
/// UUID for the actor. The starter is shared; each team's first save parents it.
pub(crate) fn commit_id(
    tree: &str,
    parent: Option<&str>,
    actor: Option<Uuid>,
    seconds: i64,
) -> String {
    let parent_line = parent
        .map(|id| format!("parent {id}\n"))
        .unwrap_or_default();
    let identity = match actor {
        Some(id) => format!("RSCTF player <{id}@rsctf.invalid>"),
        None => "RSCTF <starter@rsctf.invalid>".into(),
    };
    let message = if parent.is_some() {
        "Save patch source"
    } else {
        "Original starter"
    };
    let body = format!("tree {tree}\n{parent_line}author {identity} {seconds} +0000\ncommitter {identity} {seconds} +0000\n\n{message}\n");
    hex::encode(git_object("commit", body.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_and_child_match_git_hash_object_in_a_sha256_repository() {
        let tree = "6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321";
        let root = commit_id(tree, None, None, 0);
        assert_eq!(
            root,
            "592c93043efe9df329415af61e030807f66b772382762918cadf3eb527b13f19"
        );
        let actor = Uuid::parse_str("12345678-1234-1234-1234-123456789abc").unwrap();
        assert_eq!(
            commit_id(tree, Some(&root), Some(actor), 1_700_000_000),
            "78a848a9cb3cb0fd71d1d7a4136566f62318172188cb29696ace9ffb984f0466"
        );
    }
}
