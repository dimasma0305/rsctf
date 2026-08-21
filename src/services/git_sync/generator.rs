//! Resolve repository-authored generator source into an internal build intent.
//!
//! `generator/Dockerfile` is trusted executable source only for manager-owned
//! imports. The repository manifest declares the policy; rsctf owns the
//! mutable build tag and persists only the resulting immutable local image id.

use std::path::{Path, PathBuf};

use crate::app_state::SharedState;
use crate::models::data::game_challenge;
use crate::utils::enums::{ChallengeBuildStatus, ChallengeVariantMode};
use crate::utils::error::{AppError, AppResult};

use super::package::{archived_context_fingerprint, context_fingerprint};
use super::{ChallengeYaml, ImportPolicy};

pub(crate) const GENERATOR_CONTEXT_SUBDIR: &str = "generator";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GeneratorImportIntent {
    pub(super) image: Option<String>,
    pub(super) digest: Option<String>,
    pub(super) build_context_subdir: Option<String>,
    pub(super) build_status: ChallengeBuildStatus,
    pub(super) last_build_log: Option<String>,
    pub(super) automatic: bool,
    pub(super) build_queued: bool,
    pub(super) retain_source_archive: bool,
    pub(super) source_archive_refresh_required: bool,
}

impl GeneratorImportIntent {
    fn external(image: Option<String>, digest: Option<String>) -> Self {
        Self {
            image,
            digest,
            build_context_subdir: None,
            build_status: ChallengeBuildStatus::None,
            last_build_log: None,
            automatic: false,
            build_queued: false,
            retain_source_archive: false,
            source_archive_refresh_required: false,
        }
    }
}

fn regular_generator_context(package_dir: &Path) -> Option<PathBuf> {
    let context = package_dir.join(GENERATOR_CONTEXT_SUBDIR);
    let dockerfile = context.join("Dockerfile");
    let context_metadata = std::fs::symlink_metadata(&context).ok()?;
    let dockerfile_metadata = std::fs::symlink_metadata(dockerfile).ok()?;
    (context_metadata.file_type().is_dir() && dockerfile_metadata.file_type().is_file())
        .then_some(context)
}

fn authored_generator_pair(model: &ChallengeYaml) -> AppResult<Option<(String, String)>> {
    match (
        model.variant_generator_image.as_deref(),
        model.variant_generator_digest.as_deref(),
    ) {
        (None, None) => Ok(None),
        (Some(image), Some(digest)) => {
            let image = image.trim();
            let digest = digest.trim();
            if image.is_empty() || digest.is_empty() {
                return Err(AppError::bad_request(
                    "variantGeneratorImage and variantGeneratorDigest must be non-empty when supplied",
                ));
            }
            Ok(Some((image.to_string(), digest.to_string())))
        }
        _ => Err(AppError::bad_request(
            "variantGeneratorImage and variantGeneratorDigest must be supplied together",
        )),
    }
}

async fn generator_source_matches(
    st: &SharedState,
    existing: &game_challenge::Model,
    requested_context: &Path,
) -> AppResult<bool> {
    if existing.variant_generator_build_context_subdir.as_deref() != Some(GENERATOR_CONTEXT_SUBDIR)
    {
        return Ok(false);
    }
    let Some(archive_path) = existing
        .original_archive_blob_path
        .as_deref()
        .filter(|path| !path.trim().is_empty())
    else {
        return Ok(false);
    };
    let archive = match st
        .storage
        .load_bounded(
            archive_path,
            crate::utils::upload::SOURCE_ARCHIVE_BLOB_BYTES,
        )
        .await
    {
        Ok(archive) => archive,
        Err(_) => return Ok(false),
    };
    let current = archived_context_fingerprint(archive, GENERATOR_CONTEXT_SUBDIR).await?;
    let requested = context_fingerprint(requested_context).await?;
    Ok(current == requested)
}

fn successful_local_identity(existing: &game_challenge::Model) -> Option<&str> {
    if existing.variant_generator_build_status != ChallengeBuildStatus::Success {
        return None;
    }
    let image = existing.variant_generator_image.as_deref()?;
    let digest = existing.variant_generator_digest.as_deref()?;
    (image == digest && crate::services::challenge_images::is_local_image_id(image))
        .then_some(image)
}

pub(super) fn ensure_source_archive_refresh_allowed(
    preserve_live_runtime: bool,
    source_archive_refresh_required: bool,
) -> AppResult<()> {
    if preserve_live_runtime && source_archive_refresh_required {
        return Err(AppError::conflict(
            "the enabled live runtime retains its published source archive; disable the challenge, rescan to build the changed generator, then re-enable it",
        ));
    }
    Ok(())
}

pub(super) async fn resolve_generator_import_intent(
    st: &SharedState,
    model: &ChallengeYaml,
    existing: Option<&game_challenge::Model>,
    package_dir: &Path,
    variant_mode: ChallengeVariantMode,
    policy: ImportPolicy,
) -> AppResult<GeneratorImportIntent> {
    let authored = authored_generator_pair(model)?;
    if let Some((image, digest)) = authored {
        return Ok(GeneratorImportIntent::external(Some(image), Some(digest)));
    }

    if model.variant_mode == Some(ChallengeVariantMode::Disabled)
        && existing.is_some_and(|challenge| {
            challenge.variant_generator_build_context_subdir.as_deref()
                == Some(GENERATOR_CONTEXT_SUBDIR)
        })
    {
        return Ok(GeneratorImportIntent::external(None, None));
    }

    let context = regular_generator_context(package_dir);
    let existing_is_automatic = existing.is_some_and(|challenge| {
        challenge.variant_generator_build_context_subdir.as_deref()
            == Some(GENERATOR_CONTEXT_SUBDIR)
    });
    let automatic = variant_mode == ChallengeVariantMode::PerParticipation
        && (context.is_some() || existing_is_automatic);
    if !automatic {
        return Ok(GeneratorImportIntent::external(
            existing.and_then(|challenge| challenge.variant_generator_image.clone()),
            existing.and_then(|challenge| challenge.variant_generator_digest.clone()),
        ));
    }
    if !policy.may_execute() {
        return Err(AppError::bad_request(
            "generator/Dockerfile auto-build is available only to trusted repository imports",
        ));
    }
    let context = context.ok_or_else(|| {
        AppError::bad_request(
            "PerParticipation without explicit image fields requires a regular generator/Dockerfile",
        )
    })?;

    let source_matches = match existing {
        Some(existing) => generator_source_matches(st, existing, &context).await?,
        None => false,
    };
    let ready = if source_matches {
        match existing.and_then(successful_local_identity) {
            Some(image) => st.containers.image_exists(image).await,
            None => false,
        }
    } else {
        false
    };
    if ready {
        let existing = existing.expect("ready automatic generator has an existing row");
        return Ok(GeneratorImportIntent {
            image: existing.variant_generator_image.clone(),
            digest: existing.variant_generator_digest.clone(),
            build_context_subdir: Some(GENERATOR_CONTEXT_SUBDIR.to_string()),
            build_status: ChallengeBuildStatus::Success,
            last_build_log: existing.variant_generator_last_build_log.clone(),
            automatic: true,
            build_queued: false,
            retain_source_archive: true,
            source_archive_refresh_required: false,
        });
    }

    Ok(GeneratorImportIntent {
        image: None,
        digest: None,
        build_context_subdir: Some(GENERATOR_CONTEXT_SUBDIR.to_string()),
        build_status: ChallengeBuildStatus::Queued,
        last_build_log: None,
        automatic: true,
        build_queued: true,
        retain_source_archive: true,
        source_archive_refresh_required: !source_matches,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_generator_fields_must_be_a_complete_pair() {
        let model = ChallengeYaml {
            variant_generator_image: Some("registry.example/generator:tag".to_string()),
            ..Default::default()
        };
        assert!(authored_generator_pair(&model).is_err());
    }

    #[test]
    fn generator_context_rejects_symlinked_dockerfiles() {
        let root = std::env::temp_dir().join(format!(
            "rsctf-generator-context-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(root.join("generator")).unwrap();
        std::fs::write(root.join("real-Dockerfile"), "FROM scratch\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            root.join("real-Dockerfile"),
            root.join("generator/Dockerfile"),
        )
        .unwrap();
        #[cfg(unix)]
        assert!(regular_generator_context(&root).is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn changed_generator_cannot_replace_an_enabled_runtime_archive() {
        assert!(ensure_source_archive_refresh_allowed(true, true).is_err());
        assert!(ensure_source_archive_refresh_allowed(true, false).is_ok());
        assert!(ensure_source_archive_refresh_allowed(false, true).is_ok());
    }
}
