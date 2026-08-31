//! Installation-scoped Docker image inventory and garbage collection.

use super::{BuildImageModel, DeleteImageQuery, PruneResultModel};
use axum::extract::{Query, State};
use bollard::container::ListContainersOptions;
use bollard::errors::Error as DockerError;
use bollard::image::RemoveImageOptions;
use bollard::Docker;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, LazyLock, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::AdminUser;
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

const INVENTORY_OWNERSHIPS_SQL: &str = r#"SELECT canonical_ref, image_id, updated_at_utc, last_used_at_utc
 FROM "BuildImageOwnerships" WHERE installation_scope=$1 ORDER BY canonical_ref LIMIT $2"#;
const OWNERSHIP_SQL: &str = r#"SELECT canonical_ref, image_id, updated_at_utc, last_used_at_utc
 FROM "BuildImageOwnerships" WHERE installation_scope=$1 AND canonical_ref=$2"#;
const REFERENCES_SQL: &str = r#"
 SELECT title, container_image AS image_ref FROM "GameChallenges"
 WHERE container_image IS NOT NULL AND BTRIM(container_image)<>''
 UNION ALL
 SELECT title, ad_checker_image AS image_ref FROM "GameChallenges"
 WHERE ad_checker_image IS NOT NULL AND BTRIM(ad_checker_image)<>''
 UNION ALL
 SELECT title, variant_generator_image AS image_ref
 FROM "GameChallenges"
 WHERE variant_generator_build_context_subdir = 'generator'
   AND variant_generator_build_status = 1
   AND variant_generator_image IS NOT NULL
   AND variant_generator_image = variant_generator_digest"#;
const INVENTORY_REFERENCES_SQL: &str = r#"SELECT title, image_ref FROM (
 SELECT title, container_image AS image_ref FROM "GameChallenges"
 WHERE container_image IS NOT NULL AND BTRIM(container_image)<>''
 UNION ALL
 SELECT title, ad_checker_image AS image_ref FROM "GameChallenges"
 WHERE ad_checker_image IS NOT NULL AND BTRIM(ad_checker_image)<>''
 UNION ALL
 SELECT title, variant_generator_image AS image_ref FROM "GameChallenges"
 WHERE variant_generator_build_context_subdir = 'generator'
   AND variant_generator_build_status = 1
   AND variant_generator_image IS NOT NULL
   AND variant_generator_image = variant_generator_digest
) refs ORDER BY image_ref, title LIMIT $1"#;
const CLAIM_MANUAL_REMOVAL_SQL: &str = r#"UPDATE "BuildImageOwnerships"
   SET cleanup_claim_token=$4,
       cleanup_claim_until=clock_timestamp() + make_interval(secs => $5),
       cleanup_removal_started=TRUE
 WHERE installation_scope=$1 AND canonical_ref=$2 AND image_id=$3
   AND (cleanup_claim_until IS NULL OR cleanup_claim_until <= clock_timestamp())
   AND NOT EXISTS (
       SELECT 1 FROM "ControlPlaneResourceLeases"
        WHERE resource_key=$6
          AND lease_expires_at_utc > clock_timestamp()
   )"#;
const FINALIZE_MANUAL_CLAIM_SQL: &str = r#"UPDATE "BuildImageOwnerships"
   SET cleanup_claim_until = clock_timestamp() + make_interval(secs => $5),
       cleanup_removal_started = TRUE
 WHERE installation_scope=$1 AND canonical_ref=$2 AND image_id=$3
   AND cleanup_claim_token=$4
   AND cleanup_claim_until > clock_timestamp()
   AND NOT EXISTS (
       SELECT 1 FROM "ControlPlaneResourceLeases"
        WHERE resource_key=$6
          AND lease_expires_at_utc > clock_timestamp()
   )"#;
const INVENTORY_OWNERSHIP_LIMIT: i64 = 512;
const INVENTORY_REFERENCE_LIMIT: i64 = 10_000;
const INVENTORY_INSPECT_CONCURRENCY: usize = 4;
const MANUAL_PRUNE_LIMIT: i64 = 64;
const ADMIN_DAEMON_BUDGET: Duration = Duration::from_secs(60);
const DAEMON_OPERATION_BUDGET: Duration = Duration::from_secs(20);
const MANUAL_CLAIM_SECONDS: i32 = 2 * 60;
const INVENTORY_CACHE_TTL: Duration = Duration::from_secs(5);
static INVENTORY_GATE: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(1)));
static INVENTORY_FLIGHTS: LazyLock<crate::utils::single_flight::SingleFlight<ImageInventoryFill>> =
    LazyLock::new(crate::utils::single_flight::SingleFlight::new);
static INVENTORY_CACHE: LazyLock<RwLock<Option<(Instant, Vec<BuildImageModel>)>>> =
    LazyLock::new(|| RwLock::new(None));

#[derive(Clone, Default)]
enum ImageInventoryFill {
    Ready(Vec<BuildImageModel>),
    Busy,
    #[default]
    Failed,
}

fn cached_inventory() -> Option<Vec<BuildImageModel>> {
    INVENTORY_CACHE
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .filter(|(created, _)| created.elapsed() < INVENTORY_CACHE_TTL)
        .map(|(_, images)| images.clone())
}

fn store_inventory(images: Vec<BuildImageModel>) {
    *INVENTORY_CACHE
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((Instant::now(), images));
}

fn invalidate_inventory() {
    *INVENTORY_CACHE
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
struct OwnershipRow {
    canonical_ref: String,
    image_id: String,
    updated_at_utc: DateTime<Utc>,
    last_used_at_utc: Option<DateTime<Utc>>,
}

#[derive(Debug, sqlx::FromRow)]
struct ReferenceRow {
    title: String,
    image_ref: String,
}

async fn daemon_call<T, E, F>(
    deadline: tokio::time::Instant,
    label: &'static str,
    future: F,
) -> Result<T, String>
where
    E: std::fmt::Display,
    F: Future<Output = Result<T, E>>,
{
    let operation_deadline = deadline.min(tokio::time::Instant::now() + DAEMON_OPERATION_BUDGET);
    tokio::time::timeout_at(operation_deadline, future)
        .await
        .map_err(|_| format!("Docker {label} timed out"))?
        .map_err(|error| format!("Docker {label} failed: {error}"))
}

async fn reachable_docker(deadline: tokio::time::Instant) -> Result<Docker, String> {
    let docker = Docker::connect_with_local_defaults()
        .map_err(|error| format!("Docker connection failed: {error}"))?;
    let ping_deadline = deadline.min(tokio::time::Instant::now() + Duration::from_secs(2));
    match tokio::time::timeout_at(ping_deadline, docker.ping()).await {
        Ok(Ok(_)) => Ok(docker),
        Ok(Err(error)) => Err(format!("Docker daemon is unavailable: {error}")),
        Err(_) => Err("Docker daemon ping timed out".to_string()),
    }
}

fn docker_not_found(error: &DockerError) -> bool {
    matches!(
        error,
        DockerError::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}

fn reference_titles(
    rows: &[ReferenceRow],
    canonical_ref: &str,
    immutable_image_id: &str,
) -> Vec<String> {
    let mut titles = rows
        .iter()
        .filter(|row| {
            row.image_ref.eq_ignore_ascii_case(immutable_image_id)
                || crate::controllers::edit::canonical_image_reference(Some(&row.image_ref))
                    == canonical_ref
        })
        .map(|row| row.title.clone())
        .collect::<Vec<_>>();
    titles.sort_unstable();
    titles.dedup();
    titles
}

fn daemon_tag(inspected: &bollard::models::ImageInspect, canonical_ref: &str) -> Option<String> {
    inspected
        .repo_tags
        .as_ref()
        .into_iter()
        .flatten()
        .find(|tag| crate::controllers::edit::canonical_image_reference(Some(tag)) == canonical_ref)
        .cloned()
}

fn validate_inspect(
    inspected: &bollard::models::ImageInspect,
    ownership: &OwnershipRow,
    scope: &str,
) -> Result<String, String> {
    let current_id = crate::services::challenge_images::inspected_local_image_id(inspected)
        .ok_or_else(|| "Docker did not report a valid immutable image id".to_string())?;
    if !current_id.eq_ignore_ascii_case(&ownership.image_id) {
        return Err(format!(
            "ownership conflict: database expects {}, but Docker resolves the tag to {}",
            ownership.image_id, current_id
        ));
    }
    crate::services::challenge_images::validate_image_ownership_labels(
        inspected,
        scope,
        &ownership.canonical_ref,
        false,
    )?;
    daemon_tag(inspected, &ownership.canonical_ref)
        .ok_or_else(|| "Docker inspect omitted the owned canonical tag".to_string())
}

async fn inventory(
    st: &SharedState,
    docker: &Docker,
    deadline: tokio::time::Instant,
) -> AppResult<Vec<BuildImageModel>> {
    let policy = crate::services::container_policy::ContainerPolicy::load(st.pg()).await?;
    let scope = crate::services::container::docker_installation_scope();
    let ownerships = sqlx::query_as::<_, OwnershipRow>(INVENTORY_OWNERSHIPS_SQL)
        .bind(&scope)
        .bind(INVENTORY_OWNERSHIP_LIMIT)
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let mut references = sqlx::query_as::<_, ReferenceRow>(INVENTORY_REFERENCES_SQL)
        .bind(INVENTORY_REFERENCE_LIMIT + 1)
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let references_truncated = references.len() > INVENTORY_REFERENCE_LIMIT as usize;
    references.truncate(INVENTORY_REFERENCE_LIMIT as usize);
    let container_image_ids = daemon_call(
        deadline,
        "container inventory",
        docker.list_containers(Some(ListContainersOptions::<String> {
            all: true,
            filters: crate::services::container::managed_container_filters(&scope),
            ..Default::default()
        })),
    )
    .await
    .map_err(AppError::unavailable)?
    .into_iter()
    .filter_map(|container| container.image_id)
    .collect::<std::collections::HashSet<_>>();

    let inspected_ownerships =
        futures::stream::iter(ownerships.into_iter().map(|ownership| async move {
            let inspected = daemon_call(
                deadline,
                "image inspection",
                docker.inspect_image(&ownership.canonical_ref),
            )
            .await;
            (ownership, inspected)
        }))
        .buffer_unordered(INVENTORY_INSPECT_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

    let mut grouped = BTreeMap::<String, BuildImageModel>::new();
    for (ownership, inspected) in inspected_ownerships {
        let inspected = match inspected {
            Ok(inspected) => inspected,
            Err(error) => {
                tracing::warn!(tag=%ownership.canonical_ref, expected_image_id=%ownership.image_id,
                    %error, "owned build image is absent from Docker");
                continue;
            }
        };
        let tag = match validate_inspect(&inspected, &ownership, &scope) {
            Ok(tag) => tag,
            Err(error) => {
                tracing::warn!(tag=%ownership.canonical_ref, %error,
                    "owned build image identity conflict");
                continue;
            }
        };
        let referenced_by =
            reference_titles(&references, &ownership.canonical_ref, &ownership.image_id);
        let entry = grouped
            .entry(ownership.image_id.clone())
            .or_insert_with(|| BuildImageModel {
                id: ownership.image_id.clone(),
                tags: Vec::new(),
                size_bytes: inspected.size.unwrap_or_default(),
                created_utc: inspected.created,
                referenced: false,
                referenced_by: Vec::new(),
                is_checker: false,
                last_used_utc: ownership.last_used_at_utc,
                retention_expires_utc: ownership
                    .last_used_at_utc
                    .unwrap_or(ownership.updated_at_utc)
                    + chrono::Duration::hours(i64::from(policy.image_idle_retention_hours)),
                in_use: container_image_ids.contains(&ownership.image_id),
            });
        entry.tags.push(tag.clone());
        entry.referenced_by.extend(referenced_by);
        entry.referenced = !entry.referenced_by.is_empty();
        if references_truncated {
            // A truncated global reference snapshot must fail safe. The exact
            // delete path re-reads every reference under its image lock.
            entry.referenced = true;
        }
        entry.is_checker |= tag.contains("checker");
        entry.last_used_utc = entry.last_used_utc.max(ownership.last_used_at_utc);
        let expires = ownership
            .last_used_at_utc
            .unwrap_or(ownership.updated_at_utc)
            + chrono::Duration::hours(i64::from(policy.image_idle_retention_hours));
        entry.retention_expires_utc = entry.retention_expires_utc.max(expires);
        entry.in_use |= container_image_ids.contains(&ownership.image_id);
    }
    let mut images = grouped.into_values().collect::<Vec<_>>();
    for image in &mut images {
        image.tags.sort_unstable();
        image.tags.dedup();
        image.referenced_by.sort_unstable();
        image.referenced_by.dedup();
    }
    Ok(images)
}

pub async fn build_images(
    State(st): State<SharedState>,
    _admin: AdminUser,
) -> AppResult<RequestResponse<Vec<BuildImageModel>>> {
    if let Some(images) = cached_inventory() {
        return Ok(RequestResponse::ok(images));
    }
    let state = st.clone();
    let fill = INVENTORY_FLIGHTS
        .run_with_timeout(
            "build-image-inventory",
            Duration::from_secs(60),
            move || async move {
                if let Some(images) = cached_inventory() {
                    return ImageInventoryFill::Ready(images);
                }
                let _permit = match INVENTORY_GATE.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => return ImageInventoryFill::Busy,
                };
                let deadline = tokio::time::Instant::now() + ADMIN_DAEMON_BUDGET;
                let docker = match reachable_docker(deadline).await {
                    Ok(docker) => docker,
                    Err(error) => {
                        tracing::warn!(%error, "build image inventory daemon unavailable");
                        return ImageInventoryFill::Failed;
                    }
                };
                match inventory(&state, &docker, deadline).await {
                    Ok(images) => {
                        store_inventory(images.clone());
                        ImageInventoryFill::Ready(images)
                    }
                    Err(error) => {
                        tracing::warn!(%error, "build image inventory failed");
                        ImageInventoryFill::Failed
                    }
                }
            },
        )
        .await;
    match fill {
        ImageInventoryFill::Ready(images) => Ok(RequestResponse::ok(images)),
        ImageInventoryFill::Busy => Err(AppError::unavailable(
            "Build image inventory is already in progress",
        )),
        ImageInventoryFill::Failed => Err(AppError::unavailable(
            "Build image inventory is temporarily unavailable",
        )),
    }
}

pub async fn build_storage_status(
    State(st): State<SharedState>,
    _admin: AdminUser,
) -> AppResult<RequestResponse<crate::services::image_storage::ImageStorageStatus>> {
    Ok(RequestResponse::ok(
        crate::services::image_storage::storage_status(&st).await?,
    ))
}

pub async fn cleanup_build_storage(
    State(st): State<SharedState>,
    _admin: AdminUser,
) -> AppResult<RequestResponse<crate::services::image_storage::ImageCleanupReport>> {
    let policy = crate::services::container_policy::ContainerPolicy::load(st.pg()).await?;
    let report = crate::services::image_storage::cleanup(&st, &policy).await?;
    invalidate_inventory();
    Ok(RequestResponse::ok(report))
}

struct Removal {
    removed: i32,
    messages: Vec<String>,
}

impl Removal {
    fn blocked(message: impl Into<String>) -> Self {
        Self {
            removed: 0,
            messages: vec![message.into()],
        }
    }
}

async fn remove_one(
    st: &SharedState,
    docker: &Docker,
    requested_tag: &str,
    force_requested: bool,
    deadline: tokio::time::Instant,
) -> Removal {
    let Some(canonical_ref) = crate::controllers::edit::canonical_managed_image_tag(requested_tag)
    else {
        return Removal::blocked(format!(
            "{requested_tag} is not a canonical rsctf-managed mutable image tag"
        ));
    };
    let lock_key = crate::controllers::edit::image_build_lock_key(Some(&canonical_ref));
    let mut lock = match crate::utils::single_flight::PgAdvisoryLock::acquire_build(
        st.pg(),
        &lock_key,
    )
    .await
    {
        Ok(lock) => lock,
        Err(error) => {
            return Removal::blocked(format!(
                "{requested_tag}: image coordination failed: {error}"
            ));
        }
    };
    let scope = crate::services::container::docker_installation_scope();
    let ownership = match sqlx::query_as::<_, OwnershipRow>(OWNERSHIP_SQL)
        .bind(&scope)
        .bind(&canonical_ref)
        .fetch_optional(lock.connection_mut())
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = lock.release().await;
            return Removal::blocked(format!(
                "{requested_tag} is not owned by this rsctf installation"
            ));
        }
        Err(error) => {
            let _ = lock.release().await;
            return Removal::blocked(format!(
                "{requested_tag}: ownership ledger read failed: {error}"
            ));
        }
    };
    let claim_token = Uuid::new_v4();
    // Install the durable finalizing fence while holding only the per-image
    // coordination lock. The global challenge-reference snapshot is loaded
    // after releasing this connection; cooperating builds observe
    // `cleanup_removal_started` and cannot create a late reference meanwhile.
    let claimed = sqlx::query(CLAIM_MANUAL_REMOVAL_SQL)
        .bind(&scope)
        .bind(&canonical_ref)
        .bind(&ownership.image_id)
        .bind(claim_token)
        .bind(MANUAL_CLAIM_SECONDS)
        .bind(&lock_key)
        .execute(lock.connection_mut())
        .await;
    let released = lock.release().await;
    match (claimed, released) {
        (Ok(result), Ok(())) if result.rows_affected() == 1 => {}
        (Ok(_), Ok(())) => {
            return Removal::blocked(format!(
                "{requested_tag}: ownership changed before removal was claimed"
            ));
        }
        (Err(error), _) => {
            return Removal::blocked(format!("{requested_tag}: cleanup claim failed: {error}"));
        }
        (_, Err(error)) => {
            return Removal::blocked(format!(
                "{requested_tag}: image coordination release failed: {error}"
            ));
        }
    }

    let references = match tokio::time::timeout_at(
        deadline,
        sqlx::query_as::<_, ReferenceRow>(REFERENCES_SQL).fetch_all(st.pg()),
    )
    .await
    {
        Ok(Ok(rows)) => rows,
        Ok(Err(error)) => {
            release_manual_claim(st, &scope, &ownership, claim_token).await;
            return Removal::blocked(format!(
                "{requested_tag}: challenge reference re-read failed: {error}"
            ));
        }
        Err(_) => {
            release_manual_claim(st, &scope, &ownership, claim_token).await;
            return Removal::blocked(format!(
                "{requested_tag}: challenge reference re-read timed out"
            ));
        }
    };
    let referenced_by = reference_titles(&references, &canonical_ref, &ownership.image_id);
    if !referenced_by.is_empty() {
        release_manual_claim(st, &scope, &ownership, claim_token).await;
        return Removal::blocked(format!(
            "{requested_tag} is still referenced by {}",
            referenced_by.join(", ")
        ));
    }

    let inspected = match daemon_call(
        deadline,
        "image inspection",
        docker.inspect_image(&canonical_ref),
    )
    .await
    {
        Ok(inspected) => inspected,
        Err(error) => {
            release_manual_claim(st, &scope, &ownership, claim_token).await;
            return Removal::blocked(format!(
                "{requested_tag}: database/Docker conflict; expected {}, {error}",
                ownership.image_id
            ));
        }
    };
    if let Err(error) = validate_inspect(&inspected, &ownership, &scope) {
        release_manual_claim(st, &scope, &ownership, claim_token).await;
        return Removal::blocked(format!("{requested_tag}: {error}"));
    }
    match renew_manual_claim(st, &scope, &ownership, claim_token).await {
        Ok(true) => {}
        Ok(false) => {
            release_manual_claim(st, &scope, &ownership, claim_token).await;
            return Removal::blocked(format!(
                "{requested_tag}: cleanup ownership expired or changed before removal"
            ));
        }
        Err(error) => {
            release_manual_claim(st, &scope, &ownership, claim_token).await;
            return Removal::blocked(format!(
                "{requested_tag}: removal identity revalidation failed: {error}"
            ));
        }
    }

    let options = RemoveImageOptions {
        force: false,
        ..Default::default()
    };
    if let Err(error) = daemon_call(
        deadline,
        "image removal",
        docker.remove_image(&ownership.image_id, Some(options), None),
    )
    .await
    {
        let force_note = if force_requested {
            "; force cannot bypass rsctf ownership/reference checks"
        } else {
            ""
        };
        return Removal::blocked(format!(
            "{requested_tag}: {error}{force_note}; cleanup claim retained until expiry"
        ));
    }
    let verify_deadline = deadline.min(tokio::time::Instant::now() + DAEMON_OPERATION_BUDGET);
    match tokio::time::timeout_at(verify_deadline, docker.inspect_image(&ownership.image_id)).await
    {
        Ok(Err(error)) if docker_not_found(&error) => {}
        Ok(Err(error)) => {
            return Removal::blocked(format!(
                "{requested_tag}: removal could not be verified: {error}; cleanup claim retained until expiry"
            ));
        }
        Ok(Ok(_)) => {
            return Removal::blocked(format!(
                "{requested_tag}: Docker still resolves the immutable image; removal was not counted and the cleanup claim is retained until expiry"
            ));
        }
        Err(_) => {
            return Removal::blocked(format!(
                "{requested_tag}: removal verification timed out; cleanup claim retained until expiry"
            ));
        }
    }

    let mut messages = Vec::new();
    if force_requested {
        messages.push(
            "force=true was ignored; ownership and reference checks cannot be bypassed".to_string(),
        );
    }
    match commit_manual_removal(st, &scope, &ownership, claim_token).await {
        Ok(true) => {}
        Ok(false) => messages.push(format!(
            "{requested_tag}: image was removed, but its exact ownership changed and was preserved"
        )),
        Err(error) => messages.push(format!(
            "{requested_tag}: image was removed, but ledger cleanup failed: {error}"
        )),
    }
    Removal {
        removed: 1,
        messages,
    }
}

async fn release_manual_claim(
    st: &SharedState,
    scope: &str,
    ownership: &OwnershipRow,
    token: Uuid,
) {
    if let Err(error) = sqlx::query(
        r#"UPDATE "BuildImageOwnerships"
              SET cleanup_claim_token=NULL, cleanup_claim_until=NULL,
                  cleanup_removal_started=FALSE
            WHERE installation_scope=$1 AND canonical_ref=$2
              AND image_id=$3 AND cleanup_claim_token=$4"#,
    )
    .bind(scope)
    .bind(&ownership.canonical_ref)
    .bind(&ownership.image_id)
    .bind(token)
    .execute(st.pg())
    .await
    {
        tracing::warn!(tag=%ownership.canonical_ref, %error, "manual image claim release failed");
    }
}

async fn renew_manual_claim(
    st: &SharedState,
    scope: &str,
    ownership: &OwnershipRow,
    token: Uuid,
) -> Result<bool, String> {
    let lock_key = crate::controllers::edit::image_build_lock_key(Some(&ownership.canonical_ref));
    let mut lock = crate::utils::single_flight::PgAdvisoryLock::acquire_build(st.pg(), &lock_key)
        .await
        .map_err(|error| error.to_string())?;
    let current = sqlx::query(FINALIZE_MANUAL_CLAIM_SQL)
        .bind(scope)
        .bind(&ownership.canonical_ref)
        .bind(&ownership.image_id)
        .bind(token)
        .bind(MANUAL_CLAIM_SECONDS)
        .bind(&lock_key)
        .execute(lock.connection_mut())
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|error| error.to_string());
    let released = lock.release().await.map_err(|error| error.to_string());
    let current = current?;
    released?;
    Ok(current)
}

#[cfg(test)]
#[path = "images_claim_tests.rs"]
mod claim_tests;

async fn commit_manual_removal(
    st: &SharedState,
    scope: &str,
    ownership: &OwnershipRow,
    token: Uuid,
) -> Result<bool, String> {
    let lock_key = crate::controllers::edit::image_build_lock_key(Some(&ownership.canonical_ref));
    let mut lock = crate::utils::single_flight::PgAdvisoryLock::acquire_build(st.pg(), &lock_key)
        .await
        .map_err(|error| error.to_string())?;
    let deleted = sqlx::query(
        r#"DELETE FROM "BuildImageOwnerships"
            WHERE installation_scope=$1 AND canonical_ref=$2 AND image_id=$3
              AND cleanup_claim_token=$4"#,
    )
    .bind(scope)
    .bind(&ownership.canonical_ref)
    .bind(&ownership.image_id)
    .bind(token)
    .execute(lock.connection_mut())
    .await
    .map(|result| result.rows_affected() == 1)
    .map_err(|error| error.to_string());
    let released = lock.release().await.map_err(|error| error.to_string());
    let deleted = deleted?;
    released?;
    Ok(deleted)
}

pub async fn delete_build_image(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(query): Query<DeleteImageQuery>,
) -> RequestResponse<PruneResultModel> {
    let deadline = tokio::time::Instant::now() + ADMIN_DAEMON_BUDGET;
    let docker = match reachable_docker(deadline).await {
        Ok(docker) => docker,
        Err(error) => {
            return RequestResponse::ok(PruneResultModel {
                removed: 0,
                messages: vec![error],
            });
        }
    };
    let result = remove_one(&st, &docker, &query.tag, query.force, deadline).await;
    if result.removed > 0 {
        invalidate_inventory();
    }
    RequestResponse::ok(PruneResultModel {
        removed: result.removed,
        messages: result.messages,
    })
}

pub async fn prune_images(
    State(st): State<SharedState>,
    _admin: AdminUser,
) -> RequestResponse<PruneResultModel> {
    let deadline = tokio::time::Instant::now() + ADMIN_DAEMON_BUDGET;
    let docker = match reachable_docker(deadline).await {
        Ok(docker) => docker,
        Err(error) => {
            return RequestResponse::ok(PruneResultModel {
                removed: 0,
                messages: vec![error],
            });
        }
    };
    let scope = crate::services::container::docker_installation_scope();
    let ownerships = match sqlx::query_as::<_, OwnershipRow>(INVENTORY_OWNERSHIPS_SQL)
        .bind(&scope)
        .bind(MANUAL_PRUNE_LIMIT + 1)
        .fetch_all(st.pg())
        .await
    {
        Ok(rows) => rows,
        Err(error) => {
            return RequestResponse::ok(PruneResultModel {
                removed: 0,
                messages: vec![format!("ownership ledger read failed: {error}")],
            });
        }
    };
    let truncated = ownerships.len() > MANUAL_PRUNE_LIMIT as usize;
    let mut removed = 0;
    let mut messages = Vec::new();
    for ownership in ownerships.into_iter().take(MANUAL_PRUNE_LIMIT as usize) {
        if tokio::time::Instant::now() >= deadline {
            messages.push("manual prune reached its absolute daemon deadline".to_string());
            break;
        }
        let result = remove_one(&st, &docker, &ownership.canonical_ref, false, deadline).await;
        removed += result.removed;
        messages.extend(result.messages);
    }
    if truncated {
        messages.push(format!(
            "manual prune is bounded to {MANUAL_PRUNE_LIMIT} images; more ownership rows remain"
        ));
    }
    if removed > 0 {
        invalidate_inventory();
    }
    RequestResponse::ok(PruneResultModel { removed, messages })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const ID: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER_ID: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const SCOPE: &str = "0123456789abcdef0123456789abcdef";
    const CANONICAL: &str = "docker.io/rsctf/game/app:latest";

    fn inspect(id: &str) -> bollard::models::ImageInspect {
        bollard::models::ImageInspect {
            id: Some(id.to_string()),
            repo_tags: Some(vec!["rsctf/game/app:latest".to_string()]),
            ..Default::default()
        }
    }

    #[test]
    fn aliases_share_the_build_and_removal_lock() {
        for alias in [
            "rsctf/game/app",
            "docker.io/rsctf/game/app:latest",
            "index.docker.io/rsctf/game/app",
        ] {
            assert_eq!(
                crate::controllers::edit::canonical_managed_image_tag(alias).as_deref(),
                Some(CANONICAL)
            );
            assert_eq!(
                crate::controllers::edit::image_build_lock_key(Some(alias)),
                crate::controllers::edit::image_build_lock_key(Some(CANONICAL))
            );
        }
        assert!(crate::controllers::edit::canonical_managed_image_tag("nginx:alpine").is_none());
        assert!(crate::controllers::edit::canonical_managed_image_tag(ID).is_none());
    }

    #[test]
    fn immutable_identity_and_reserved_labels_fail_closed() {
        let ownership = OwnershipRow {
            canonical_ref: CANONICAL.to_string(),
            image_id: ID.to_string(),
            updated_at_utc: Utc::now(),
            last_used_at_utc: None,
        };
        assert!(validate_inspect(&inspect(ID), &ownership, SCOPE).is_ok());
        assert!(validate_inspect(&inspect(OTHER_ID), &ownership, SCOPE)
            .unwrap_err()
            .contains("ownership conflict"));

        let mut conflicting = inspect(ID);
        conflicting.config = Some(bollard::models::ContainerConfig {
            labels: Some(HashMap::from([
                (
                    crate::services::container::IMAGE_SCOPE_LABEL.to_string(),
                    "fedcba9876543210fedcba9876543210".to_string(),
                ),
                (
                    crate::services::container::IMAGE_REFERENCE_LABEL.to_string(),
                    CANONICAL.to_string(),
                ),
            ])),
            ..Default::default()
        });
        assert!(validate_inspect(&conflicting, &ownership, SCOPE).is_err());
    }

    #[test]
    fn active_references_are_compared_canonically() {
        let rows = vec![ReferenceRow {
            title: "active".to_string(),
            image_ref: "index.docker.io/rsctf/game/app".to_string(),
        }];
        assert_eq!(reference_titles(&rows, CANONICAL, ID), vec!["active"]);
    }

    #[test]
    fn inventory_daemon_and_database_work_have_explicit_bounds() {
        assert!(INVENTORY_OWNERSHIP_LIMIT > 0);
        assert!(INVENTORY_REFERENCE_LIMIT > INVENTORY_OWNERSHIP_LIMIT);
        assert!((1..=8).contains(&INVENTORY_INSPECT_CONCURRENCY));
        let filters = crate::services::container::managed_container_filters(SCOPE);
        let labels = filters.get("label").unwrap();
        assert_eq!(labels.len(), 2);
        assert!(labels.iter().all(|label| label.ends_with(SCOPE)));
        let source = include_str!("images.rs");
        let production = source.rsplit_once("\n#[cfg(test)]\nmod tests {").unwrap().0;
        assert!(!production.contains(".list_images("));
        assert!(production.contains("docker.list_containers"));
        assert!(production.contains("buffer_unordered(INVENTORY_INSPECT_CONCURRENCY)"));
        assert!(production.contains("run_with_timeout"));
        assert!(production.contains("OR cleanup_claim_until <= clock_timestamp()"));
        assert!(production.contains("SET cleanup_claim_until = clock_timestamp()"));
        assert!(production.contains("cleanup_removal_started = TRUE"));
        assert!(production.contains("ControlPlaneResourceLeases"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn delete_alias_waits_for_build_lock_before_out_of_lock_reference_snapshot() {
        use std::str::FromStr;

        use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("admin_image_race_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(3)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            r#"CREATE TABLE "GameChallenges" (
                 id INTEGER PRIMARY KEY,
                 game_id INTEGER NOT NULL DEFAULT 1,
                 title TEXT NOT NULL,
                 container_image TEXT,
                 ad_checker_image TEXT,
                 variant_generator_build_context_subdir TEXT,
                 variant_generator_build_status SMALLINT NOT NULL DEFAULT 0,
                 variant_generator_image TEXT,
                 variant_generator_digest TEXT
               )"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let first = crate::utils::single_flight::PgAdvisoryLock::acquire_build(
            &pool,
            &crate::controllers::edit::image_build_lock_key(Some("rsctf/game/app")),
        )
        .await
        .unwrap();
        let mut waiter = tokio::spawn({
            let pool = pool.clone();
            async move {
                crate::utils::single_flight::PgAdvisoryLock::acquire_build(
                    &pool,
                    &crate::controllers::edit::image_build_lock_key(Some(
                        "index.docker.io/rsctf/game/app:latest",
                    )),
                )
                .await
            }
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut waiter)
                .await
                .is_err()
        );
        sqlx::query(
            r#"INSERT INTO "GameChallenges" (id, title, container_image)
               VALUES (1, 'late reference', 'docker.io/rsctf/game/app:latest')"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "GameChallenges"
                 (id, game_id, title, variant_generator_build_context_subdir,
                  variant_generator_build_status, variant_generator_image,
                  variant_generator_digest)
               VALUES (2, 9, 'managed generator', 'generator', 1, $1, $1)"#,
        )
        .bind(ID)
        .execute(&pool)
        .await
        .unwrap();
        first.release().await.unwrap();

        let second = tokio::time::timeout(std::time::Duration::from_secs(2), &mut waiter)
            .await
            .expect("delete waiter must acquire after the build releases")
            .unwrap()
            .unwrap();
        second.release().await.unwrap();
        let rows = sqlx::query_as::<_, ReferenceRow>(REFERENCES_SQL)
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(
            reference_titles(&rows, CANONICAL, OTHER_ID),
            vec!["late reference"]
        );
        assert_eq!(
            reference_titles(&rows, "docker.io/rsctf/unrelated:latest", ID),
            vec!["managed generator"]
        );
        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
