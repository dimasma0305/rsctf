//! Ordered, best-effort challenge push-back for repository-bound games.

use super::*;

use crate::models::data::repo_binding;
use crate::services::git_sync;

struct PushPayload {
    token: String,
    title: String,
    manifest: std::path::PathBuf,
    relative_manifest: String,
    yaml: String,
    revision: i64,
}

enum SnapshotResult {
    Ready(PushPayload),
    Retry,
    Skip,
}

const PUSH_BATCH: i64 = 16;
const PUSH_LEASE_SECONDS: i32 = 900;

#[derive(Clone, Debug)]
struct ClaimedPush {
    binding_id: i32,
    challenge_id: i32,
    game_id: i32,
    target_revision: i64,
    owner: uuid::Uuid,
}

struct DatabaseSnapshot {
    token: String,
    challenge: git_sync::ChallengePushSnapshot,
    source_yaml_path: Option<String>,
    flag_texts: Vec<String>,
    revision: i64,
}

#[allow(clippy::large_enum_variant)]
enum DatabaseSnapshotResult {
    Ready(DatabaseSnapshot),
    Retry,
    Skip,
}

#[derive(sqlx::FromRow)]
struct ChallengeSnapshotRow {
    game_id: i32,
    title: String,
    content: String,
    category: i16,
    challenge_type: i16,
    hints: Option<serde_json::Value>,
    revision: i64,
    submission_limit: i32,
    container_image: Option<String>,
    memory_limit: Option<i32>,
    storage_limit: Option<i32>,
    cpu_count: Option<i32>,
    expose_port: Option<i32>,
    flag_template: Option<String>,
    enable_traffic_capture: bool,
    enable_shared_container: bool,
    disable_blood_bonus: bool,
    min_score_rate: f64,
    difficulty: f64,
    network_mode: Option<i16>,
    variant_mode: i16,
    variant_generator_image: Option<String>,
    variant_generator_digest: Option<String>,
    variant_generator_build_context_subdir: Option<String>,
    solve_receipt_mode: i16,
    receipt_verifier_identity: Option<String>,
    ad_checker_image: Option<String>,
    ad_allow_egress: bool,
    ad_allow_self_reset: bool,
    ad_ssh_requires_flag: bool,
    ad_self_hosted: bool,
    source_yaml_path: Option<String>,
}

impl ChallengeSnapshotRow {
    fn into_snapshot(self) -> AppResult<(git_sync::ChallengePushSnapshot, Option<String>, i64)> {
        let category = <ChallengeCategory as sea_orm::ActiveEnum>::try_from_value(&self.category)
            .map_err(|error| AppError::internal(error.to_string()))?;
        let challenge_type =
            <ChallengeType as sea_orm::ActiveEnum>::try_from_value(&self.challenge_type)
                .map_err(|error| AppError::internal(error.to_string()))?;
        let network_mode = self
            .network_mode
            .map(|value| {
                <NetworkMode as sea_orm::ActiveEnum>::try_from_value(&value)
                    .map_err(|error| AppError::internal(error.to_string()))
            })
            .transpose()?;
        let variant_mode =
            <ChallengeVariantMode as sea_orm::ActiveEnum>::try_from_value(&self.variant_mode)
                .map_err(|error| AppError::internal(error.to_string()))?;
        let solve_receipt_mode =
            <SolveReceiptMode as sea_orm::ActiveEnum>::try_from_value(&self.solve_receipt_mode)
                .map_err(|error| AppError::internal(error.to_string()))?;
        let revision = self.revision;
        let source_yaml_path = self.source_yaml_path;
        Ok((
            git_sync::ChallengePushSnapshot {
                game_id: self.game_id,
                title: self.title,
                content: self.content,
                category,
                challenge_type,
                hints: self.hints,
                submission_limit: self.submission_limit,
                container_image: self.container_image,
                memory_limit: self.memory_limit,
                storage_limit: self.storage_limit,
                cpu_count: self.cpu_count,
                expose_port: self.expose_port,
                flag_template: self.flag_template,
                enable_traffic_capture: self.enable_traffic_capture,
                enable_shared_container: self.enable_shared_container,
                disable_blood_bonus: self.disable_blood_bonus,
                min_score_rate: self.min_score_rate,
                difficulty: self.difficulty,
                network_mode,
                variant_mode,
                variant_generator_image: self.variant_generator_image,
                variant_generator_digest: self.variant_generator_digest,
                variant_generator_build_context_subdir: self.variant_generator_build_context_subdir,
                solve_receipt_mode,
                receipt_verifier_identity: self.receipt_verifier_identity,
                ad_checker_image: self.ad_checker_image,
                ad_allow_egress: self.ad_allow_egress,
                ad_allow_self_reset: self.ad_allow_self_reset,
                ad_ssh_requires_flag: self.ad_ssh_requires_flag,
                ad_self_hosted: self.ad_self_hosted,
            },
            source_yaml_path,
            revision,
        ))
    }
}

async fn claim_batch(pool: &sqlx::PgPool) -> AppResult<Vec<ClaimedPush>> {
    let owner = uuid::Uuid::new_v4();
    let rows = sqlx::query_as::<_, (i32, i32, i32, i64)>(
        r#"WITH due_candidates AS MATERIALIZED (
               SELECT queue.binding_id, queue.available_at_utc
                 FROM "RepoPushQueue" queue
                WHERE queue.available_at_utc <= clock_timestamp()
                  AND (queue.lease_expires_at_utc IS NULL
                       OR queue.lease_expires_at_utc <= clock_timestamp())
                ORDER BY queue.available_at_utc, queue.binding_id, queue.challenge_id
                LIMIT 64
           ), candidate_bindings AS MATERIALIZED (
               SELECT candidate.binding_id, MIN(candidate.available_at_utc) AS first_due
                 FROM due_candidates candidate
                GROUP BY candidate.binding_id
           ), selected_binding AS MATERIALIZED (
               SELECT binding.id
                 FROM candidate_bindings candidate
                 JOIN "RepoBindings" binding ON binding.id = candidate.binding_id
                WHERE binding.push_on_edit = TRUE
                  AND NOT EXISTS (
                      SELECT 1 FROM "RepoPushQueue" active
                       WHERE active.binding_id = binding.id
                         AND active.lease_expires_at_utc > clock_timestamp()
                  )
                ORDER BY candidate.first_due, binding.id
                LIMIT 1
                FOR UPDATE OF binding SKIP LOCKED
           ), due AS MATERIALIZED (
               SELECT queue.binding_id, queue.challenge_id
                 FROM "RepoPushQueue" queue
                 JOIN selected_binding ON selected_binding.id = queue.binding_id
                WHERE queue.available_at_utc <= clock_timestamp()
                  AND (queue.lease_expires_at_utc IS NULL
                       OR queue.lease_expires_at_utc <= clock_timestamp())
                ORDER BY queue.available_at_utc, queue.challenge_id
                LIMIT $1
                FOR UPDATE OF queue SKIP LOCKED
           )
           UPDATE "RepoPushQueue" queue
              SET lease_owner = $2,
                  lease_expires_at_utc = clock_timestamp() + make_interval(secs => $3),
                  attempts = LEAST(1000000, queue.attempts + 1),
                  updated_at_utc = clock_timestamp()
             FROM due
            WHERE queue.binding_id = due.binding_id
              AND queue.challenge_id = due.challenge_id
        RETURNING queue.binding_id, queue.challenge_id, queue.game_id,
                  queue.target_revision"#,
    )
    .bind(PUSH_BATCH)
    .bind(owner)
    .bind(PUSH_LEASE_SECONDS)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(rows
        .into_iter()
        .map(|row| ClaimedPush {
            binding_id: row.0,
            challenge_id: row.1,
            game_id: row.2,
            target_revision: row.3,
            owner,
        })
        .collect())
}

async fn fail_batch(pool: &sqlx::PgPool, batch: &[ClaimedPush], error: &str) -> AppResult<()> {
    let Some(first) = batch.first() else {
        return Ok(());
    };
    let error = error.chars().take(2_000).collect::<String>();
    sqlx::query(
        r#"UPDATE "RepoPushQueue"
              SET lease_owner = NULL, lease_expires_at_utc = NULL,
                  available_at_utc = clock_timestamp() + make_interval(
                      secs => LEAST(300, power(2, LEAST(attempts, 8))::integer)
                  ),
                  last_error = $3, updated_at_utc = clock_timestamp()
            WHERE binding_id = $1 AND lease_owner = $2"#,
    )
    .bind(first.binding_id)
    .bind(first.owner)
    .bind(error)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

async fn complete_push(
    pool: &sqlx::PgPool,
    claim: &ClaimedPush,
    processed_revision: i64,
) -> AppResult<()> {
    sqlx::query(
        r#"DELETE FROM "RepoPushQueue"
            WHERE binding_id = $1 AND challenge_id = $2 AND lease_owner = $3
              AND target_revision <= $4"#,
    )
    .bind(claim.binding_id)
    .bind(claim.challenge_id)
    .bind(claim.owner)
    .bind(processed_revision)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"UPDATE "RepoPushQueue"
              SET lease_owner = NULL, lease_expires_at_utc = NULL,
                  available_at_utc = clock_timestamp(), last_error = NULL,
                  updated_at_utc = clock_timestamp()
            WHERE binding_id = $1 AND challenge_id = $2 AND lease_owner = $3"#,
    )
    .bind(claim.binding_id)
    .bind(claim.challenge_id)
    .bind(claim.owner)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

async fn discard_push(pool: &sqlx::PgPool, claim: &ClaimedPush) -> AppResult<()> {
    sqlx::query(
        r#"DELETE FROM "RepoPushQueue"
            WHERE binding_id = $1 AND challenge_id = $2 AND lease_owner = $3"#,
    )
    .bind(claim.binding_id)
    .bind(claim.challenge_id)
    .bind(claim.owner)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

async fn process_batch(st: &SharedState, batch: &[ClaimedPush]) -> AppResult<()> {
    let Some(first) = batch.first() else {
        return Ok(());
    };
    let Some(initial_binding) = repo_binding::Entity::find_by_id(first.binding_id)
        .one(&st.db)
        .await?
    else {
        return Ok(());
    };
    let repo_url = git_sync::validate_binding_repo_url(&initial_binding.repo_url)?;
    let host = reqwest::Url::parse(&repo_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .ok_or_else(|| AppError::bad_request("repository URL requires a host"))?;
    let host_lock =
        crate::utils::single_flight::PgSessionAdvisoryLock::acquire_repo_host(st.pg(), &host)
            .await
            .map_err(|error| AppError::internal(format!("lock repository host: {error}")))?;
    let dest = std::path::PathBuf::from(&st.config.storage_root)
        .join("repos")
        .join(first.binding_id.to_string());
    let checkout = git_sync::lock_checkout_distributed(st.pg(), &dest).await?;
    let Some(binding) = repo_binding::Entity::find_by_id(first.binding_id)
        .one(&st.db)
        .await?
    else {
        drop(checkout);
        host_lock
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(());
    };
    let Some(token) = binding
        .github_token
        .as_deref()
        .filter(|token| binding.push_on_edit && !token.is_empty())
        .map(str::to_string)
    else {
        for claim in batch {
            discard_push(st.pg(), claim).await?;
        }
        drop(checkout);
        host_lock
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(());
    };
    let git_ref = git_sync::validate_git_ref(binding.git_ref.as_deref())?;
    let auth_url = git_sync::GitCredentials::new(token).apply(&repo_url);
    git_sync::sync_repo(&auth_url, git_ref.as_deref(), &dest).await?;

    let mut payloads = Vec::with_capacity(batch.len());
    let mut skipped = Vec::new();
    for claim in batch {
        match snapshot_after_checkout(
            st,
            claim.game_id,
            claim.challenge_id,
            claim.binding_id,
            &binding,
            &dest,
        )
        .await?
        {
            SnapshotResult::Ready(payload) if payload.revision >= claim.target_revision => {
                payloads.push((claim, payload));
            }
            SnapshotResult::Ready(_) => {
                return Err(AppError::conflict(
                    "queued repository revision is newer than the visible challenge",
                ));
            }
            SnapshotResult::Retry => {
                return Err(AppError::conflict(
                    "repository binding changed while push-back was claimed",
                ));
            }
            SnapshotResult::Skip => skipped.push(claim),
        }
    }
    for (_, payload) in &payloads {
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
            .map(|(_, payload)| payload.relative_manifest.clone())
            .collect::<Vec<_>>();
        let message = if payloads.len() == 1 {
            format!(
                "chore: update {} from rsctf admin edit",
                payloads[0].1.title
            )
        } else {
            format!(
                "chore: update {} challenges from rsctf admin edits",
                payloads.len()
            )
        };
        git_sync::push_files(&dest, &paths, &repo_url, &payloads[0].1.token, &message).await?;
    }
    for (claim, payload) in payloads {
        complete_push(st.pg(), claim, payload.revision).await?;
    }
    for claim in skipped {
        discard_push(st.pg(), claim).await?;
    }
    drop(checkout);
    host_lock
        .release()
        .await
        .map_err(|error| AppError::internal(format!("unlock repository host: {error}")))?;
    Ok(())
}

async fn tick(st: &SharedState) -> AppResult<()> {
    let batch = claim_batch(st.pg()).await?;
    if batch.is_empty() {
        return Ok(());
    }
    if let Err(error) = process_batch(st, &batch).await {
        fail_batch(st.pg(), &batch, &error.to_string()).await?;
        return Err(error);
    }
    Ok(())
}

pub fn start(
    st: SharedState,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Err(error) = tick(&st).await {
                tracing::warn!(%error, "repository push queue tick failed");
            }
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { break; }
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
            }
        }
    })
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
    let database_snapshot: AppResult<DatabaseSnapshotResult> = async {
        match super::reject_pending_mutation(
            &mut **game_lock.transaction_mut(),
            game_id,
            challenge_id,
        )
        .await
        {
            Ok(()) => {}
            Err(AppError::Conflict(_)) | Err(AppError::NotFound(_)) => {
                return Ok(DatabaseSnapshotResult::Skip);
            }
            Err(error) => return Err(error),
        }
        let current_binding = sqlx::query_scalar::<_, Option<i32>>(
            r#"SELECT repo_binding_id FROM "Games" WHERE id = $1"#,
        )
        .bind(game_id)
        .fetch_optional(&mut **game_lock.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        let Some(current_binding) = current_binding else {
            return Ok(DatabaseSnapshotResult::Skip);
        };
        if current_binding != Some(expected_binding_id) {
            return Ok(DatabaseSnapshotResult::Retry);
        }
        let binding = sqlx::query_as::<_, (bool, String, Option<String>, Option<String>)>(
            r#"SELECT push_on_edit, repo_url, git_ref, github_token
                 FROM "RepoBindings" WHERE id = $1"#,
        )
        .bind(expected_binding_id)
        .fetch_optional(&mut **game_lock.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        let Some((push_on_edit, repo_url, git_ref, github_token)) = binding else {
            return Ok(DatabaseSnapshotResult::Skip);
        };
        if !push_on_edit {
            return Ok(DatabaseSnapshotResult::Skip);
        }
        if repo_url != synced_binding.repo_url
            || git_ref != synced_binding.git_ref
            || github_token.as_deref() != synced_binding.github_token.as_deref()
        {
            return Ok(DatabaseSnapshotResult::Retry);
        }
        let token = github_token
            .filter(|token| !token.is_empty())
            .ok_or_else(|| AppError::internal("push-back token disappeared"))?;
        let challenge = sqlx::query_as::<_, ChallengeSnapshotRow>(
            r#"SELECT game_id, title, content, category::smallint AS category,
                      "Type"::smallint AS challenge_type, hints, revision,
                      submission_limit, container_image, memory_limit,
                      storage_limit, cpu_count, expose_port, flag_template,
                      enable_traffic_capture, enable_shared_container,
                      disable_blood_bonus, min_score_rate, difficulty,
                      network_mode::smallint AS network_mode,
                      variant_mode::smallint AS variant_mode,
                      variant_generator_image, variant_generator_digest,
                      variant_generator_build_context_subdir,
                      solve_receipt_mode::smallint AS solve_receipt_mode,
                      receipt_verifier_identity, ad_checker_image,
                      ad_allow_egress, ad_allow_self_reset,
                      ad_ssh_requires_flag, ad_self_hosted, source_yaml_path
                 FROM "GameChallenges"
                WHERE id = $1 AND game_id = $2"#,
        )
        .bind(challenge_id)
        .bind(game_id)
        .fetch_optional(&mut **game_lock.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        let Some(challenge) = challenge else {
            return Ok(DatabaseSnapshotResult::Skip);
        };
        let (challenge, source_yaml_path, revision) = challenge.into_snapshot()?;
        let flag_texts = if challenge.challenge_type == ChallengeType::DynamicContainer {
            Vec::new()
        } else {
            sqlx::query_scalar::<_, String>(
                r#"SELECT flag FROM "FlagContexts" WHERE challenge_id = $1 ORDER BY id"#,
            )
            .bind(challenge_id)
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
        Ok(DatabaseSnapshotResult::Ready(DatabaseSnapshot {
            token,
            challenge,
            source_yaml_path,
            flag_texts,
            revision,
        }))
    }
    .await;
    game_lock
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let snapshot = match database_snapshot? {
        DatabaseSnapshotResult::Skip => return Ok(SnapshotResult::Skip),
        DatabaseSnapshotResult::Retry => return Ok(SnapshotResult::Retry),
        DatabaseSnapshotResult::Ready(snapshot) => snapshot,
    };
    let Some(manifest) = locate_owned_manifest(
        checkout,
        expected_binding_id,
        snapshot.source_yaml_path.as_deref(),
    )
    .await
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
    let source_yaml = tokio::fs::read_to_string(&manifest)
        .await
        .map_err(|error| {
            AppError::internal(format!(
                "push-back: read current manifest {}: {error}",
                manifest.display()
            ))
        })?;
    let yaml = git_sync::serialize_challenge_snapshot_preserving_source(
        &snapshot.challenge,
        &snapshot.flag_texts,
        &source_yaml,
    )?;
    Ok(SnapshotResult::Ready(PushPayload {
        token: snapshot.token,
        title: snapshot.challenge.title.clone(),
        manifest,
        relative_manifest,
        yaml,
        revision: snapshot.revision,
    }))
}

/// Push-back never adopts a same-title manifest. Only a binding-scoped durable
/// repository identity (including its exact safe legacy form) proves ownership.
async fn locate_owned_manifest(
    checkout: &std::path::Path,
    binding_id: i32,
    source_yaml_path: Option<&str>,
) -> Option<std::path::PathBuf> {
    let source = source_yaml_path.filter(|source| !source.is_empty())?;
    let candidate = git_sync::manifest_candidate_in_checkout(checkout, Some(binding_id), source)?;
    let checkout = tokio::fs::canonicalize(checkout).await.ok()?;
    let manifest = tokio::fs::canonicalize(candidate).await.ok()?;
    (manifest.is_file() && manifest.starts_with(checkout)).then_some(manifest)
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

#[cfg(test)]
mod queue_contract_tests {
    #[test]
    fn push_queue_is_durable_coalescing_batched_and_recoverable() {
        let source = include_str!("repo_push.rs");
        let production = source
            .split_once("#[cfg(test)]\nmod queue_contract_tests")
            .expect("queue contract tests remain after the production worker")
            .0;
        assert!(production.contains("WITH due_candidates AS MATERIALIZED"));
        assert!(production.contains("LIMIT 64"));
        assert!(production.contains("FOR UPDATE OF binding SKIP LOCKED"));
        assert!(production.contains("lease_expires_at_utc <= clock_timestamp()"));
        assert!(production.contains("LIMIT $1"));
        assert!(production.contains("git_sync::push_files"));
        assert!(production.contains("target_revision <= $4"));
        assert!(production.contains("acquire_transaction_advisory_lock"));
        assert!(production.contains("fetch_all(&mut **game_lock.transaction_mut())"));
        assert!(!production.contains("pub(super) fn spawn"));
        let release = production.find("game_lock\n        .release()").unwrap();
        let read = production
            .find("tokio::fs::read_to_string(&manifest)")
            .unwrap();
        assert!(release < read);
    }
}
