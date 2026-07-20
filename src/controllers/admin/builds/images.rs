//! Installation-scoped Docker image inventory and garbage collection.

use super::{BuildImageModel, DeleteImageQuery, PruneResultModel};
use axum::extract::{Query, State};
use bollard::errors::Error as DockerError;
use bollard::image::{ListImagesOptions, RemoveImageOptions};
use bollard::Docker;
use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, HashMap};

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::AdminUser;
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

const OWNERSHIPS_SQL: &str = r#"SELECT canonical_ref, image_id
 FROM "BuildImageOwnerships" WHERE installation_scope=$1 ORDER BY canonical_ref"#;
const OWNERSHIP_SQL: &str = r#"SELECT canonical_ref, image_id
 FROM "BuildImageOwnerships" WHERE installation_scope=$1 AND canonical_ref=$2"#;
const REFERENCES_SQL: &str = r#"
 SELECT title, container_image AS image_ref FROM "GameChallenges"
 WHERE container_image IS NOT NULL AND BTRIM(container_image)<>''
 UNION ALL
 SELECT title, ad_checker_image AS image_ref FROM "GameChallenges"
 WHERE ad_checker_image IS NOT NULL AND BTRIM(ad_checker_image)<>''"#;

#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
struct OwnershipRow {
    canonical_ref: String,
    image_id: String,
}

#[derive(Debug, sqlx::FromRow)]
struct ReferenceRow {
    title: String,
    image_ref: String,
}

async fn reachable_docker() -> Result<Docker, String> {
    let docker = Docker::connect_with_local_defaults()
        .map_err(|error| format!("Docker connection failed: {error}"))?;
    match tokio::time::timeout(std::time::Duration::from_secs(2), docker.ping()).await {
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

fn reference_titles(rows: &[ReferenceRow], canonical_ref: &str) -> Vec<String> {
    let mut titles = rows
        .iter()
        .filter(|row| {
            crate::controllers::edit::canonical_image_reference(Some(&row.image_ref))
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

async fn inventory(st: &SharedState, docker: &Docker) -> AppResult<Vec<BuildImageModel>> {
    let scope = crate::services::container::docker_installation_scope();
    let ownerships = sqlx::query_as::<_, OwnershipRow>(OWNERSHIPS_SQL)
        .bind(&scope)
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let references = sqlx::query_as::<_, ReferenceRow>(REFERENCES_SQL)
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let summaries = docker
        .list_images(Some(ListImagesOptions::<String> {
            all: false,
            ..Default::default()
        }))
        .await
        .map_err(|error| AppError::unavailable(format!("Docker image inventory failed: {error}")))?
        .into_iter()
        .map(|summary| (summary.id.clone(), summary))
        .collect::<HashMap<_, _>>();

    let mut grouped = BTreeMap::<String, BuildImageModel>::new();
    for ownership in ownerships {
        let inspected = match docker.inspect_image(&ownership.canonical_ref).await {
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
        let Some(summary) = summaries.get(&ownership.image_id) else {
            tracing::warn!(tag=%ownership.canonical_ref, expected_image_id=%ownership.image_id,
                "owned build image is missing from Docker list output");
            continue;
        };
        let referenced_by = reference_titles(&references, &ownership.canonical_ref);
        let entry = grouped
            .entry(ownership.image_id.clone())
            .or_insert_with(|| BuildImageModel {
                id: ownership.image_id.clone(),
                tags: Vec::new(),
                size_bytes: summary.size,
                created_utc: DateTime::<Utc>::from_timestamp(summary.created, 0),
                referenced: false,
                referenced_by: Vec::new(),
                is_checker: false,
            });
        entry.tags.push(tag.clone());
        entry.referenced_by.extend(referenced_by);
        entry.referenced = !entry.referenced_by.is_empty();
        entry.is_checker |= tag.contains("checker");
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
    let docker = reachable_docker().await.map_err(AppError::unavailable)?;
    Ok(RequestResponse::ok(inventory(&st, &docker).await?))
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
    let inspected = match docker.inspect_image(&canonical_ref).await {
        Ok(inspected) => inspected,
        Err(error) => {
            let _ = lock.release().await;
            return Removal::blocked(format!(
                "{requested_tag}: database/Docker conflict; expected {}, inspect failed: {error}",
                ownership.image_id
            ));
        }
    };
    let tag = match validate_inspect(&inspected, &ownership, &scope) {
        Ok(tag) => tag,
        Err(error) => {
            let _ = lock.release().await;
            return Removal::blocked(format!("{requested_tag}: {error}"));
        }
    };
    let references = match sqlx::query_as::<_, ReferenceRow>(REFERENCES_SQL)
        .fetch_all(lock.connection_mut())
        .await
    {
        Ok(rows) => rows,
        Err(error) => {
            let _ = lock.release().await;
            return Removal::blocked(format!(
                "{requested_tag}: challenge reference re-read failed: {error}"
            ));
        }
    };
    let referenced_by = reference_titles(&references, &canonical_ref);
    if !referenced_by.is_empty() {
        let _ = lock.release().await;
        return Removal::blocked(format!(
            "{requested_tag} is still referenced by {}",
            referenced_by.join(", ")
        ));
    }

    let options = RemoveImageOptions {
        force: false,
        ..Default::default()
    };
    if let Err(error) = docker.remove_image(&tag, Some(options), None).await {
        let _ = lock.release().await;
        let force_note = if force_requested {
            "; force cannot bypass rsctf ownership/reference checks"
        } else {
            ""
        };
        return Removal::blocked(format!(
            "{requested_tag}: Docker removal failed: {error}{force_note}"
        ));
    }
    match docker.inspect_image(&canonical_ref).await {
        Err(error) if docker_not_found(&error) => {}
        Err(error) => {
            let _ = lock.release().await;
            return Removal::blocked(format!(
                "{requested_tag}: removal could not be verified: {error}"
            ));
        }
        Ok(current) => {
            let current_id = crate::services::challenge_images::inspected_local_image_id(&current)
                .unwrap_or("<invalid>");
            let _ = lock.release().await;
            return Removal::blocked(format!(
                "{requested_tag}: Docker still resolves the tag to {current_id}; removal was not counted"
            ));
        }
    }

    let mut messages = Vec::new();
    if force_requested {
        messages.push(
            "force=true was ignored; ownership and reference checks cannot be bypassed".to_string(),
        );
    }
    let deletion = sqlx::query(
        r#"DELETE FROM "BuildImageOwnerships"
        WHERE installation_scope=$1 AND canonical_ref=$2 AND image_id=$3"#,
    )
    .bind(&scope)
    .bind(&canonical_ref)
    .bind(&ownership.image_id)
    .execute(lock.connection_mut())
    .await;
    match deletion {
        Ok(result) if result.rows_affected() == 1 => {}
        Ok(_) => messages.push(format!(
            "{requested_tag}: image was removed, but its ownership row changed"
        )),
        Err(error) => messages.push(format!(
            "{requested_tag}: image was removed, but ledger cleanup failed: {error}"
        )),
    }
    if let Err(error) = lock.release().await {
        messages.push(format!(
            "{requested_tag}: image coordination release failed: {error}"
        ));
    }
    Removal {
        removed: 1,
        messages,
    }
}

pub async fn delete_build_image(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(query): Query<DeleteImageQuery>,
) -> RequestResponse<PruneResultModel> {
    let docker = match reachable_docker().await {
        Ok(docker) => docker,
        Err(error) => {
            return RequestResponse::ok(PruneResultModel {
                removed: 0,
                messages: vec![error],
            });
        }
    };
    let result = remove_one(&st, &docker, &query.tag, query.force).await;
    RequestResponse::ok(PruneResultModel {
        removed: result.removed,
        messages: result.messages,
    })
}

pub async fn prune_images(
    State(st): State<SharedState>,
    _admin: AdminUser,
) -> RequestResponse<PruneResultModel> {
    let docker = match reachable_docker().await {
        Ok(docker) => docker,
        Err(error) => {
            return RequestResponse::ok(PruneResultModel {
                removed: 0,
                messages: vec![error],
            });
        }
    };
    let scope = crate::services::container::docker_installation_scope();
    let ownerships = match sqlx::query_as::<_, OwnershipRow>(OWNERSHIPS_SQL)
        .bind(scope)
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
    let mut removed = 0;
    let mut messages = Vec::new();
    for ownership in ownerships {
        let result = remove_one(&st, &docker, &ownership.canonical_ref, false).await;
        removed += result.removed;
        messages.extend(result.messages);
    }
    RequestResponse::ok(PruneResultModel { removed, messages })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(reference_titles(&rows, CANONICAL), vec!["active"]);
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn delete_alias_waits_for_build_lock_then_rereads_references() {
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
                 title TEXT NOT NULL,
                 container_image TEXT,
                 ad_checker_image TEXT
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
        first.release().await.unwrap();

        let mut second = tokio::time::timeout(std::time::Duration::from_secs(2), &mut waiter)
            .await
            .expect("delete waiter must acquire after the build releases")
            .unwrap()
            .unwrap();
        let rows = sqlx::query_as::<_, ReferenceRow>(REFERENCES_SQL)
            .fetch_all(second.connection_mut())
            .await
            .unwrap();
        assert_eq!(reference_titles(&rows, CANONICAL), vec!["late reference"]);
        second.release().await.unwrap();

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
