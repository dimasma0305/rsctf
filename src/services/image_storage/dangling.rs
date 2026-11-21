//! Retire old anonymous archive outputs without following parent layers or removing
//! a name concurrently published by a builder. Docker's bulk prune cannot exclude IDs.
use super::*;
use crate::services::container::{IMAGE_REFERENCE_LABEL, IMAGE_SCOPE_LABEL};
use bollard::image::{ListImagesOptions, TagImageOptions};
use bollard::models::ImageSummary;

fn cleanup_tag(scope: &str, id: &str) -> String {
    format!(
        "docker.io/rsctf-cleanup/{scope}:{}",
        id.trim_start_matches("sha256:")
    )
}

fn eligible(
    image: &ImageSummary,
    scope: &str,
    references: &ReferenceSnapshot,
    cutoff: DateTime<Utc>,
) -> bool {
    if !crate::services::challenge_images::is_local_image_id(&image.id)
        || image.created >= cutoff.timestamp()
        || references.by_image_id.contains_key(&image.id)
        || image.labels.get(IMAGE_SCOPE_LABEL).map(String::as_str) != Some(scope)
        || !image.labels.get(IMAGE_REFERENCE_LABEL).is_some_and(|name| {
            crate::services::challenge_builds::canonical_managed_image_tag(name).as_ref()
                == Some(name)
        })
    {
        return false;
    }
    if image.repo_tags.is_empty() {
        return image.repo_digests.is_empty();
    }
    marker_can_be_removed(image, &cleanup_tag(scope, &image.id))
}

fn marker_can_be_removed(image: &ImageSummary, marker: &str) -> bool {
    use crate::services::challenge_builds::canonical_image_reference;
    let tags: Vec<_> = image
        .repo_tags
        .iter()
        .map(|tag| canonical_image_reference(Some(tag)))
        .collect();
    if !tags.iter().any(|tag| tag == marker) {
        return false;
    }
    // Docker's containerd store synthesizes a RepoDigest for a local tag,
    // including our temporary marker. Only untag the marker when each digest
    // is explained by a present tag on this exact manifest. Keep digest-only
    // references and manifests whose identity the daemon cannot establish.
    let repositories: std::collections::HashSet<_> = tags
        .iter()
        .filter_map(|tag| tag.rsplit_once(':').map(|(repository, _)| repository))
        .collect();
    image.repo_digests.iter().all(|digest| {
        canonical_image_reference(Some(digest))
            .split_once('@')
            .is_some_and(|(repository, id)| id == image.id && repositories.contains(repository))
    })
}

pub(super) async fn collect(
    docker: &Docker,
    pool: &sqlx::PgPool,
    scope: &str,
    references: &ReferenceSnapshot,
    cutoff: DateTime<Utc>,
    deadline: tokio::time::Instant,
    report: &mut ImageCleanupReport,
) -> AppResult<()> {
    // A process that dies after Docker removes its last marker may never see
    // that image again. Reap expired claims even when inventory is empty.
    sqlx::query(r#"DELETE FROM "AnonymousImageCleanupClaims" WHERE installation_scope=$1 AND canonical_ref IN (
        SELECT canonical_ref FROM "AnonymousImageCleanupClaims" WHERE installation_scope=$1 AND expires_at<=clock_timestamp()
        ORDER BY expires_at LIMIT $2 FOR UPDATE SKIP LOCKED)"#)
        .bind(scope).bind(CLEANUP_BATCH_SIZE).execute(pool).await.map_err(|e| AppError::internal(e.to_string()))?;
    let mut images = docker_call(
        DockerOperationClass::Read,
        deadline,
        "managed image inventory",
        docker.list_images(Some(ListImagesOptions {
            filters: HashMap::from([(
                "label".to_owned(),
                vec![format!("{IMAGE_SCOPE_LABEL}={scope}")],
            )]),
            ..Default::default()
        })),
    )
    .await?;
    images.retain(|image| {
        eligible(image, scope, references, cutoff) && !references.used_by_container(&image.id)
    });
    images.sort_unstable_by(|a, b| (a.created, &a.id).cmp(&(b.created, &b.id)));
    for image in images.into_iter().take(CLEANUP_BATCH_SIZE as usize) {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        let marker = cleanup_tag(scope, &image.id);
        // A content-derived marker survives an interrupted pass. Removing only
        // this name lets Docker retain any tag added after inventory. Never
        // remove by ID here: even force=false can remove a newly assigned tag.
        let reference = &image.labels[IMAGE_REFERENCE_LABEL];
        let Some(token) = claim(pool, scope, reference, &image.id).await? else {
            continue;
        };
        let result = remove_marker(docker, &image, &marker, deadline).await;
        // Keep the expiring fence if the daemon's final response was lost.
        if result.is_ok() {
            sqlx::query(
                r#"DELETE FROM "AnonymousImageCleanupClaims"
                WHERE installation_scope=$1 AND canonical_ref=$2 AND claim_token=$3"#,
            )
            .bind(scope)
            .bind(reference)
            .bind(token)
            .execute(pool)
            .await
            .map_err(|e| AppError::internal(e.to_string()))?;
        }
        match result {
            Ok(true) => {
                report.images_removed += 1;
                report.image_bytes_evicted = report
                    .image_bytes_evicted
                    .saturating_add(image.size.max(0) as u64);
            }
            Ok(false) => {}
            Err(error) => report.messages.push(format!(
                "anonymous image {} was retained: {error}",
                image.id
            )),
        }
    }
    Ok(())
}

async fn claim(
    pool: &sqlx::PgPool,
    scope: &str,
    reference: &str,
    image_id: &str,
) -> AppResult<Option<Uuid>> {
    let key = crate::services::challenge_builds::image_build_lock_key(Some(reference));
    let Some(mut lock) =
        crate::utils::single_flight::PgAdvisoryLock::try_acquire(pool, &key).await?
    else {
        return Ok(None);
    };
    let token = Uuid::new_v4();
    let acquired = sqlx::query(
        r#"INSERT INTO "AnonymousImageCleanupClaims" (installation_scope, canonical_ref, claim_token, expires_at)
           SELECT $1, $2, $3, clock_timestamp() + make_interval(secs => $4)
           WHERE NOT EXISTS (
               SELECT 1 FROM "ControlPlaneResourceLeases"
                WHERE resource_key = $5 AND lease_expires_at_utc > clock_timestamp()
           ) AND NOT EXISTS (SELECT 1 FROM "AdProtectedLocalImages" WHERE image_id=lower($6))
           AND NOT EXISTS (
               SELECT 1 FROM "GameChallenges" WHERE $6 IN (
                   lower(btrim(container_image)), lower(btrim(build_image_digest)),
                   lower(btrim(ad_checker_image)), lower(btrim(variant_generator_image)))
           )
           ON CONFLICT (installation_scope, canonical_ref) DO UPDATE
               SET claim_token = EXCLUDED.claim_token, expires_at = EXCLUDED.expires_at
             WHERE "AnonymousImageCleanupClaims".expires_at <= clock_timestamp()"#,
    )
    .bind(scope)
    .bind(reference)
    .bind(token)
    .bind(CLAIM_LEASE_SECONDS)
    .bind(key)
    .bind(image_id)
    .execute(&mut **lock.transaction_mut())
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;
    lock.release().await?;
    Ok((acquired.rows_affected() == 1).then_some(token))
}

async fn remove_marker(
    docker: &Docker,
    image: &ImageSummary,
    marker: &str,
    deadline: tokio::time::Instant,
) -> AppResult<bool> {
    match admitted_docker_call(
        DockerOperationClass::Read,
        deadline,
        "inspect cleanup tag",
        docker.inspect_image(marker),
    )
    .await?
    {
        Ok(inspected) if inspected.id.as_deref() == Some(&image.id) => {}
        Ok(_) => return Err(AppError::conflict("Cleanup tag belongs to another image")),
        Err(error) if docker_not_found(&error) => {
            docker_call(
                DockerOperationClass::Lifecycle,
                deadline,
                "mark anonymous image",
                docker.tag_image(
                    &image.id,
                    Some(TagImageOptions {
                        repo: marker,
                        tag: "",
                    }),
                ),
            )
            .await?;
        }
        Err(error) => {
            return Err(AppError::unavailable(format!(
                "Could not inspect cleanup tag: {error}"
            )))
        }
    }
    let removed = docker_call(
        DockerOperationClass::Lifecycle,
        deadline,
        "remove anonymous image",
        docker.remove_image(
            marker,
            Some(RemoveImageOptions {
                force: false,
                noprune: true,
            }),
            None,
        ),
    )
    .await?;
    Ok(removed
        .iter()
        .any(|item| item.deleted.as_ref() == Some(&image.id)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_cleanup_preserves_independent_digest_only_references() {
        let id = format!("sha256:{}", "a".repeat(64));
        let marker = cleanup_tag("fixture", &id);
        let mut image = ImageSummary {
            id: id.clone(),
            repo_tags: vec![
                marker.clone(),
                "example.test:5000/team/service:ready".into(),
            ],
            repo_digests: vec![format!("example.test:5000/team/service@{id}")],
            ..Default::default()
        };
        assert!(marker_can_be_removed(&image, &marker));
        image.repo_tags.pop();
        assert!(!marker_can_be_removed(&image, &marker));
        image.repo_digests = vec![format!("rsctf-cleanup/fixture@{id}")];
        assert!(marker_can_be_removed(&image, &marker));
        image.repo_digests = vec![format!("rsctf-cleanup/fixture@sha256:{}", "b".repeat(64))];
        assert!(!marker_can_be_removed(&image, &marker));
    }
}
