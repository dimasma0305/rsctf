//! Fail-closed inventory and deletion for locally-built challenge images.

use super::*;

use bollard::image::{ListImagesOptions, RemoveImageOptions};
use bollard::models::ImageInspect;
use bollard::Docker;

use crate::services::container::{IMAGE_REFERENCE_LABEL, IMAGE_SCOPE_LABEL};

const IMAGE_REFERENCES_SQL: &str = r#"
    SELECT title, container_image AS image_ref
      FROM "GameChallenges"
     WHERE container_image IS NOT NULL
       AND BTRIM(container_image) <> ''
    UNION ALL
    SELECT title, ad_checker_image AS image_ref
      FROM "GameChallenges"
     WHERE ad_checker_image IS NOT NULL
       AND BTRIM(ad_checker_image) <> ''"#;

#[derive(Debug, sqlx::FromRow)]
struct ImageReferenceRow {
    title: String,
    image_ref: String,
}

#[derive(Debug, PartialEq, Eq)]
struct ManagedImageRemoval {
    image_id: String,
}

struct RemovalResult {
    removed: i32,
    messages: Vec<String>,
}

impl RemovalResult {
    fn refused(message: impl Into<String>) -> Self {
        Self {
            removed: 0,
            messages: vec![message.into()],
        }
    }
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

fn managed_image_tags(
    repo_tags: &[String],
    labels: &std::collections::HashMap<String, String>,
    installation_scope: &str,
) -> Vec<String> {
    if labels.get(IMAGE_SCOPE_LABEL).map(String::as_str) != Some(installation_scope) {
        return Vec::new();
    }
    let Some(labeled_reference) = labels.get(IMAGE_REFERENCE_LABEL) else {
        return Vec::new();
    };
    if crate::controllers::edit::canonical_managed_image_tag(labeled_reference).as_deref()
        != Some(labeled_reference.as_str())
    {
        return Vec::new();
    }

    repo_tags
        .iter()
        .filter(|tag| {
            crate::controllers::edit::canonical_managed_image_tag(tag).as_deref()
                == Some(labeled_reference.as_str())
        })
        .cloned()
        .collect()
}

fn reference_titles_for(rows: &[ImageReferenceRow], canonical_reference: &str) -> Vec<String> {
    let mut titles = rows
        .iter()
        .filter(|row| {
            crate::controllers::edit::canonical_image_reference(Some(&row.image_ref))
                == canonical_reference
        })
        .map(|row| row.title.clone())
        .collect::<Vec<_>>();
    titles.sort_unstable();
    titles.dedup();
    titles
}

async fn load_references(st: &SharedState) -> Result<Vec<ImageReferenceRow>, String> {
    sqlx::query_as::<_, ImageReferenceRow>(IMAGE_REFERENCES_SQL)
        .fetch_all(st.pg())
        .await
        .map_err(|error| format!("challenge image reference query failed: {error}"))
}

/// Only images carrying this installation's exact reserved scope/reference
/// pair enter the inventory. Pulls, pre-pinned images, legacy unlabeled builds,
/// foreign scopes, and mismatched aliases remain outside the deletion boundary.
async fn collect_build_images(
    st: &SharedState,
    docker: &Docker,
) -> Result<Vec<BuildImageModel>, String> {
    let images = docker
        .list_images(Some(ListImagesOptions::<String> {
            all: false,
            ..Default::default()
        }))
        .await
        .map_err(|error| format!("Docker image inventory failed: {error}"))?;
    let references = load_references(st).await?;
    let installation_scope = crate::services::container::docker_installation_scope();

    let mut out = Vec::new();
    for image in images {
        let tags = managed_image_tags(&image.repo_tags, &image.labels, &installation_scope);
        if tags.is_empty() {
            continue;
        }
        let mut referenced_by = tags
            .iter()
            .flat_map(|tag| {
                let canonical =
                    crate::controllers::edit::canonical_image_reference(Some(tag.as_str()));
                reference_titles_for(&references, &canonical)
            })
            .collect::<Vec<_>>();
        referenced_by.sort_unstable();
        referenced_by.dedup();
        let is_checker = tags.iter().any(|tag| tag.contains("checker"));
        out.push(BuildImageModel {
            id: image.id,
            referenced: !referenced_by.is_empty(),
            referenced_by,
            is_checker,
            size_bytes: image.size,
            created_utc: DateTime::<Utc>::from_timestamp(image.created, 0),
            tags,
        });
    }
    Ok(out)
}

fn validate_managed_image_for_removal(
    inspected: &ImageInspect,
    installation_scope: &str,
    canonical_reference: &str,
) -> Result<ManagedImageRemoval, String> {
    let labels = inspected
        .config
        .as_ref()
        .and_then(|config| config.labels.as_ref())
        .ok_or_else(|| "image has no rsctf ownership labels".to_string())?;
    if labels.get(IMAGE_SCOPE_LABEL).map(String::as_str) != Some(installation_scope) {
        return Err("image belongs to another installation or has no scope label".to_string());
    }
    if labels.get(IMAGE_REFERENCE_LABEL).map(String::as_str) != Some(canonical_reference) {
        return Err("image reference label does not match the requested canonical tag".to_string());
    }

    let tags = inspected.repo_tags.as_deref().unwrap_or_default();
    if tags.len() != 1
        || crate::controllers::edit::canonical_managed_image_tag(&tags[0]).as_deref()
            != Some(canonical_reference)
    {
        return Err("image has another tag alias; refusing whole-image removal".to_string());
    }

    let image_id = inspected
        .id
        .as_deref()
        .filter(|image_id| crate::services::challenge_images::is_local_image_id(image_id))
        .ok_or_else(|| "Docker did not return a valid immutable image id".to_string())?;
    Ok(ManagedImageRemoval {
        image_id: image_id.to_string(),
    })
}

async fn inspect_managed_image_for_removal(
    docker: &Docker,
    canonical_reference: &str,
    installation_scope: &str,
) -> Result<ManagedImageRemoval, String> {
    let inspected = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        docker.inspect_image(canonical_reference),
    )
    .await
    .map_err(|_| "Docker image inspection timed out".to_string())?
    .map_err(|error| format!("Docker image inspection failed: {error}"))?;
    validate_managed_image_for_removal(&inspected, installation_scope, canonical_reference)
}

/// Serialize against builds of the same mutable tag, then re-read live
/// challenge references and re-inspect labels, aliases, and immutable identity.
/// Docker receives only the captured id and is never allowed to force-remove it.
async fn remove_one(
    st: &SharedState,
    docker: &Docker,
    requested_tag: &str,
    force_requested: bool,
) -> RemovalResult {
    let Some(canonical_reference) =
        crate::controllers::edit::canonical_managed_image_tag(requested_tag)
    else {
        return RemovalResult::refused(format!(
            "{requested_tag} is not an rsctf-managed mutable image tag"
        ));
    };
    let lock_key = crate::controllers::edit::image_build_lock_key(Some(&canonical_reference));
    let image_lock = match crate::utils::single_flight::PgAdvisoryLock::acquire_build(
        st.pg(),
        &lock_key,
    )
    .await
    {
        Ok(lock) => lock,
        Err(error) => {
            return RemovalResult::refused(format!(
                "{requested_tag}: image coordination failed: {error}"
            ));
        }
    };

    let operation = async {
        let references = load_references(st).await?;
        let referenced_by = reference_titles_for(&references, &canonical_reference);
        if !referenced_by.is_empty() {
            return Err(format!(
                "image is still referenced by {}",
                referenced_by.join(", ")
            ));
        }
        let managed = inspect_managed_image_for_removal(
            docker,
            &canonical_reference,
            &crate::services::container::docker_installation_scope(),
        )
        .await?;
        docker
            .remove_image(
                &managed.image_id,
                Some(RemoveImageOptions {
                    force: false,
                    ..Default::default()
                }),
                None,
            )
            .await
            .map_err(|error| format!("Docker refused immutable image removal: {error}"))?;
        Ok::<(), String>(())
    }
    .await;
    let released = image_lock
        .release()
        .await
        .map_err(|error| format!("image coordination release failed: {error}"));

    let mut messages = Vec::new();
    if force_requested {
        messages.push(
            "force=true was ignored; ownership, aliases, references, and Docker conflicts cannot be bypassed"
                .to_string(),
        );
    }
    match operation {
        Ok(()) => {
            if let Err(error) = released {
                messages.push(format!("{requested_tag}: {error}"));
            }
            RemovalResult {
                removed: 1,
                messages,
            }
        }
        Err(error) => {
            messages.push(format!("{requested_tag}: {error}"));
            if let Err(release_error) = released {
                messages.push(format!("{requested_tag}: {release_error}"));
            }
            RemovalResult {
                removed: 0,
                messages,
            }
        }
    }
}

/// `GET /api/admin/builds/images` — locally-built images owned by this rsctf
/// installation. Docker/DB failures preserve the established `200 + []` API.
pub async fn build_images(
    State(st): State<SharedState>,
    _admin: AdminUser,
) -> RequestResponse<Vec<BuildImageModel>> {
    let images = match reachable_docker().await {
        Ok(docker) => collect_build_images(&st, &docker).await.unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    RequestResponse::ok(images)
}

/// `DELETE /api/admin/builds/images?tag=&force=` — remove one currently
/// unreferenced, exclusively-tagged image owned by this installation.
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

/// `POST /api/admin/builds/pruneimages` — remove every inventory image that is
/// not referenced, revalidating each candidate under its per-tag build lock.
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
    let images = match collect_build_images(&st, &docker).await {
        Ok(images) => images,
        Err(error) => {
            return RequestResponse::ok(PruneResultModel {
                removed: 0,
                messages: vec![error],
            });
        }
    };

    let mut removed = 0;
    let mut messages = Vec::new();
    for image in images.into_iter().filter(|image| !image.referenced) {
        for tag in image.tags {
            let result = remove_one(&st, &docker, &tag, false).await;
            removed += result.removed;
            messages.extend(result.messages);
        }
    }
    RequestResponse::ok(PruneResultModel { removed, messages })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCOPE: &str = "0123456789abcdef0123456789abcdef";
    const CANONICAL: &str = "docker.io/rsctf/game/app:latest";
    const IMAGE_ID: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn owned_labels() -> std::collections::HashMap<String, String> {
        std::collections::HashMap::from([
            (IMAGE_SCOPE_LABEL.to_string(), SCOPE.to_string()),
            (IMAGE_REFERENCE_LABEL.to_string(), CANONICAL.to_string()),
        ])
    }

    fn owned_inspect(tags: Vec<&str>) -> ImageInspect {
        ImageInspect {
            id: Some(IMAGE_ID.to_string()),
            repo_tags: Some(tags.into_iter().map(str::to_string).collect()),
            config: Some(bollard::models::ContainerConfig {
                labels: Some(owned_labels()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn aliases_share_one_canonical_tag_and_build_lock() {
        let aliases = [
            "rsctf/game/app",
            CANONICAL,
            "index.docker.io/rsctf/game/app",
        ];
        for alias in aliases {
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
        assert!(crate::controllers::edit::canonical_managed_image_tag(IMAGE_ID).is_none());
    }

    #[test]
    fn inventory_requires_scope_and_the_exact_labeled_tag() {
        let tags = vec![
            "rsctf/game/app:latest".to_string(),
            "rsctf/game/other:latest".to_string(),
        ];
        let labels = owned_labels();
        assert_eq!(
            managed_image_tags(&tags, &labels, SCOPE),
            vec!["rsctf/game/app:latest"]
        );
        assert!(managed_image_tags(&tags, &labels, "foreign-scope").is_empty());
        assert!(managed_image_tags(&tags, &std::collections::HashMap::new(), SCOPE).is_empty());

        let mut wrong_reference = labels;
        wrong_reference.insert(
            IMAGE_REFERENCE_LABEL.to_string(),
            "docker.io/rsctf/game/other:latest".to_string(),
        );
        assert_eq!(
            managed_image_tags(&tags, &wrong_reference, SCOPE),
            vec!["rsctf/game/other:latest"]
        );
    }

    #[test]
    fn destructive_validation_rejects_aliases_and_foreign_scope() {
        let owned = owned_inspect(vec!["rsctf/game/app:latest"]);
        assert_eq!(
            validate_managed_image_for_removal(&owned, SCOPE, CANONICAL).unwrap(),
            ManagedImageRemoval {
                image_id: IMAGE_ID.to_string(),
            }
        );

        let aliased = owned_inspect(vec![
            "rsctf/game/app:latest",
            "example.com/shared/alias:latest",
        ]);
        assert!(
            validate_managed_image_for_removal(&aliased, SCOPE, CANONICAL)
                .unwrap_err()
                .contains("alias")
        );
        assert!(
            validate_managed_image_for_removal(&owned, "foreign-scope", CANONICAL)
                .unwrap_err()
                .contains("another installation")
        );

        let mut unlabeled = owned.clone();
        unlabeled.config = None;
        assert!(validate_managed_image_for_removal(&unlabeled, SCOPE, CANONICAL).is_err());
    }

    #[test]
    fn active_references_are_compared_canonically() {
        let rows = vec![ImageReferenceRow {
            title: "active".to_string(),
            image_ref: "index.docker.io/rsctf/game/app".to_string(),
        }];
        assert_eq!(reference_titles_for(&rows, CANONICAL), vec!["active"]);
    }
}
