use std::path::Path;

use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

use super::{repository, ImportPolicy, ManifestImportResult};
use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

/// Parse and persist one repository-backed challenge manifest.
pub async fn import_manifest(
    st: &SharedState,
    game_id: i32,
    manifest: &Path,
    policy: ImportPolicy,
) -> AppResult<ManifestImportResult> {
    super::import_manifest_inner(st, game_id, manifest, policy, None, None).await
}

/// Import a manifest from an immutable binding snapshot while retaining the
/// binding-relative identity used by persistent checkouts. The snapshot path
/// itself is deliberately never persisted because it is removed after the
/// scan and differs across replicas.
pub async fn import_repository_snapshot_manifest(
    st: &SharedState,
    game_id: i32,
    manifest: &Path,
    policy: ImportPolicy,
    binding_id: i32,
    source_path: &str,
) -> AppResult<ManifestImportResult> {
    let prefix = format!("binding/{binding_id}/");
    let relative_manifest = source_path.strip_prefix(&prefix).unwrap_or_default();
    let relative = Path::new(relative_manifest);
    if binding_id <= 0
        || relative_manifest.is_empty()
        || relative_manifest.len() > 1_024
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(AppError::internal(
            "invalid repository snapshot manifest identity",
        ));
    }
    super::import_manifest_inner(st, game_id, manifest, policy, None, Some(source_path)).await
}

/// Import a job-owned manifest with an identity that survives retries and
/// worker restarts, preventing one source revision from creating duplicates.
pub async fn import_manifest_with_source_identity(
    st: &SharedState,
    game_id: i32,
    manifest: &Path,
    policy: ImportPolicy,
    source_identity: &str,
) -> AppResult<ManifestImportResult> {
    if source_identity.len() > 96 || !source_identity.starts_with("import/") {
        return Err(AppError::internal(
            "invalid challenge import source identity",
        ));
    }
    super::import_manifest_inner(st, game_id, manifest, policy, Some(source_identity), None).await
}

/// Persist a replica-independent source identity only when the manifest resolves
/// inside this game's binding-owned checkout. Job imports use a content identity.
pub(super) fn durable_repo_manifest_path(
    storage_root: &str,
    binding_id: Option<i32>,
    manifest: &Path,
) -> Option<String> {
    let binding_id = binding_id?;
    let checkout = std::fs::canonicalize(
        Path::new(storage_root)
            .join("repos")
            .join(binding_id.to_string()),
    )
    .ok()?;
    let manifest = std::fs::canonicalize(manifest).ok()?;
    (manifest.is_file() && manifest.starts_with(&checkout))
        .then(|| manifest.strip_prefix(&checkout).ok())
        .flatten()
        .filter(|relative| !relative.as_os_str().is_empty())
        .map(|relative| {
            repository::scoped_manifest_identity(
                binding_id,
                &relative.to_string_lossy().replace('\\', "/"),
            )
        })
}

pub(super) async fn associate_import_source_identity(
    transaction: &sea_orm::DatabaseTransaction,
    challenge_id: i32,
    source_identity: &str,
) -> AppResult<()> {
    transaction
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"UPDATE "GameChallenges"
                  SET import_source_identity = $2
                WHERE id = $1"#,
            [challenge_id.into(), source_identity.to_owned().into()],
        ))
        .await?;
    Ok(())
}
