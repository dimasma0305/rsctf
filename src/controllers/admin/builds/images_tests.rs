use super::*;
use std::collections::HashMap;

const ID: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OTHER_ID: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
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
    const { assert!(INVENTORY_OWNERSHIP_LIMIT > 0) };
    const { assert!(INVENTORY_REFERENCE_LIMIT > INVENTORY_OWNERSHIP_LIMIT) };
    assert!((1..=8).contains(&INVENTORY_INSPECT_CONCURRENCY));
    let filters = crate::services::container::managed_container_filters(SCOPE);
    let labels = filters.get("label").unwrap();
    assert_eq!(labels.len(), 2);
    assert!(labels.iter().all(|label| label.ends_with(SCOPE)));
    let production = include_str!("images.rs");
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
    sqlx::raw_sql(
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
           );
           CREATE TABLE "AdProtectedLocalImages" (challenge_id INTEGER, image_id TEXT)"#,
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
    sqlx::query(r#"INSERT INTO "AdProtectedLocalImages" VALUES (2, $1)"#)
        .bind(OTHER_ID)
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
        vec!["late reference", "managed generator"]
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
