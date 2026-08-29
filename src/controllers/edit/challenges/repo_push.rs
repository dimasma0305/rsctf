//! Ordered, best-effort challenge push-back for repository-bound games.

use super::*;

use crate::models::data::repo_binding;
use crate::services::git_sync;

struct PushPayload {
    binding_id: i32,
    token: String,
    challenge_id: i32,
    revision: i64,
    manifest: std::path::PathBuf,
    relative_manifest: String,
    yaml: String,
}

enum SnapshotResult {
    Ready(PushPayload),
    Retry,
    Skip,
}

enum DatabaseSnapshot {
    Ready(
        Box<repo_binding::Model>,
        Box<game_challenge::Model>,
        Vec<String>,
    ),
    Retry,
    Skip,
}

/// Enqueue identifiers only. Every queued edit re-reads the current durable
/// state after acquiring the checkout lock, so delayed tasks cannot push an old
/// in-memory challenge snapshot after a newer save.
pub(super) async fn enqueue_locked(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
    revision: i64,
) -> AppResult<()> {
    sqlx::query(
        r#"INSERT INTO "RepoBindingPushJobs"
                  (binding_id, challenge_id, game_id, requested_revision)
           SELECT game.repo_binding_id, $2, game.id, $3
             FROM "Games" game
             JOIN "RepoBindings" binding ON binding.id = game.repo_binding_id
            WHERE game.id = $1 AND binding.push_on_edit = TRUE
           ON CONFLICT (binding_id, challenge_id) DO UPDATE
             SET requested_revision = GREATEST(
                     "RepoBindingPushJobs".requested_revision,
                     EXCLUDED.requested_revision),
                 game_id = EXCLUDED.game_id,
                 updated_at_utc = clock_timestamp(),
                 last_error = NULL"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(revision)
    .execute(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

pub(crate) struct ClaimedRepoPushBatch {
    pub binding_id: i32,
    pub lease_token: Uuid,
    jobs: Vec<(i32, i32, i64)>,
}

const MAX_FILES_PER_PUSH: i64 = 64;

pub(crate) async fn claim_jobs(
    pool: &sqlx::PgPool,
    limit: i64,
) -> AppResult<Vec<ClaimedRepoPushBatch>> {
    let token = Uuid::new_v4();
    let binding_ids = sqlx::query_scalar::<_, i32>(
        r#"WITH due AS (
               SELECT binding.id
                 FROM "RepoBindings" binding
                WHERE (binding.push_lease_until IS NULL
                       OR binding.push_lease_until <= clock_timestamp())
                  AND EXISTS (
                      SELECT 1 FROM "RepoBindingPushJobs" job
                       WHERE job.binding_id = binding.id
                         AND job.updated_at_utc <= clock_timestamp()
                  )
                ORDER BY binding.id
                FOR UPDATE SKIP LOCKED
                LIMIT $1
           )
           UPDATE "RepoBindings" binding
              SET push_lease_token = $2,
                  push_lease_until = clock_timestamp() + INTERVAL '5 minutes'
             FROM due
            WHERE binding.id = due.id
           RETURNING binding.id"#,
    )
    .bind(limit.clamp(1, 8))
    .bind(token)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if binding_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<_, (i32, i32, i32, i64)>(
        r#"SELECT binding_id, game_id, challenge_id, requested_revision
             FROM (
                 SELECT job.*,
                        row_number() OVER (
                            PARTITION BY binding_id
                            ORDER BY updated_at_utc, challenge_id
                        ) AS position
                   FROM "RepoBindingPushJobs" job
                  WHERE binding_id = ANY($1)
                    AND updated_at_utc <= clock_timestamp()
             ) bounded
            WHERE position <= $2
            ORDER BY binding_id, position"#,
    )
    .bind(&binding_ids)
    .bind(MAX_FILES_PER_PUSH)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let mut jobs = std::collections::BTreeMap::<i32, Vec<(i32, i32, i64)>>::new();
    for (binding_id, game_id, challenge_id, revision) in rows {
        jobs.entry(binding_id)
            .or_default()
            .push((game_id, challenge_id, revision));
    }
    Ok(binding_ids
        .into_iter()
        .filter_map(|binding_id| {
            jobs.remove(&binding_id).map(|jobs| ClaimedRepoPushBatch {
                binding_id,
                lease_token: token,
                jobs,
            })
        })
        .collect())
}

pub(crate) async fn run_claimed_job(
    st: &SharedState,
    batch: ClaimedRepoPushBatch,
) -> AppResult<()> {
    let result = push_batch(st, &batch).await;
    let mut transaction = st
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    match &result {
        Ok(pushed) => {
            for (challenge_id, pushed_revision) in pushed {
                sqlx::query(
                    r#"DELETE FROM "RepoBindingPushJobs"
                    WHERE binding_id = $1 AND challenge_id = $2
                      AND requested_revision <= $3"#,
                )
                .bind(batch.binding_id)
                .bind(challenge_id)
                .bind(pushed_revision)
                .execute(&mut *transaction)
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            }
        }
        Err(error) => {
            let challenge_ids = batch.jobs.iter().map(|job| job.1).collect::<Vec<_>>();
            sqlx::query(
                r#"UPDATE "RepoBindingPushJobs"
                      SET attempts = attempts + 1,
                          last_error = $3,
                          updated_at_utc = clock_timestamp() + make_interval(
                              secs => LEAST(3600, 15 * (1 << LEAST(attempts, 7))))
                    WHERE binding_id = $1 AND challenge_id = ANY($2)"#,
            )
            .bind(batch.binding_id)
            .bind(&challenge_ids)
            .bind(error.to_string().chars().take(2_000).collect::<String>())
            .execute(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        }
    }
    sqlx::query(
        r#"UPDATE "RepoBindings"
              SET push_lease_token = NULL, push_lease_until = NULL
            WHERE id = $1 AND push_lease_token = $2"#,
    )
    .bind(batch.binding_id)
    .bind(batch.lease_token)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    result.map(|_| ())
}

#[cfg(test)]
async fn current_binding_id(st: &SharedState, game_id: i32) -> AppResult<Option<i32>> {
    sqlx::query_scalar(r#"SELECT repo_binding_id FROM "Games" WHERE id = $1"#)
        .bind(game_id)
        .fetch_optional(st.pg())
        .await
        .map(|row: Option<Option<i32>>| row.flatten())
        .map_err(|error| AppError::internal(error.to_string()))
}

async fn push_batch(st: &SharedState, batch: &ClaimedRepoPushBatch) -> AppResult<Vec<(i32, i64)>> {
    let dest = std::path::PathBuf::from(&st.config.storage_root)
        .join("repos")
        .join(batch.binding_id.to_string());
    let _checkout = git_sync::lock_checkout_distributed(st.pg(), &dest).await?;
    let Some(initial_binding) = repo_binding::Entity::find_by_id(batch.binding_id)
        .one(&st.db)
        .await?
    else {
        return Ok(batch.jobs.iter().map(|job| (job.1, i64::MAX)).collect());
    };
    if !initial_binding.push_on_edit {
        return Ok(batch.jobs.iter().map(|job| (job.1, i64::MAX)).collect());
    }
    let Some(token) = initial_binding
        .github_token
        .as_deref()
        .filter(|token| !token.is_empty())
        .map(str::to_string)
    else {
        return Err(AppError::conflict(
            "repository push-back is enabled but no write token is configured",
        ));
    };
    let repo_url = git_sync::validate_binding_repo_url(&initial_binding.repo_url)?;
    let git_ref = git_sync::validate_git_ref(initial_binding.git_ref.as_deref())?;
    let auth_url = git_sync::GitCredentials::new(token).apply(&repo_url);
    git_sync::sync_repo(&auth_url, git_ref.as_deref(), &dest).await?;

    let mut payloads = Vec::with_capacity(batch.jobs.len());
    let mut completed = Vec::with_capacity(batch.jobs.len());
    for (game_id, challenge_id, _) in &batch.jobs {
        match snapshot_after_checkout(
            st,
            *game_id,
            *challenge_id,
            batch.binding_id,
            &initial_binding,
            &dest,
        )
        .await?
        {
            SnapshotResult::Retry => {
                return Err(AppError::conflict(
                    "repository binding changed while push-back was queued",
                ));
            }
            SnapshotResult::Skip => completed.push((*challenge_id, i64::MAX)),
            SnapshotResult::Ready(payload) => payloads.push(payload),
        }
    }
    for payload in &payloads {
        tokio::fs::write(&payload.manifest, &payload.yaml)
            .await
            .map_err(|error| {
                AppError::internal(format!(
                    "push-back: write {}: {error}",
                    payload.manifest.display()
                ))
            })?;
    }
    if !payloads.is_empty() {
        let paths = payloads
            .iter()
            .map(|payload| payload.relative_manifest.as_str())
            .collect::<Vec<_>>();
        git_sync::push_files(
            &dest,
            &paths,
            &repo_url,
            payloads[0].token.as_str(),
            &format!("chore: update {} challenge(s) from rsctf", payloads.len()),
        )
        .await?;
        for payload in payloads {
            tracing::info!(
                binding = payload.binding_id,
                challenge = payload.challenge_id,
                yaml = %payload.relative_manifest,
                "push-back: pushed latest database state"
            );
            completed.push((payload.challenge_id, payload.revision));
        }
    }
    Ok(completed)
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
    let mut game_lock = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    crate::utils::single_flight::acquire_transaction_advisory_lock(
        game_lock.transaction_mut(),
        &crate::services::challenge_workloads::definition_lock_key(game_id, challenge_id),
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let database_snapshot: AppResult<DatabaseSnapshot> = async {
        match super::reject_pending_mutation(
            &mut **game_lock.transaction_mut(),
            game_id,
            challenge_id,
        )
        .await
        {
            Ok(()) => {}
            Err(AppError::Conflict(_)) | Err(AppError::NotFound(_)) => {
                return Ok(DatabaseSnapshot::Skip);
            }
            Err(error) => return Err(error),
        }
        let binding_id = sqlx::query_scalar::<_, Option<i32>>(
            r#"SELECT repo_binding_id FROM "Games" WHERE id = $1"#,
        )
        .bind(game_id)
        .fetch_optional(&mut **game_lock.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if binding_id.flatten() != Some(expected_binding_id) {
            return Ok(DatabaseSnapshot::Retry);
        }
        let Some(binding_json) = sqlx::query_scalar::<_, serde_json::Value>(
            r#"SELECT to_jsonb(binding) || jsonb_build_object(
                       'status', CASE binding.status
                           WHEN 0 THEN 'Active' WHEN 1 THEN 'Paused' END)
                 FROM "RepoBindings" binding WHERE id = $1"#,
        )
        .bind(expected_binding_id)
        .fetch_optional(&mut **game_lock.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        else {
            return Ok(DatabaseSnapshot::Skip);
        };
        let binding: repo_binding::Model =
            serde_json::from_value(binding_json).map_err(|error| {
                AppError::internal(format!("could not decode binding row: {error}"))
            })?;
        if !binding.push_on_edit {
            return Ok(DatabaseSnapshot::Skip);
        }
        if binding.repo_url != synced_binding.repo_url
            || binding.git_ref != synced_binding.git_ref
            || binding.github_token != synced_binding.github_token
        {
            return Ok(DatabaseSnapshot::Retry);
        }
        let challenge =
            load_challenge_locked(game_lock.transaction_mut(), game_id, challenge_id).await?;
        let flag_texts = if challenge.challenge_type == ChallengeType::DynamicContainer {
            Vec::new()
        } else {
            sqlx::query_scalar::<_, String>(
                r#"SELECT flag FROM "FlagContexts"
                    WHERE challenge_id = $1 ORDER BY id"#,
            )
            .bind(challenge.id)
            .fetch_all(&mut **game_lock.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
            .into_iter()
            .filter_map(|flag| {
                let flag = flag.trim().to_string();
                (!flag.is_empty()).then_some(flag)
            })
            .collect()
        };
        Ok(DatabaseSnapshot::Ready(
            Box::new(binding),
            Box::new(challenge),
            flag_texts,
        ))
    }
    .await;
    game_lock
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let (binding, challenge, flag_texts) = match database_snapshot? {
        DatabaseSnapshot::Skip => return Ok(SnapshotResult::Skip),
        DatabaseSnapshot::Retry => return Ok(SnapshotResult::Retry),
        DatabaseSnapshot::Ready(binding, challenge, flags) => (binding, challenge, flags),
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
        token,
        challenge_id: challenge.id,
        revision: challenge.revision,
        manifest,
        relative_manifest,
        yaml,
    }))
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
