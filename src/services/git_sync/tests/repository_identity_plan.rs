use sqlx::postgres::PgPoolOptions;

use super::super::repository::{legacy_manifest_lookup_parameters, REPOSITORY_MANIFEST_LOOKUP_SQL};

#[test]
fn legacy_suffix_pattern_escapes_every_like_metacharacter() {
    let (relative, pattern) =
        legacy_manifest_lookup_parameters(Some(7), "binding/7/event/under_%!/challenge.yaml");
    assert_eq!(relative.as_deref(), Some("event/under_%!/challenge.yaml"));
    let pattern = pattern.expect("binding-scoped paths retain legacy lookup compatibility");
    assert!(pattern.ends_with('%'));
    assert!(pattern.contains("!!"));
    assert!(pattern.contains("!%"));
    assert!(pattern.contains("!_"));
}

/// Query-plan evidence for the game-lock hot path. This opt-in PostgreSQL test
/// proves the migration is idempotent, all historical identity forms resolve,
/// ambiguity materializes at most two IDs, and the lookup uses the reverse-path
/// index rather than scanning a game's complete challenge catalog.
#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn repository_identity_lookup_is_indexed_and_result_bounded() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(crate::migrations::test_pg_connect_options(&database_url))
        .await
        .unwrap();
    let schema = format!("rsctf_m0332_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(
            crate::migrations::test_pg_connect_options(&database_url)
                .options([("search_path", schema.as_str())]),
        )
        .await
        .unwrap();

    sqlx::raw_sql(
        r#"CREATE TABLE "GameChallenges" (
               id INTEGER PRIMARY KEY,
               game_id INTEGER NOT NULL,
               source_yaml_path TEXT NULL
           );"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(crate::migrations::REPOSITORY_MANIFEST_LOOKUP_INDEX_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(crate::migrations::REPOSITORY_MANIFEST_LOOKUP_INDEX_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "GameChallenges" (id, game_id, source_yaml_path)
           SELECT value, 7, 'binding/7/noise/' || value || '/challenge.yaml'
             FROM generate_series(1, 10000) value"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    for (id, path) in [
        (20_001, "binding/7/event/web/challenge.yaml"),
        (20_002, "event/web/challenge.yaml"),
        (20_003, "/srv/repos/7/event/web/challenge.yaml"),
        (20_004, r"C:\storage\repos\7\event\web\challenge.yaml"),
    ] {
        sqlx::query(
            r#"INSERT INTO "GameChallenges" (id, game_id, source_yaml_path)
               VALUES ($1, 7, $2)"#,
        )
        .bind(id)
        .bind(path)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query(r#"ANALYZE "GameChallenges""#)
        .execute(&pool)
        .await
        .unwrap();

    let canonical = "binding/7/event/web/challenge.yaml";
    let (relative, suffix_pattern) = legacy_manifest_lookup_parameters(Some(7), canonical);
    let mut connection = pool.acquire().await.unwrap();
    sqlx::query("SET enable_seqscan = off")
        .execute(&mut *connection)
        .await
        .unwrap();
    let explain = format!("EXPLAIN (COSTS OFF) {REPOSITORY_MANIFEST_LOOKUP_SQL}");
    let plan = sqlx::query_scalar::<_, String>(&explain)
        .bind(7_i32)
        .bind(canonical)
        .bind(relative.clone())
        .bind(suffix_pattern.clone())
        .fetch_all(&mut *connection)
        .await
        .unwrap()
        .join("\n");
    assert!(
        plan.contains("ix_gamechallenges_repository_source_suffix"),
        "repository lookup did not use its bounded access path:\n{plan}"
    );
    let matches = sqlx::query_scalar::<_, i32>(REPOSITORY_MANIFEST_LOOKUP_SQL)
        .bind(7_i32)
        .bind(canonical)
        .bind(relative)
        .bind(suffix_pattern)
        .fetch_all(&mut *connection)
        .await
        .unwrap();
    assert_eq!(matches.len(), 2, "ambiguity evidence is capped at two rows");
    drop(connection);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
