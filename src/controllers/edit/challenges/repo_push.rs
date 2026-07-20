//! Ordered, best-effort challenge push-back for repository-bound games.

use super::*;

use crate::models::data::repo_binding;
use crate::services::git_sync;

struct PushPayload {
    binding_id: i32,
    repo_url: String,
    token: String,
    challenge_id: i32,
    title: String,
    manifest: std::path::PathBuf,
    relative_manifest: String,
    yaml: String,
}

enum SnapshotResult {
    Ready(PushPayload),
    Retry,
    Skip,
}

/// Enqueue identifiers only. Every queued edit re-reads the current durable
/// state after acquiring the checkout lock, so delayed tasks cannot push an old
/// in-memory challenge snapshot after a newer save.
pub(super) fn spawn(st: SharedState, game_id: i32, challenge_id: i32) {
    tokio::spawn(async move {
        if let Err(error) = push_latest(&st, game_id, challenge_id).await {
            tracing::warn!(
                game = game_id,
                challenge = challenge_id,
                %error,
                "push-back: failed (best-effort; edit already committed)"
            );
        }
    });
}

async fn current_binding_id(st: &SharedState, game_id: i32) -> AppResult<Option<i32>> {
    sqlx::query_scalar(r#"SELECT repo_binding_id FROM "Games" WHERE id = $1"#)
        .bind(game_id)
        .fetch_optional(st.pg())
        .await
        .map(|row: Option<Option<i32>>| row.flatten())
        .map_err(|error| AppError::internal(error.to_string()))
}

async fn push_latest(st: &SharedState, game_id: i32, challenge_id: i32) -> AppResult<()> {
    // A concurrent rebind can change the checkout root between the cheap lookup
    // and the fenced snapshot. Retry a bounded number of times on that rare path.
    for _ in 0..3 {
        let Some(binding_id) = current_binding_id(st, game_id).await? else {
            return Ok(());
        };
        let dest = std::path::PathBuf::from(&st.config.storage_root)
            .join("repos")
            .join(binding_id.to_string());
        let _checkout = git_sync::lock_checkout_distributed(st.pg(), &dest).await?;
        let Some(initial_binding) = repo_binding::Entity::find_by_id(binding_id)
            .one(&st.db)
            .await?
        else {
            return Ok(());
        };
        if !initial_binding.push_on_edit {
            return Ok(());
        }
        let Some(token) = initial_binding
            .github_token
            .as_deref()
            .filter(|token| !token.is_empty())
            .map(str::to_string)
        else {
            tracing::info!(binding = binding_id, "push-back: no token; skipping");
            return Ok(());
        };
        let repo_url = git_sync::validate_binding_repo_url(&initial_binding.repo_url)?;
        let git_ref = git_sync::validate_git_ref(initial_binding.git_ref.as_deref())?;
        let auth_url = git_sync::GitCredentials::new(token).apply(&repo_url);
        git_sync::sync_repo(&auth_url, git_ref.as_deref(), &dest).await?;

        match snapshot_after_checkout(
            st,
            game_id,
            challenge_id,
            binding_id,
            &initial_binding,
            &dest,
        )
        .await?
        {
            SnapshotResult::Retry => continue,
            SnapshotResult::Skip => return Ok(()),
            SnapshotResult::Ready(payload) => {
                tokio::fs::write(&payload.manifest, &payload.yaml)
                    .await
                    .map_err(|error| {
                        AppError::internal(format!(
                            "push-back: write {}: {error}",
                            payload.manifest.display()
                        ))
                    })?;
                let message = format!("chore: update {} from rsctf admin edit", payload.title);
                git_sync::push_file(
                    &dest,
                    &payload.relative_manifest,
                    &payload.repo_url,
                    &payload.token,
                    &message,
                )
                .await?;
                tracing::info!(
                    binding = payload.binding_id,
                    challenge = payload.challenge_id,
                    yaml = %payload.relative_manifest,
                    "push-back: pushed latest database state"
                );
                return Ok(());
            }
        }
    }
    Err(AppError::conflict(
        "repository binding changed repeatedly while push-back was queued",
    ))
}

/// The checkout is already serialized and current. Take the same short
/// game -> definition order as repository import and interactive edits, then
/// read the binding, challenge and static flags as one authoritative snapshot.
async fn snapshot_after_checkout(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
    expected_binding_id: i32,
    synced_binding: &repo_binding::Model,
    checkout: &std::path::Path,
) -> AppResult<SnapshotResult> {
    let game_lock = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    let mut definition_lock = crate::services::challenge_workloads::acquire_definition_lock(
        st.pg(),
        game_id,
        challenge_id,
    )
    .await?;
    let snapshot: AppResult<SnapshotResult> = async {
        match super::reject_pending_mutation(
            &mut **definition_lock.transaction_mut(),
            game_id,
            challenge_id,
        )
        .await
        {
            Ok(()) => {}
            Err(AppError::Conflict(_)) | Err(AppError::NotFound(_)) => {
                return Ok(SnapshotResult::Skip);
            }
            Err(error) => return Err(error),
        }
        let Some(current_game) = game::Entity::find_by_id(game_id).one(&st.db).await? else {
            return Ok(SnapshotResult::Skip);
        };
        if current_game.repo_binding_id != Some(expected_binding_id) {
            return Ok(SnapshotResult::Retry);
        }
        let Some(binding) = repo_binding::Entity::find_by_id(expected_binding_id)
            .one(&st.db)
            .await?
        else {
            return Ok(SnapshotResult::Skip);
        };
        if !binding.push_on_edit {
            return Ok(SnapshotResult::Skip);
        }
        if binding.repo_url != synced_binding.repo_url
            || binding.git_ref != synced_binding.git_ref
            || binding.github_token != synced_binding.github_token
        {
            return Ok(SnapshotResult::Retry);
        }
        let Some(challenge) = game_challenge::Entity::find_by_id(challenge_id)
            .one(&st.db)
            .await?
            .filter(|challenge| challenge.game_id == game_id)
        else {
            return Ok(SnapshotResult::Skip);
        };
        let Some(manifest) = locate_owned_manifest(checkout, expected_binding_id, &challenge).await
        else {
            tracing::warn!(
                binding = expected_binding_id,
                challenge = challenge_id,
                "push-back: repository ownership path is missing or invalid; skipping"
            );
            return Ok(SnapshotResult::Skip);
        };
        let flag_texts = if challenge.challenge_type == ChallengeType::DynamicContainer {
            Vec::new()
        } else {
            flag_context::Entity::find()
                .filter(flag_context::Column::ChallengeId.eq(challenge.id))
                .all(&st.db)
                .await?
                .into_iter()
                .filter_map(|flag| {
                    let flag = flag.flag.trim().to_string();
                    (!flag.is_empty()).then_some(flag)
                })
                .collect()
        };
        let relative_manifest = manifest
            .strip_prefix(checkout)
            .map_err(|_| AppError::internal("push-back manifest escaped checkout"))?
            .to_string_lossy()
            .replace('\\', "/");
        let token = binding
            .github_token
            .clone()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| AppError::internal("push-back token disappeared"))?;
        let source_yaml = tokio::fs::read_to_string(&manifest)
            .await
            .map_err(|error| {
                AppError::internal(format!(
                    "push-back: read current manifest {}: {error}",
                    manifest.display()
                ))
            })?;
        let yaml =
            git_sync::serialize_challenge_preserving_source(&challenge, &flag_texts, &source_yaml)?;
        Ok(SnapshotResult::Ready(PushPayload {
            binding_id: binding.id,
            repo_url: git_sync::validate_binding_repo_url(&binding.repo_url)?,
            token,
            challenge_id: challenge.id,
            title: challenge.title.clone(),
            manifest,
            relative_manifest,
            yaml,
        }))
    }
    .await;
    definition_lock.release().await?;
    game_lock
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    snapshot
}

/// Push-back never adopts a same-title manifest. Only a binding-scoped durable
/// repository identity (including its exact safe legacy form) proves ownership.
async fn locate_owned_manifest(
    checkout: &std::path::Path,
    binding_id: i32,
    challenge: &game_challenge::Model,
) -> Option<std::path::PathBuf> {
    let source = challenge
        .source_yaml_path
        .as_deref()
        .filter(|source| !source.is_empty())?;
    let candidate = git_sync::manifest_candidate_in_checkout(checkout, Some(binding_id), source)?;
    let checkout = tokio::fs::canonicalize(checkout).await.ok()?;
    let manifest = tokio::fs::canonicalize(candidate).await.ok()?;
    (manifest.is_file() && manifest.starts_with(checkout)).then_some(manifest)
}

/// Exercise the production checkout + database snapshot ordering while using a
/// local test remote. Network authentication is deliberately bypassed only in
/// this test seam; ownership, locking, serialization, commit order and HEAD are
/// the same operations whose ordering guards production push-back.
#[cfg(test)]
pub(crate) async fn commit_latest_to_checkout_for_test(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
    started: Option<tokio::sync::oneshot::Sender<()>>,
) -> AppResult<()> {
    let binding_id = current_binding_id(st, game_id)
        .await?
        .ok_or_else(|| AppError::internal("test game is not repository-bound"))?;
    let checkout = std::path::PathBuf::from(&st.config.storage_root)
        .join("repos")
        .join(binding_id.to_string());
    if let Some(started) = started {
        let _ = started.send(());
    }
    let _checkout_lock = git_sync::lock_checkout_distributed(st.pg(), &checkout).await?;
    let binding = repo_binding::Entity::find_by_id(binding_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::internal("test repository binding disappeared"))?;
    let payload =
        match snapshot_after_checkout(st, game_id, challenge_id, binding_id, &binding, &checkout)
            .await?
        {
            SnapshotResult::Ready(payload) => payload,
            SnapshotResult::Retry => {
                return Err(AppError::conflict("test push-back snapshot moved"))
            }
            SnapshotResult::Skip => {
                return Err(AppError::internal("test push-back snapshot was skipped"));
            }
        };
    tokio::fs::write(&payload.manifest, &payload.yaml)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    run_test_git(&checkout, &["add", "--", &payload.relative_manifest]).await?;
    let staged = tokio::process::Command::new("git")
        .current_dir(&checkout)
        .args(["diff", "--cached", "--quiet"])
        .status()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if !staged.success() {
        run_test_git(&checkout, &["commit", "-m", "test: ordered push-back"]).await?;
    }
    run_test_git(&checkout, &["push", "origin", "HEAD:refs/heads/main"]).await
}

#[cfg(test)]
async fn run_test_git(checkout: &std::path::Path, args: &[&str]) -> AppResult<()> {
    let output = tokio::process::Command::new("git")
        .current_dir(checkout)
        .args(args)
        .output()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(AppError::internal(format!(
            "test git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}
