//! git_sync::attachment — package + attach a challenge artifact (`provide:` /
//! `dist/`), split from git_sync/mod.rs to stay under the 1000-line rule.
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use crate::app_state::SharedState;
use crate::utils::enums::FileType;
use crate::utils::error::{AppError, AppResult};

const MAX_ATTACHMENT_FILES: usize = 2_048;
const MAX_ATTACHMENT_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ATTACHMENT_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ATTACHMENT_DEPTH: usize = 32;

pub(super) enum AttachmentIntent {
    Keep,
    Clear,
    Failed,
    Staged(crate::services::blob_refs::StagedBlob),
}

enum PreparedAttachment {
    Keep,
    Clear,
    Failed,
    Bytes { filename: String, bytes: Vec<u8> },
}

#[derive(Default)]
pub(super) struct AttachmentPostCommit {
    purge_hash: Option<String>,
    invalidate_hashes: Vec<String>,
    receipt: Option<(uuid::Uuid, String)>,
}

pub(super) struct AttachmentPublishFailure {
    pub(super) error: AppError,
    pub(super) post_commit: AttachmentPostCommit,
}

impl AttachmentPublishFailure {
    fn before_commit(error: AppError) -> Self {
        Self {
            error,
            post_commit: AttachmentPostCommit::default(),
        }
    }
}

fn attachment_already_applied(
    intent: &AttachmentIntent,
    old_attachment_id: Option<i32>,
    old_hash: Option<&str>,
    expected_hash: Option<&str>,
) -> bool {
    match intent {
        AttachmentIntent::Staged(_) => old_attachment_id.is_some() && old_hash == expected_hash,
        AttachmentIntent::Clear => old_attachment_id.is_none(),
        AttachmentIntent::Keep | AttachmentIntent::Failed => false,
    }
}

/// Package and stage an attachment before any game/definition fence is taken.
/// A malformed artifact remains best-effort (`Failed`), preserving the import
/// contract while ensuring object-store I/O never occurs in the owner swap.
pub(super) async fn stage_attachment(
    st: &SharedState,
    game_id: i32,
    package_dir: &Path,
    provide: Option<&str>,
    replace_existing: bool,
) -> AttachmentIntent {
    let prepared = prepare_attachment_intent(package_dir, provide, replace_existing).await;
    stage_prepared_attachment(st, game_id, prepared).await
}

async fn prepare_attachment_intent(
    package_dir: &Path,
    provide: Option<&str>,
    replace_existing: bool,
) -> PreparedAttachment {
    let has_explicit_source = provide.is_some_and(|value| !value.trim().is_empty());
    let implicit_source_absent = matches!(package_dir.join("dist").try_exists(), Ok(false));
    if !has_explicit_source && implicit_source_absent {
        return if replace_existing {
            PreparedAttachment::Clear
        } else {
            PreparedAttachment::Keep
        };
    }
    let package_dir = package_dir.to_path_buf();
    let provide = provide.map(str::to_owned);
    let packaged =
        tokio::task::spawn_blocking(move || prepare_attachment(&package_dir, provide.as_deref()))
            .await;
    let Some((filename, bytes)) = (match packaged {
        Ok(packaged) => packaged,
        Err(error) => {
            tracing::warn!(%error, "git_sync: attachment packaging task failed");
            return PreparedAttachment::Failed;
        }
    }) else {
        return PreparedAttachment::Failed;
    };
    PreparedAttachment::Bytes { filename, bytes }
}

async fn stage_prepared_attachment(
    st: &SharedState,
    game_id: i32,
    prepared: PreparedAttachment,
) -> AttachmentIntent {
    let (filename, bytes) = match prepared {
        PreparedAttachment::Keep => return AttachmentIntent::Keep,
        PreparedAttachment::Clear => return AttachmentIntent::Clear,
        PreparedAttachment::Failed => return AttachmentIntent::Failed,
        PreparedAttachment::Bytes { filename, bytes } => (filename, bytes),
    };
    match crate::services::blob_refs::stage_blob(
        st.pg(),
        st.storage.as_ref(),
        uuid::Uuid::new_v4(),
        &format!("git-sync-attachment:{game_id}"),
        None,
        &filename,
        &bytes,
    )
    .await
    {
        Ok(stage) => AttachmentIntent::Staged(stage),
        Err(error) => {
            tracing::warn!(%error, "git_sync: attachment staging failed");
            AttachmentIntent::Failed
        }
    }
}

/// Publish a pre-staged attachment with its owner/ref metadata in one short
/// transaction. Same-content imports consume the stage without acquiring a
/// second reference or leaving a Ready lease behind.
pub(super) async fn publish_attachment(
    st: &SharedState,
    challenge_id: i32,
    intent: &AttachmentIntent,
    replace_existing: bool,
) -> Result<(bool, AttachmentPostCommit), AttachmentPublishFailure> {
    match intent {
        AttachmentIntent::Keep => return Ok((true, AttachmentPostCommit::default())),
        AttachmentIntent::Failed => return Ok((false, AttachmentPostCommit::default())),
        AttachmentIntent::Clear | AttachmentIntent::Staged(_) => {}
    }
    let expected_hash = match intent {
        AttachmentIntent::Staged(stage) => Some(stage.blob.hash.as_str()),
        _ => None,
    };
    let owner_scope = format!("challenge-attachment:{challenge_id}");
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| {
            AttachmentPublishFailure::before_commit(AppError::internal(error.to_string()))
        })?;
    let operation: AppResult<AttachmentPostCommit> = async {
        let (old_attachment_id, old_hash) = sqlx::query_as::<_, (Option<i32>, Option<String>)>(
            r#"SELECT challenge.attachment_id, file.hash
                  FROM "GameChallenges" challenge
                  LEFT JOIN "Attachments" attachment
                    ON attachment.id = challenge.attachment_id
                  LEFT JOIN "Files" file ON file.id = attachment.local_file_id
                 WHERE challenge.id = $1
                 FOR UPDATE OF challenge"#,
        )
        .bind(challenge_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::not_found("Challenge not found"))?;

        crate::services::blob_refs::lock_direct_hashes_locked(
            &mut transaction,
            old_hash
                .iter()
                .map(String::as_str)
                .chain(expected_hash.into_iter()),
        )
        .await?;
        let mut invalidate_hashes = old_hash.iter().cloned().collect::<Vec<_>>();
        if let Some(hash) = expected_hash {
            if !invalidate_hashes.iter().any(|value| value == hash) {
                invalidate_hashes.push(hash.to_string());
            }
        }
        let already_applied = attachment_already_applied(
            intent,
            old_attachment_id,
            old_hash.as_deref(),
            expected_hash,
        );
        if already_applied {
            if let AttachmentIntent::Staged(stage) = intent {
                stage
                    .consume_with_existing_reference_as(&mut transaction, &owner_scope)
                    .await?;
            }
            return Ok(AttachmentPostCommit {
                purge_hash: None,
                invalidate_hashes,
                receipt: match intent {
                    AttachmentIntent::Staged(stage) => {
                        Some((stage.operation_id, owner_scope.clone()))
                    }
                    _ => None,
                },
            });
        }
        if old_attachment_id.is_some() && !replace_existing {
            return Err(AppError::conflict(
                "challenge attachment was populated concurrently",
            ));
        }

        let new_attachment_id = if let AttachmentIntent::Staged(stage) = intent {
            let file_id = crate::services::blob_refs::publish_staged_blob_for_owner(
                &mut transaction,
                stage,
                &owner_scope,
            )
            .await?;
            Some(
                sqlx::query_scalar::<_, i32>(
                    r#"INSERT INTO "Attachments" ("Type", remote_url, local_file_id)
                       VALUES ($1, NULL, $2)
                       RETURNING id"#,
                )
                .bind(FileType::Local as i16)
                .bind(file_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(|error| AppError::internal(error.to_string()))?,
            )
        } else {
            None
        };
        sqlx::query(r#"UPDATE "GameChallenges" SET attachment_id = $2 WHERE id = $1"#)
            .bind(challenge_id)
            .bind(new_attachment_id)
            .execute(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        let purge_hash = match old_attachment_id {
            Some(attachment_id) => {
                crate::services::blob_refs::delete_attachment_locked(
                    &mut transaction,
                    attachment_id,
                )
                .await?
            }
            None => None,
        };
        Ok(AttachmentPostCommit {
            purge_hash,
            invalidate_hashes,
            receipt: match intent {
                AttachmentIntent::Staged(stage) => Some((stage.operation_id, owner_scope.clone())),
                _ => None,
            },
        })
    }
    .await;
    let post_commit = match operation {
        Ok(post_commit) => post_commit,
        Err(error) => {
            let _ = transaction.rollback().await;
            return Err(AttachmentPublishFailure::before_commit(error));
        }
    };
    if let Err(error) = transaction.commit().await {
        // PostgreSQL may have committed even when the acknowledgement was
        // lost. Return the exact old/new hashes and publication receipt so the
        // caller can conservatively invalidate authorization gates and finish
        // idempotent cleanup after releasing every domain fence.
        return Err(AttachmentPublishFailure {
            error: AppError::internal(error.to_string()),
            post_commit,
        });
    }
    Ok((true, post_commit))
}

pub(super) async fn discard_attachment(st: &SharedState, intent: &AttachmentIntent) {
    let AttachmentIntent::Staged(stage) = intent else {
        return;
    };
    if let Err(error) =
        crate::services::blob_refs::discard_unpublished_stage(st.pg(), st.storage.as_ref(), stage)
            .await
    {
        tracing::warn!(%error, hash = %stage.blob.hash, "git_sync: attachment stage cleanup deferred");
    }
}

pub(super) async fn finish_attachment_post_commit(
    st: &SharedState,
    post_commit: AttachmentPostCommit,
) {
    for hash in &post_commit.invalidate_hashes {
        crate::controllers::assets::invalidate_asset_gate(st, hash).await;
    }
    if let Some((operation_id, owner_scope)) = post_commit.receipt {
        if let Err(error) = sqlx::query(
            r#"DELETE FROM "BlobStagingOperations"
                WHERE operation_id = $1 AND state = 'Published'
                  AND published_owner_scope = $2"#,
        )
        .bind(operation_id)
        .bind(owner_scope)
        .execute(st.pg())
        .await
        {
            tracing::warn!(%error, %operation_id, "git_sync: attachment receipt cleanup deferred");
        }
    }
    if let Some(hash) = post_commit.purge_hash {
        if let Err(error) =
            crate::services::blob_refs::purge_if_unreferenced(st.pg(), st.storage.as_ref(), &hash)
                .await
        {
            tracing::warn!(%error, %hash, "git_sync: replaced attachment purge deferred");
        }
    }
}

fn prepare_attachment(package_dir: &Path, provide: Option<&str>) -> Option<(String, Vec<u8>)> {
    let rel = match provide.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => p.replace('\\', "/").trim_start_matches('/').to_string(),
        None => {
            // No explicit provide → fall back to the `dist/` convention if present.
            if package_dir.join("dist").is_dir() {
                "dist".to_string()
            } else {
                return None;
            }
        }
    };
    // Reject traversal / absolute paths (RSCTF `provide` escape guard).
    if rel.contains("..") || Path::new(&rel).is_absolute() {
        tracing::warn!(rel, "git_sync: rejecting unsafe 'provide' path");
        return None;
    }
    let Some(absolute) = resolve_attachment_path(package_dir, &rel) else {
        tracing::warn!(rel, "git_sync: rejecting attachment path outside package");
        return None;
    };
    package_attachment(&absolute)
}

/// Backfill artifacts for challenges imported before attachment packaging was
/// available. Only manifests below the managed repository checkout are read.
pub async fn repair_missing_attachments(st: &SharedState) -> AppResult<u64> {
    let cleaned = reconcile_attachment_references(st).await?;
    if cleaned > 0 {
        tracing::info!(cleaned, "git_sync: removed orphan attachment records");
    }
    let repos_root = PathBuf::from(&st.config.storage_root).join("repos");
    let Ok(repos_root) = tokio::fs::canonicalize(repos_root).await else {
        return Ok(0);
    };
    let mut repaired = 0u64;
    let mut after_id = 0i32;
    loop {
        let challenges = sqlx::query_as::<_, (i32, i32, String)>(
            r#"SELECT challenge.id, game.repo_binding_id, challenge.source_yaml_path
                 FROM "GameChallenges" challenge
                 JOIN "Games" game ON game.id = challenge.game_id
                WHERE challenge.attachment_id IS NULL
                  AND challenge.source_yaml_path IS NOT NULL
                  AND game.repo_binding_id IS NOT NULL
                  AND challenge.id > $1
                ORDER BY challenge.id
                LIMIT 100"#,
        )
        .bind(after_id)
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        let Some((last_id, _, _)) = challenges.last() else {
            break;
        };
        after_id = *last_id;
        for (challenge_id, binding_id, source) in challenges {
            // Resolve only far enough to identify the managed checkout, then
            // take the same cross-replica lock used by scans/push-back and
            // resolve again under that guard. This prevents startup repair from
            // packaging a tree while another role is replacing its files.
            let checkout = repos_root.join(binding_id.to_string());
            let checkout_lock = super::lock_checkout_distributed(st.pg(), &checkout).await?;
            let Ok(locked_checkout) = tokio::fs::canonicalize(&checkout).await else {
                continue;
            };
            if !locked_checkout.starts_with(&repos_root) {
                continue;
            }
            let Some(candidate) =
                super::manifest_candidate_in_checkout(&locked_checkout, Some(binding_id), &source)
            else {
                continue;
            };
            let Ok(manifest) = tokio::fs::canonicalize(candidate).await else {
                continue;
            };
            let is_manifest = manifest
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| matches!(name, "challenge.yml" | "challenge.yaml"));
            if !is_manifest || !manifest.starts_with(&locked_checkout) {
                continue;
            }
            let provide = tokio::fs::read_to_string(&manifest)
                .await
                .ok()
                .and_then(|raw| serde_norway::from_str::<super::ChallengeYaml>(&raw).ok())
                .and_then(|model| model.provide);
            let package_dir = manifest.parent().unwrap_or(locked_checkout.as_path());
            let prepared = prepare_attachment_intent(package_dir, provide.as_deref(), false).await;
            drop(checkout_lock);
            let intent = stage_prepared_attachment(st, 0, prepared).await;
            match publish_attachment(st, challenge_id, &intent, false).await {
                Ok((true, post_commit)) => {
                    finish_attachment_post_commit(st, post_commit).await;
                    repaired += 1;
                }
                Ok((false, post_commit)) => {
                    finish_attachment_post_commit(st, post_commit).await;
                }
                Err(failure) => {
                    tracing::warn!(error = %failure.error, challenge_id, "git_sync: attachment repair failed");
                    finish_attachment_post_commit(st, failure.post_commit).await;
                    discard_attachment(st, &intent).await;
                }
            }
        }
    }
    Ok(repaired)
}

/// Repair legacy attachment/refcount drift before creating new links. A Files
/// row with no relational target may still be a deliberate standalone
/// `/api/assets` ownership reference, so reconciliation never guesses that it
/// is safe to remove metadata or physical content.
async fn reconcile_attachment_references(st: &SharedState) -> AppResult<u64> {
    let removed_attachments =
        crate::services::blob_refs::delete_orphan_attachments(st.pg(), st.storage.as_ref()).await?;
    let mut tx = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"UPDATE "Files" file
              SET reference_count = GREATEST(file.reference_count, refs.reference_count)
             FROM (
                   SELECT file_id, SUM(reference_count)::bigint AS reference_count
                     FROM (
                           SELECT local_file_id AS file_id,
                                  COUNT(*)::bigint AS reference_count
                             FROM "Attachments"
                            WHERE local_file_id IS NOT NULL
                            GROUP BY local_file_id
                           UNION ALL
                           SELECT writeup_id AS file_id,
                                  COUNT(*)::bigint AS reference_count
                             FROM "Participations"
                            WHERE writeup_id IS NOT NULL
                            GROUP BY writeup_id
                           UNION ALL
                           SELECT file.id, COUNT(*)::bigint
                             FROM "Files" file
                             JOIN "AspNetUsers" owner ON owner.avatar_hash = file.hash
                            GROUP BY file.id
                           UNION ALL
                           SELECT file.id, COUNT(*)::bigint
                             FROM "Files" file
                             JOIN "Teams" owner ON owner.avatar_hash = file.hash
                            GROUP BY file.id
                           UNION ALL
                           SELECT file.id, COUNT(*)::bigint
                             FROM "Files" file
                             JOIN "Games" owner ON owner.poster_hash = file.hash
                            GROUP BY file.id
                           UNION ALL
                           SELECT file.id, COUNT(*)::bigint
                             FROM "Files" file
                             JOIN "GameChallenges" owner
                               ON owner.original_archive_blob_path = file.hash
                            GROUP BY file.id
                           UNION ALL
                           SELECT file.id, 1::bigint
                             FROM "Files" file
                            WHERE EXISTS (
                                  SELECT 1 FROM "Configs" config
                                   WHERE config.config_key IN (
                                         'GlobalConfig:LogoHash',
                                         'GlobalConfig:FaviconHash'
                                   )
                                     AND config.value = file.hash
                            )
                     ) live_references
                    GROUP BY file_id
             ) refs
            WHERE file.id = refs.file_id"#,
    )
    .execute(&mut *tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    tx.commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(removed_attachments)
}

/// Resolve a repository-authored attachment path after following symlinks, then
/// require the result to remain below the canonical package root. Checking the
/// final path text alone is insufficient because an intermediate component can
/// be a Git symlink such as `root -> /`.
fn resolve_attachment_path(package_dir: &Path, rel: &str) -> Option<PathBuf> {
    let root = std::fs::canonicalize(package_dir).ok()?;
    let candidate = std::fs::canonicalize(package_dir.join(rel)).ok()?;
    candidate.starts_with(&root).then_some(candidate)
}

/// Read/package an attachment source into `(filename, bytes)`: a file → itself; a
/// single-file directory → that file; a multi-file directory → a zip. Symlinks are
/// skipped (never followed out of the package). `None` on any I/O error or empty
/// directory.
fn package_attachment(absolute: &Path) -> Option<(String, Vec<u8>)> {
    let meta = std::fs::symlink_metadata(absolute).ok()?;
    if meta.file_type().is_symlink() {
        return None;
    }
    if meta.is_file() {
        if meta.len() > MAX_ATTACHMENT_FILE_BYTES {
            return None;
        }
        let bytes = std::fs::read(absolute).ok()?;
        if bytes.len() as u64 > MAX_ATTACHMENT_FILE_BYTES {
            return None;
        }
        return Some((absolute.file_name()?.to_str()?.to_string(), bytes));
    }
    if meta.is_dir() {
        let mut files: Vec<PathBuf> = Vec::new();
        let mut total = 0u64;
        collect_attachment_files(absolute, &mut files, &mut total, 0)?;
        files.sort();
        if files.is_empty() {
            return None;
        }
        if files.len() == 1 {
            let bytes = std::fs::read(&files[0]).ok()?;
            if bytes.len() as u64 > MAX_ATTACHMENT_FILE_BYTES {
                return None;
            }
            return Some((files[0].file_name()?.to_str()?.to_string(), bytes));
        }
        let mut zw = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        let mut actual_total = 0u64;
        for f in &files {
            let rel = f.strip_prefix(absolute).ok()?.to_str()?;
            let bytes = std::fs::read(f).ok()?;
            let actual_len = bytes.len() as u64;
            if actual_len > MAX_ATTACHMENT_FILE_BYTES
                || actual_total.saturating_add(actual_len) > MAX_ATTACHMENT_TOTAL_BYTES
            {
                return None;
            }
            actual_total = actual_total.saturating_add(actual_len);
            zw.start_file(rel, opts).ok()?;
            zw.write_all(&bytes).ok()?;
        }
        let cursor = zw.finish().ok()?;
        return Some((
            format!("{}.zip", absolute.file_name()?.to_str()?),
            cursor.into_inner(),
        ));
    }
    None
}

/// Recursively collect regular files under `dir`, skipping symlinks (so a
/// dir-symlink can't tar an arbitrary host tree into a downloadable attachment).
fn collect_attachment_files(
    dir: &Path,
    out: &mut Vec<PathBuf>,
    total: &mut u64,
    depth: usize,
) -> Option<()> {
    if depth > MAX_ATTACHMENT_DEPTH {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries {
        let entry = entry.ok()?;
        let ft = entry.file_type().ok()?;
        if ft.is_symlink() {
            continue;
        }
        let path = entry.path();
        if ft.is_dir() {
            collect_attachment_files(&path, out, total, depth + 1)?;
        } else if ft.is_file() {
            let len = entry.metadata().ok()?.len();
            if len > MAX_ATTACHMENT_FILE_BYTES
                || total.saturating_add(len) > MAX_ATTACHMENT_TOTAL_BYTES
                || out.len() >= MAX_ATTACHMENT_FILES
            {
                return None;
            }
            *total = total.saturating_add(len);
            out.push(path);
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rsctf-{tag}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn attachment_resolution_accepts_only_canonical_descendants() {
        let root = temp_dir("attach-root");
        std::fs::create_dir_all(root.join("inside")).unwrap();
        std::fs::write(root.join("inside/file"), b"ok").unwrap();
        assert_eq!(
            resolve_attachment_path(&root, "inside/file"),
            Some(std::fs::canonicalize(root.join("inside/file")).unwrap())
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn clearing_a_remote_attachment_is_not_mistaken_for_a_hash_noop() {
        assert!(!attachment_already_applied(
            &AttachmentIntent::Clear,
            Some(41),
            None,
            None,
        ));
        assert!(attachment_already_applied(
            &AttachmentIntent::Clear,
            None,
            None,
            None,
        ));
    }

    #[cfg(unix)]
    #[test]
    fn attachment_resolution_rejects_intermediate_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = temp_dir("attach-link-root");
        let outside = temp_dir("attach-link-outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), b"secret").unwrap();
        symlink(&outside, root.join("link")).unwrap();

        assert!(resolve_attachment_path(&root, "link/secret").is_none());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn tcp1p_dist_directory_is_packaged_as_zip() {
        let root = temp_dir("tcp1p-dist");
        let dist = root.join("dist");
        std::fs::create_dir_all(&dist).unwrap();
        std::fs::write(dist.join("Dockerfile"), b"FROM python:3.12").unwrap();
        std::fs::write(dist.join("app.py"), b"print('throne')").unwrap();
        std::fs::write(dist.join("requirements.txt"), b"flask\n").unwrap();

        let (name, bytes) = prepare_attachment(&root, None).expect("implicit dist should package");
        assert_eq!(name, "dist.zip");
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut names: Vec<String> = (0..archive.len())
            .map(|index| archive.by_index(index).unwrap().name().to_string())
            .collect();
        names.sort();
        assert_eq!(names, ["Dockerfile", "app.py", "requirements.txt"]);
        assert!(prepare_attachment(&root, Some("./dist")).is_some());

        let no_dist = temp_dir("tcp1p-no-dist");
        std::fs::create_dir_all(&no_dist).unwrap();
        assert!(prepare_attachment(&no_dist, None).is_none());

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(no_dist);
    }
}
