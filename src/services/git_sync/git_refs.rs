//! Restore branch identity when moving between immutable scans and push-on-edit.
use super::*;

pub(super) async fn fetch_and_checkout(
    dest: &Path,
    url: &str,
    transport: &GitTransportPolicy,
    reference: Option<&str>,
    pinned_commit: bool,
) -> AppResult<()> {
    let branch = if pinned_commit || reference.is_some_and(|value| value.starts_with("refs/tags/"))
    {
        None
    } else if let Some(reference) = reference.filter(|value| *value != "HEAD") {
        let name = reference.strip_prefix("refs/heads/").unwrap_or(reference);
        let exact = format!("refs/heads/{name}");
        // Resolve and fetch with the same checkout configuration, including
        // administrator-owned transport policy and exact remote URL rewrites.
        let advertised = run_git_with_transport(
            Some(dest),
            transport,
            &["ls-remote", "--heads", "--", url, &exact],
        )
        .await?;
        advertised
            .lines()
            .any(|line| line.split_whitespace().nth(1) == Some(exact.as_str()))
            .then(|| name.to_owned())
    } else {
        let advertised = run_git_with_transport(
            Some(dest),
            transport,
            &["ls-remote", "--symref", "--", url, "HEAD"],
        )
        .await?;
        advertised.lines().find_map(|line| {
            let (name, target) = line.strip_prefix("ref: refs/heads/")?.split_once('\t')?;
            (target == "HEAD").then(|| name.to_owned())
        })
    };
    // Resolve the default branch before fetching so a remote HEAD change cannot
    // attach files fetched from the old default to the new push destination.
    let branch_ref = branch.as_ref().map(|name| format!("refs/heads/{name}"));
    let refspec = branch_ref.as_deref().unwrap_or(reference.unwrap_or("HEAD"));
    run_git_with_transport(
        Some(dest),
        transport,
        &["fetch", "--depth", "1", "--no-tags", "--", url, refspec],
    )
    .await?;
    if let Some(branch) = branch {
        run_git(dest, &["checkout", "--force", "-B", &branch, "FETCH_HEAD"]).await?;
    } else {
        run_git(dest, &["checkout", "--detach", "--force", "FETCH_HEAD"]).await?;
    }
    Ok(())
}
