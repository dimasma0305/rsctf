//! Compare source inputs for team patch revisions without extracting or executing them.

use crate::services::challenge_builds::visit_build_files;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) mod revisions;

#[derive(Debug, PartialEq, Eq)]
struct FileIdentity {
    mode: u32,
    digest: [u8; 32],
}

pub struct PatchManifest(BTreeMap<String, FileIdentity>);

#[derive(Debug, Eq, PartialEq, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Starter {
    pub tree_id: String,
    pub archive_bytes: i32,
}

// Git SHA-256 repositories hash the object type and byte count before the payload.
fn git_object(kind: &str, bytes: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(format!("{kind} {}\0", bytes.len()));
    digest.update(bytes);
    digest.finalize().into()
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct PatchChange {
    pub path: String,
    pub kind: ChangeKind,
}

impl PatchManifest {
    pub fn from_zip(zip: &[u8]) -> Result<Self, String> {
        let mut files = BTreeMap::new();
        let mut buffer = [0u8; 32 * 1024];
        visit_build_files(zip, None, |name, mode, size, bytes| {
            if name
                .split('/')
                .any(|part| part.eq_ignore_ascii_case(".git"))
            {
                return Err("a patch archive cannot contain Git repository metadata".into());
            }
            let mut hash = Sha256::new();
            hash.update(format!("blob {size}\0"));
            loop {
                let count = bytes
                    .read(&mut buffer)
                    .map_err(|e| format!("ZIP read error: {e}"))?;
                if count == 0 {
                    break;
                }
                hash.update(&buffer[..count]);
            }
            files.insert(
                name.to_owned(),
                FileIdentity {
                    mode,
                    digest: hash.finalize().into(),
                },
            );
            Ok(())
        })?;
        Ok(Self(files))
    }

    /// Equivalent to `git write-tree` in a SHA-256 repository. Empty directories
    /// do not appear in either the validated build context or the Git tree.
    pub fn tree_id(&self) -> String {
        let mut root = Directory::default();
        for (path, file) in &self.0 {
            let mut dir = &mut root;
            let mut parts = path.split('/').peekable();
            while let Some(part) = parts.next() {
                if parts.peek().is_none() {
                    dir.files.push((part, file));
                    break;
                }
                dir = dir.children.entry(part).or_default();
            }
        }
        hex::encode(root.digest())
    }

    pub fn changes_from(&self, current: &Self) -> Vec<PatchChange> {
        current
            .0
            .keys()
            .chain(self.0.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter_map(|path| {
                let kind = match (current.0.get(path), self.0.get(path)) {
                    (Some(before), Some(after)) if before != after => ChangeKind::Modified,
                    (None, Some(_)) => ChangeKind::Added,
                    (Some(_), None) => ChangeKind::Deleted,
                    _ => return None,
                };
                Some(PatchChange {
                    path: path.to_owned(),
                    kind,
                })
            })
            .collect()
    }
}

#[derive(Default)]
struct Directory<'a> {
    files: Vec<(&'a str, &'a FileIdentity)>,
    children: BTreeMap<&'a str, Directory<'a>>,
}

impl Directory<'_> {
    fn digest(&self) -> [u8; 32] {
        // Git compares directory names as though they end in '/', but stores
        // the bare name in the object. This matters for e.g. "app.js" and "app/".
        let mut entries = BTreeMap::new();
        for &(name, file) in &self.files {
            let mode = if file.mode == 0o755 {
                "100755"
            } else {
                "100644"
            };
            entries.insert(name.to_owned(), (mode, name, file.digest));
        }
        for (&name, dir) in &self.children {
            entries.insert(format!("{name}/"), ("40000", name, dir.digest()));
        }
        let mut bytes = Vec::new();
        for (mode, name, digest) in entries.into_values() {
            bytes.extend_from_slice(mode.as_bytes());
            bytes.push(b' ');
            bytes.extend_from_slice(name.as_bytes());
            bytes.push(0);
            bytes.extend_from_slice(&digest);
        }
        git_object("tree", &bytes)
    }
}
