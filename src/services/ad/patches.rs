//! Compare source inputs for team patch revisions without extracting or executing them.

use crate::services::challenge_builds::visit_build_files;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) mod checkout;
pub(crate) mod public_diff;
pub(crate) mod restore;
pub(crate) mod revisions;
pub(crate) mod ssh_observer;
pub(crate) mod ssh_sources;
pub(crate) mod starter;

/// Used by the trusted queue consumer. A lost or superseded claim cannot read the source.
pub async fn claimed_source(
    pool: &sqlx::PgPool,
    attempt: uuid::Uuid,
    claim: uuid::Uuid,
) -> anyhow::Result<std::fs::File> {
    use anyhow::Context;
    let (service,tree_id,record,original,original_id)=sqlx::query_as::<_,(i32,String,String,Vec<u8>,String)>(r#"
      SELECT b.service_id,t.tree_id,t.manifest,st.archive,st.tree_id
        FROM "AdPatchBuilds" b JOIN "AdPatchRevisions" r USING(service_id,revision)
        JOIN "AdPatchSourceTrees" t USING(service_id,tree_id)
        JOIN "AdPatchBuildEligibleServices" s ON s.service_id=b.service_id
        JOIN "AdPatchOnlineBuilders" w ON w.challenge_id=s.challenge_id AND w.backend_pid=b.claimed_by AND w.profile=b.profile
        JOIN "AdPatchStarters" st ON st.challenge_id=s.challenge_id
        WHERE b.attempt=$1 AND b.claim=$2 AND b.status='Building'
        AND octet_length(t.manifest)<=4194304"#)
       .bind(attempt).bind(claim).fetch_optional(pool).await?.context("patch build claim is no longer available")?;
    let manifest = PatchManifest::from_record(&record)?;
    let sizes = restore::blob_sizes(pool, service, manifest.blob_ids()).await?;
    restore::restore(
        pool,
        service,
        restore::Source {
            manifest,
            tree_id,
            original,
            original_id,
        },
        sizes,
        std::sync::Arc::new(()),
    )
    .await
}

#[derive(Debug, PartialEq, Eq)]
struct FileIdentity {
    mode: u32,
    digest: [u8; 32],
}

pub struct PatchManifest(BTreeMap<String, FileIdentity>);

/// Check the entire immutable input before sending any source to the builder.
/// A second streaming pass emits ustar without retaining the expanded context.
pub fn write_verified_context(
    zip: &[u8],
    expected_tree: &str,
    out: &mut dyn std::io::Write,
) -> Result<(), String> {
    if PatchManifest::from_zip(zip)?.tree_id() != expected_tree {
        return Err("patch ZIP does not match the expected Git tree".into());
    }
    crate::services::challenge_builds::write_build_context(zip, None, out)
}

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

#[derive(Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
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
