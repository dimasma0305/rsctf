use rsctf::services::ad::patches::PatchManifest;
use rsctf::utils::upload::SOURCE_ARCHIVE_BLOB_BYTES;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

pub(super) fn job_source(attempt: uuid::Uuid, claim: uuid::Uuid) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut file = runtime.block_on(async {
        let url = std::env::var("RSCTF_DATABASE_URL")?;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await?;
        rsctf::services::ad::patches::claimed_source(&pool, attempt, claim).await
    })?;
    std::io::copy(&mut file, &mut std::io::stdout().lock())?;
    Ok(())
}

fn archive(path: &Path) -> anyhow::Result<Vec<u8>> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    anyhow::ensure!(metadata.is_file(), "expected a regular ZIP file");
    let limit = SOURCE_ARCHIVE_BLOB_BYTES as u64;
    anyhow::ensure!(metadata.len() <= limit, "compressed archive is too large");
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= limit,
        "compressed archive is too large"
    );
    Ok(bytes)
}

fn manifest(path: &Path) -> anyhow::Result<PatchManifest> {
    PatchManifest::from_zip(&archive(path)?).map_err(anyhow::Error::msg)
}

pub(super) fn context(path: &Path, tree: &str) -> anyhow::Result<()> {
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    rsctf::services::ad::patches::write_verified_context(&archive(path)?, tree, &mut out)
        .map_err(anyhow::Error::msg)?;
    out.flush()?;
    Ok(())
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
