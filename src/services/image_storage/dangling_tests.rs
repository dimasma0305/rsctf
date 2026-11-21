use super::*;
use crate::services::container::{IMAGE_REFERENCE_LABEL, IMAGE_SCOPE_LABEL};
use std::process::Command;

fn docker(args: &[&str]) -> String {
    let result = Command::new("docker").args(args).output().unwrap();
    assert!(
        result.status.success(),
        "Docker {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim().into()
}

#[derive(Default)]
struct Fixture {
    images: Vec<String>,
    containers: Vec<String>,
    tags: Vec<String>,
    dirs: Vec<std::path::PathBuf>,
}

impl Fixture {
    fn build(&mut self, scope: &str, slot: &str) -> String {
        let context = std::env::temp_dir().join(format!("rsctf-retention-{}", Uuid::new_v4()));
        std::fs::create_dir(&context).unwrap();
        self.dirs.push(context.clone());
        std::fs::write(
            context.join("Dockerfile"),
            "FROM scratch\nCOPY payload /payload\n",
        )
        .unwrap();
        std::fs::write(context.join("payload"), slot).unwrap();
        let id = docker(&[
            "build",
            "--network=none",
            "--quiet",
            "--label",
            &format!("{IMAGE_SCOPE_LABEL}={scope}"),
            "--label",
            &format!("{IMAGE_REFERENCE_LABEL}=docker.io/rsctf/fixtures/{scope}:build"),
            context.to_str().unwrap(),
        ]);
        assert!(
            crate::services::challenge_images::is_local_image_id(&id),
            "{id}"
        );
        self.images.push(id.clone());
        id
    }

    fn tag(&mut self, id: &str, name: String) {
        docker(&["tag", id, &name]);
        self.tags.push(name);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for dir in &self.dirs {
            let _ = std::fs::remove_dir_all(dir);
        }
        for id in &self.containers {
            let _ = Command::new("docker").args(["rm", "-fv", id]).output();
        }
        for name in self.tags.iter().chain(self.images.iter()) {
            let _ = Command::new("docker").args(["image", "rm", name]).output();
        }
    }
}

#[tokio::test]
#[ignore = "requires local Docker BuildKit and PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn pressure_cleanup_retains_pinned_revisions_and_reclaims_only_abandoned_outputs() {
    let db = crate::utils::test_database::TestDatabase::new("archive_retention").await;
    sqlx::raw_sql(crate::migrations::m0369_anonymous_image_cleanup::UP_SQL)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::raw_sql(r#"CREATE TABLE "ControlPlaneResourceLeases" (resource_key TEXT PRIMARY KEY, lease_expires_at_utc TIMESTAMPTZ NOT NULL)"#).execute(&db.pool).await.unwrap();
    sqlx::raw_sql(r#"CREATE TABLE "GameChallenges" (container_image TEXT, build_image_digest TEXT, ad_checker_image TEXT, variant_generator_image TEXT)"#).execute(&db.pool).await.unwrap();
    sqlx::raw_sql(r#"CREATE TABLE "AdProtectedLocalImages" (image_id TEXT PRIMARY KEY)"#)
        .execute(&db.pool)
        .await
        .unwrap();
    let daemon = Docker::connect_with_local_defaults().unwrap();
    let scope = Uuid::new_v4().simple().to_string();
    let mut fixture = Fixture::default();
    let starter = fixture.build(&scope, "starter");
    let checker = fixture.build(&scope, "checker");
    let active = fixture.build(&scope, "active");
    let obsolete = fixture.build(&scope, "obsolete");
    let late_pin = fixture.build(&scope, "pin committed after reference inventory");
    let original_patch_image = fixture.build(&scope, "original image after organizer rebuild");
    let published = fixture.build(&scope, "published after mark");
    let foreign = fixture.build(
        &Uuid::new_v4().simple().to_string(),
        "different installation",
    );
    let published_tag = format!("rsctf/fixture/{scope}:current");
    // Crash after marking, followed by successful publication before the retry.
    let marker = format!("rsctf-cleanup/{scope}:{}", &published[7..]);
    fixture.tag(&published, marker.clone());
    fixture.tag(&published, published_tag.clone());
    fixture
        .containers
        .push(docker(&["create", "--network=none", &active, "/unused"]));
    let mut pinned = reference();
    pinned.build_image_digest = Some(starter.clone());
    pinned.ad_checker_image = Some(checker.clone());
    let mut references = ReferenceSnapshot::new(vec![pinned]);
    sqlx::query(r#"INSERT INTO "GameChallenges" (build_image_digest) VALUES ($1)"#)
        .bind(&late_pin)
        .execute(&db.pool)
        .await
        .unwrap();
    references.live_image_ids.insert(active.clone());
    sqlx::query(r#"INSERT INTO "AdProtectedLocalImages" VALUES($1)"#)
        .bind(&original_patch_image)
        .execute(&db.pool)
        .await
        .unwrap();
    let mut report = ImageCleanupReport::default();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    // Nothing is old enough on the first sweep. Advance only the fixture cutoff.
    dangling::collect(
        &daemon,
        &db.pool,
        &scope,
        &references,
        Utc::now() - ChronoDuration::days(1),
        deadline,
        &mut report,
    )
    .await
    .unwrap();
    assert_eq!(report.images_removed, 0);
    let key = crate::services::challenge_builds::image_build_lock_key(Some(&format!(
        "rsctf/fixtures/{scope}:build"
    )));
    let lock = crate::utils::single_flight::PgAdvisoryLock::acquire_build(&db.pool, &key)
        .await
        .unwrap();
    dangling::collect(
        &daemon,
        &db.pool,
        &scope,
        &references,
        Utc::now() + ChronoDuration::days(1),
        deadline,
        &mut report,
    )
    .await
    .unwrap();
    assert_eq!(report.images_removed, 0);
    daemon.inspect_image(&obsolete).await.unwrap();
    lock.release().await.unwrap();
    sqlx::query(
        r#"INSERT INTO "ControlPlaneResourceLeases" VALUES ($1,now()+interval '60 seconds')"#,
    )
    .bind(&key)
    .execute(&db.pool)
    .await
    .unwrap();
    dangling::collect(
        &daemon,
        &db.pool,
        &scope,
        &references,
        Utc::now() + ChronoDuration::days(1),
        deadline,
        &mut report,
    )
    .await
    .unwrap();
    assert_eq!(report.images_removed, 0);
    sqlx::query(r#"DELETE FROM "ControlPlaneResourceLeases""#)
        .execute(&db.pool)
        .await
        .unwrap();
    for _ in 0..2 {
        dangling::collect(
            &daemon,
            &db.pool,
            &scope,
            &references,
            Utc::now() + ChronoDuration::days(1),
            deadline,
            &mut report,
        )
        .await
        .unwrap();
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        assert_eq!(report.images_removed, 1);
        assert!(matches!(daemon.inspect_image(&obsolete).await, Err(ref e) if docker_not_found(e)));
        assert!(matches!(daemon.inspect_image(&marker).await, Err(ref e) if docker_not_found(e)));
        for id in [
            &starter,
            &checker,
            &active,
            &foreign,
            &published,
            &late_pin,
            &original_patch_image,
        ] {
            daemon.inspect_image(id).await.unwrap();
        }
        assert_eq!(
            docker(&["image", "inspect", "-f", "{{.Id}}", &published_tag]),
            published
        );
    }
    sqlx::query(r#"DELETE FROM "AdProtectedLocalImages""#)
        .execute(&db.pool)
        .await
        .unwrap();
    dangling::collect(
        &daemon,
        &db.pool,
        &scope,
        &references,
        Utc::now() + ChronoDuration::days(1),
        deadline,
        &mut report,
    )
    .await
    .unwrap();
    assert!(
        matches!(daemon.inspect_image(&original_patch_image).await, Err(ref e) if docker_not_found(e))
    );
    db.close().await;
}
