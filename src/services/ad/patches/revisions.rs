use super::{git_object, FileIdentity, PatchManifest};
use bytes::Bytes;
use std::collections::BTreeSet;
use std::io::Cursor;
use uuid::Uuid;

pub(crate) struct SourceBlob {
    pub oid: String,
    pub deflated: bool,
    pub raw_size: i32,
    pub data: Bytes,
}

pub(crate) struct SourceTree {
    pub oid: String,
    /// Sorted (path, Git blob ID, permissions) tuples. Directory trees are
    /// implicit, exactly as in PatchManifest::tree_id.
    pub manifest: String,
    pub blobs: Vec<SourceBlob>,
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
    pub fn prepare(original: &[u8], candidate: Bytes) -> anyhow::Result<Self> {
        let starter = PatchManifest::from_zip(original).map_err(anyhow::Error::msg)?;
        let manifest = PatchManifest::from_zip(&candidate).map_err(anyhow::Error::msg)?;
        let mut known: BTreeSet<_> = starter.0.values().map(|file| file.digest).collect();
        let mut entries = Vec::with_capacity(manifest.0.len());
        let mut blobs = Vec::new();
        let mut packed_bytes = 0usize;
        let mut zip = zip::ZipArchive::new(Cursor::new(&candidate[..]))?;
        for (path, file) in &manifest.0 {
            let oid = hex::encode(file.digest);
            entries.push((path, oid.clone(), file.mode));
            if !known.insert(file.digest) {
                continue;
            }
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
            blobs.push(SourceBlob {
                oid,
                deflated,
                raw_size: i32::try_from(member.size())?,
                data: candidate.slice(start..end),
            });
        }
        Ok(Self {
            oid: manifest.tree_id(),
            manifest: serde_json::to_string(&entries)?,
            blobs,
        })
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
