use rsctf::services::ad::patches::PatchManifest;
use rsctf::utils::upload::SOURCE_ARCHIVE_BLOB_BYTES;
use std::fs::File;
use std::io::Read;
use std::path::Path;

fn manifest(path: &Path) -> anyhow::Result<PatchManifest> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    anyhow::ensure!(metadata.is_file(), "expected a regular ZIP file");
    let limit = SOURCE_ARCHIVE_BLOB_BYTES as u64;
    anyhow::ensure!(metadata.len() <= limit, "compressed archive is too large");
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    PatchManifest::from_zip(&bytes).map_err(anyhow::Error::msg)
}

pub(super) fn run(current: &Path, candidate: &Path) -> anyhow::Result<()> {
    use anyhow::Context;

    let current = manifest(current).context("cannot preview current archive")?;
    let candidate = manifest(candidate).context("cannot preview candidate archive")?;
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "currentTree": current.tree_id(),
            "candidateTree": candidate.tree_id(),
            "changes": candidate.changes_from(&current),
        }))?
    );
    Ok(())
}
