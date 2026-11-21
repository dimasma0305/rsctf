use super::*;
use crate::services::ad::patches::revisions::SourceTree;
use bytes::Bytes;
use std::io::Write;
use zip::CompressionMethod::{Deflated, Stored};

fn archive(entries: &[(&str, &[u8], u32, CompressionMethod)]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for &(path, contents, mode, compression) in entries {
        zip.start_file(
            path,
            zip::write::SimpleFileOptions::default()
                .unix_permissions(mode)
                .compression_method(compression),
        )
        .unwrap();
        zip.write_all(contents).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn restore(starter: &[u8], tree: &SourceTree, blobs: Vec<BlobRow>) -> anyhow::Result<Vec<u8>> {
    let sizes = blobs
        .iter()
        .map(|(id, _, _, data)| (id.clone(), data.len() as i32))
        .collect();
    let mut checkout = Checkout::start(
        PatchManifest::from_record(&tree.manifest)?,
        &tree.oid,
        starter,
        &PatchManifest::from_zip(starter).unwrap().tree_id(),
        &sizes,
    )?;
    checkout.append_blobs(blobs)?;
    let mut bytes = Vec::new();
    checkout.finish()?.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn rows(tree: &SourceTree, limit: usize) -> Vec<BlobRow> {
    tree.storage_plan(&BTreeMap::new(), limit)
        .into_iter()
        .map(|blob| {
            (
                blob.oid.clone(),
                blob.deflated,
                blob.raw_size,
                blob.data.to_vec(),
            )
        })
        .collect()
}

#[test]
fn checkout_restores_renames_modes_empty_and_binary_files() {
    let starter = archive(&[
        ("Dockerfile", b"FROM scratch\n", 0o644, Stored),
        ("old", b"hello\n", 0o644, Stored),
        ("removed", b"remove me", 0o644, Deflated),
    ]);
    let target = archive(&[
        ("Dockerfile", b"FROM scratch\n", 0o644, Deflated),
        ("tools/été", b"hello\n", 0o755, Deflated),
        ("copy", b"hello\n", 0o644, Stored),
        ("empty", b"", 0o755, Deflated),
        ("assets/bin", b"\0\xffdata", 0o644, Stored),
    ]);
    let tree = SourceTree::prepare(&starter, Bytes::from(target)).unwrap();
    let blobs = rows(&tree, usize::MAX);
    assert_eq!(blobs.len(), 2);
    let output = restore(&starter, &tree, blobs).unwrap();
    assert_eq!(
        PatchManifest::from_zip(&output).unwrap().tree_id(),
        tree.oid
    );
    let mut zip = ZipArchive::new(Cursor::new(output)).unwrap();
    assert!(zip.by_name("removed").is_err());
    assert!(zip.by_name("old").is_err());
    assert_eq!(
        zip.by_name("tools/été").unwrap().unix_mode().unwrap() & 0o777,
        0o755
    );
    assert_eq!(
        zip.by_name("copy").unwrap().unix_mode().unwrap() & 0o777,
        0o644
    );
}

#[test]
fn checkout_requires_complete_intact_source_and_pinned_starter() {
    let starter = archive(&[("Dockerfile", b"FROM scratch\n", 0o644, Stored)]);
    let target = archive(&[
        ("Dockerfile", b"FROM scratch\n", 0o644, Stored),
        ("asset", b"a private asset", 0o644, Stored),
    ]);
    let tree = SourceTree::prepare(&starter, Bytes::from(target)).unwrap();
    assert!(restore(&starter, &tree, vec![])
        .unwrap_err()
        .to_string()
        .contains("missing"));
    let mut blobs = rows(&tree, usize::MAX);
    blobs[0].3[0] ^= 1;
    assert!(restore(&starter, &tree, blobs)
        .unwrap_err()
        .to_string()
        .contains("integrity"));
    assert!(Checkout::start(
        PatchManifest::from_record(&tree.manifest).unwrap(),
        &tree.oid,
        &starter,
        &tree.oid,
        &BTreeMap::new()
    )
    .is_err());
}

#[test]
fn checkout_preserves_resubmission_budget_when_many_paths_reuse_a_stored_blob() {
    let block: Vec<_> = (0..256u32)
        .flat_map(|i| Sha256::digest(i.to_le_bytes()))
        .collect();
    let payload = block.repeat(3);
    let starter = archive(&[
        ("Dockerfile", b"FROM scratch\n", 0o644, Stored),
        ("base", &payload, 0o644, Stored),
    ]);
    let candidate = archive(&[
        ("Dockerfile", b"FROM scratch\n", 0o644, Stored),
        ("a", &payload, 0o644, Deflated),
        ("b", &payload, 0o755, Deflated),
        ("c", &payload, 0o644, Deflated),
    ]);
    let cap = candidate.len();
    let tree = SourceTree::prepare(&starter, Bytes::from(candidate)).unwrap();
    assert!(rows(&tree, usize::MAX).is_empty());
    let blobs = rows(&tree, cap);
    assert_eq!(blobs.len(), 1);
    assert!(blobs[0].3.len() < payload.len());
    let output = restore(&starter, &tree, blobs).unwrap();
    assert!(output.len() <= cap);
    assert_eq!(
        PatchManifest::from_zip(&output).unwrap().tree_id(),
        tree.oid
    );
}
