//! Real-PostgreSQL round trip of a competition data archive through the full
//! migrated schema: export a seeded multi-format event, restore it as a new
//! game, and prove every table and every board come back identical.

use std::io::{Cursor, Read, Write};
use std::str::FromStr;
use std::sync::Arc;

use sea_orm::SqlxPostgresConnector;
use serde_json::{json, Value as JsonValue};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::spec::{catalog, Restore};
use super::*;
use crate::app_state::AppState;
use crate::controllers::edit::persist_game_import_with;
use crate::migrations::{test_process_application_name, Migrator, MigratorTrait};
use crate::models::internal::configs::AppConfig;
use crate::services::cache::InMemoryCache;
use crate::services::container::NoopContainerManager;
use crate::services::token::TokenService;
use crate::storage::LocalBlobStorage;

#[path = "db_tests_seed.rs"]
mod seed;
use seed::seed_event;

/// Every archived table's rows for one game, as JSON with identifiers and
/// timestamps blanked so two installations of the same event compare equal.
async fn table_snapshot(pool: &sqlx::PgPool, game_id: i32) -> Vec<(String, Vec<JsonValue>)> {
    let mut snapshot = Vec::new();
    for spec in catalog() {
        let rows = sqlx::query_scalar::<_, String>(&spec.select_sql())
            .bind(game_id)
            .bind(10_000i64)
            .bind(0i64)
            .fetch_all(pool)
            .await
            .unwrap_or_else(|error| panic!("{}: {error}", spec.name));
        let rows = rows
            .into_iter()
            .map(|row| {
                let mut value: JsonValue = serde_json::from_str(&row).unwrap();
                blank_identifiers(&mut value);
                value
            })
            .collect();
        snapshot.push((spec.name.to_string(), rows));
    }
    snapshot
}

fn blank_identifiers(value: &mut JsonValue) {
    match value {
        JsonValue::Object(map) => {
            for (key, entry) in map.iter_mut() {
                let is_identifier = key == "id"
                    || key.ends_with("Id")
                    || key.ends_with("Ids")
                    || key == "rosterSnapshot"
                    || key == "hillsSnapshot"
                    || key == "token"
                    || key == "captainId"
                    || key == "competitiveAdmittedAtUtc"
                    || key == "reconciliationVersion";
                if is_identifier {
                    *entry = JsonValue::Null;
                } else {
                    blank_identifiers(entry);
                }
            }
        }
        JsonValue::Array(items) => items.iter_mut().for_each(blank_identifiers),
        _ => {}
    }
}

fn board_summary(value: JsonValue) -> JsonValue {
    let mut value = value;
    blank_identifiers(&mut value);
    if let Some(map) = value.as_object_mut() {
        for volatile in [
            "updateTimeUtc",
            "revision",
            "generatedAtUtc",
            "generatedAt",
            "currentRoundEndsAt",
        ] {
            map.remove(volatile);
        }
    }
    value
}

fn rewrite_archive(bytes: &[u8], rewrite: impl Fn(&str, Vec<u8>) -> Vec<u8>) -> Vec<u8> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        let mut content = Vec::new();
        entry.read_to_end(&mut content).unwrap();
        writer.start_file(&name, options).unwrap();
        writer.write_all(&rewrite(&name, content)).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn archive_round_trips_a_multi_format_event_through_the_real_schema() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let application_name = test_process_application_name();
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(
            PgConnectOptions::from_str(&database_url)
                .unwrap()
                .application_name(application_name),
        )
        .await
        .unwrap();
    let schema = format!("rsctf_data_transfer_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin_pool)
        .await
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(6)
        .connect_with(
            PgConnectOptions::from_str(&database_url)
                .unwrap()
                .application_name(application_name)
                .options([("search_path", schema.as_str())]),
        )
        .await
        .unwrap();
    let db = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
    Migrator::up(&db, None).await.unwrap();
    let storage_root = std::env::temp_dir().join(format!("rsctf-data-transfer-{}", Uuid::new_v4()));
    let st = AppState::new(
        db,
        Arc::new(AppConfig::default()),
        Arc::new(InMemoryCache::new()),
        Arc::new(LocalBlobStorage::new(storage_root.clone())),
        TokenService::new("0123456789abcdef0123456789abcdef", 60),
        Arc::new(NoopContainerManager),
    );
    let admin = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "AspNetUsers"
             (id, user_name, normalized_user_name, email_confirmed, phone_number_confirmed,
              two_factor_enabled, lockout_enabled, access_failed_count, role, ip,
              last_signed_in_utc, last_visited_utc, register_time_utc, bio, real_name, std_number,
              exercise_visible)
           VALUES ($1, 'admin', 'ADMIN', TRUE, FALSE, FALSE, FALSE, 0, 3, '', $2, $2, $2, '', '', '', TRUE)"#,
    )
    .bind(admin)
    .bind(Utc::now())
    .execute(&pool)
    .await
    .unwrap();

    let seeded = seed_event(&st, &pool, admin).await;
    // Arm the keyed identity ledger guards exactly like a bootstrapped
    // installation, so every restored membership and placeholder account must
    // go through the identity-neutral provisioning path.
    sqlx::query(
        r#"INSERT INTO "IdentityObservationBootstrapState"
             (version, key_identifier, completed_at_utc, observations_inserted)
           VALUES (1, decode(repeat('ab', 32), 'hex'), $1, 0)"#,
    )
    .bind(Utc::now())
    .execute(&pool)
    .await
    .unwrap();
    let source_game = game::Entity::find_by_id(seeded.game_id)
        .one(&st.db)
        .await
        .unwrap()
        .unwrap();
    let source_jeopardy =
        crate::controllers::game::build_scoreboard_cached(&st, &source_game, true)
            .await
            .unwrap();
    let source_ad =
        crate::services::ad::scoring::build_ad_scoreboard(&pool, seeded.game_id, true, Utc::now())
            .await
            .unwrap();
    let source_koth =
        crate::controllers::game::koth::build_koth_scoreboard_cached(&st, &source_game, true)
            .await
            .unwrap();
    assert_eq!(source_jeopardy.items[0].score, 500);
    assert_eq!(source_ad.teams[0].team_name, "Alpha");
    assert_eq!(source_koth.teams[0].team_name, "Alpha");

    // --- Export -----------------------------------------------------------
    let bytes = super::export::export_archive_bytes(&st, &source_game, true)
        .await
        .unwrap();
    let source_snapshot = table_snapshot(&pool, seeded.game_id).await;
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes.clone())).unwrap();
    let names = (0..archive.len())
        .map(|index| archive.by_index(index).unwrap().name().to_string())
        .collect::<Vec<_>>();
    for expected in [
        "game.json",
        "manifest.json",
        "scoreboards/jeopardy.json",
        "scoreboards/overall.json",
        "scoreboards/attack-defense.json",
        "scoreboards/koth.json",
        "data/submissions.jsonl",
        "data/adFlags.jsonl",
        "data/kothTokens.jsonl",
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "missing {expected}: {names:?}"
        );
    }
    let mut manifest_text = String::new();
    archive
        .by_name("manifest.json")
        .unwrap()
        .read_to_string(&mut manifest_text)
        .unwrap();
    let manifest: DataArchiveManifest = serde_json::from_str(&manifest_text).unwrap();
    assert_eq!(manifest.kind, ARCHIVE_KIND);
    assert_eq!(manifest.source_game_id, seeded.game_id);
    let rows = |name: &str| {
        manifest
            .tables
            .iter()
            .find(|table| table.name == name)
            .map(|table| table.rows)
            .unwrap()
    };
    assert_eq!(rows("users"), 3, "members, captain, and the grading admin");
    assert_eq!(rows("submissions"), 2);
    assert_eq!(rows("firstSolves"), 1);
    assert_eq!(rows("adRounds"), 2);
    assert_eq!(rows("adFlags"), 1);
    assert_eq!(rows("kothTokens"), 1);
    assert_eq!(rows("writeups"), 1);
    assert_eq!(rows("kothOfficialConfigs"), 1);
    assert_eq!(rows("aiChatLinks"), 1);
    assert_eq!(rows("aiChatLinkEvents"), 1);
    assert_eq!(rows("solverUploads"), 1);
    assert_eq!(rows("agentArtifactMatches"), 1);
    let mut users_text = String::new();
    archive
        .by_name("data/users.jsonl")
        .unwrap()
        .read_to_string(&mut users_text)
        .unwrap();
    assert!(!users_text.contains("password") && !users_text.contains("securityStamp"));
    let writeup_entry = names
        .iter()
        .filter(|name| name.starts_with("files/") && !name.ends_with('/'))
        .count();
    assert_eq!(writeup_entry, 1, "the writeup blob is bundled");

    let unbundled = super::export::export_archive_bytes(&st, &source_game, false)
        .await
        .unwrap();
    let mut unbundled_archive = zip::ZipArchive::new(Cursor::new(unbundled)).unwrap();
    let mut unbundled_manifest = String::new();
    unbundled_archive
        .by_name("manifest.json")
        .unwrap()
        .read_to_string(&mut unbundled_manifest)
        .unwrap();
    let unbundled_manifest: DataArchiveManifest =
        serde_json::from_str(&unbundled_manifest).unwrap();
    assert!(!unbundled_manifest.attachments_bundled);
    assert!(manifest.attachments_bundled);

    // --- Restore on the same installation ---------------------------------
    let result = super::import::restore_archive(&st, bytes.clone(), admin)
        .await
        .unwrap();
    assert_ne!(result.game_id, seeded.game_id);
    assert_eq!(result.users.matched, 3);
    assert_eq!(result.users.created, 0);
    assert_eq!(result.teams.matched, 1);
    assert_eq!(result.teams.created, 0);
    for table in &result.tables {
        let archived = rows(&table.name);
        let restorable = catalog()
            .find(|spec| spec.name == table.name)
            .map(|spec| !matches!(spec.restore, Restore::ExportOnly))
            .unwrap();
        assert_eq!(table.restored, restorable, "{}", table.name);
        assert_eq!(table.rows, archived, "{} restored count", table.name);
    }
    let restored_game = game::Entity::find_by_id(result.game_id)
        .one(&st.db)
        .await
        .unwrap()
        .unwrap();
    assert!(restored_game.hidden);
    assert_eq!(restored_game.title, "Round Trip Finals");
    let restored_snapshot = table_snapshot(&pool, result.game_id).await;
    for ((name, source_rows), (_, restored_rows)) in source_snapshot.iter().zip(&restored_snapshot)
    {
        let restorable = catalog()
            .find(|spec| spec.name == name)
            .map(|spec| !matches!(spec.restore, Restore::ExportOnly))
            .unwrap();
        if restorable {
            assert_eq!(
                source_rows, restored_rows,
                "table {name} differs after restore"
            );
        }
    }
    let participation_tokens: Vec<String> =
        sqlx::query_scalar(r#"SELECT token FROM "Participations" WHERE game_id = $1"#)
            .bind(result.game_id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(participation_tokens.len(), 1);
    assert_ne!(participation_tokens[0], "source-token");
    let restored_writeup: Option<i32> =
        sqlx::query_scalar(r#"SELECT writeup_id FROM "Participations" WHERE game_id = $1"#)
            .bind(result.game_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        restored_writeup.is_some(),
        "writeup re-attached from the bundle"
    );
    let counts: (i32, i32) = sqlx::query_as(
        r#"SELECT accepted_count, submission_count FROM "GameChallenges"
            WHERE game_id = $1 ORDER BY id LIMIT 1"#,
    )
    .bind(result.game_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(counts, (1, 2), "counters recomputed from restored rows");

    let restored_jeopardy =
        crate::controllers::game::build_scoreboard_cached(&st, &restored_game, true)
            .await
            .unwrap();
    assert_eq!(
        board_summary(serde_json::to_value(&restored_jeopardy).unwrap()),
        board_summary(serde_json::to_value(&source_jeopardy).unwrap()),
    );
    let restored_ad =
        crate::services::ad::scoring::build_ad_scoreboard(&pool, result.game_id, true, Utc::now())
            .await
            .unwrap();
    assert_eq!(
        board_summary(serde_json::to_value(&restored_ad).unwrap()),
        board_summary(serde_json::to_value(&source_ad).unwrap()),
    );
    let restored_koth =
        crate::controllers::game::koth::build_koth_scoreboard_cached(&st, &restored_game, true)
            .await
            .unwrap();
    assert_eq!(
        board_summary(serde_json::to_value(&restored_koth).unwrap()),
        board_summary(serde_json::to_value(&source_koth).unwrap()),
    );

    // --- Restore on a "different installation": unknown users ---------------
    let stranger = Uuid::new_v4();
    let relocated = rewrite_archive(&bytes, |name, content| {
        if name == "data/users.jsonl"
            || name == "data/aiChatLinks.jsonl"
            || name == "data/aiChatLinkEvents.jsonl"
            || name == "data/solverUploads.jsonl"
            || name == "data/agentArtifactMatches.jsonl"
            || name == "data/teamMembers.jsonl"
            || name == "data/userParticipations.jsonl"
            || name == "data/submissions.jsonl"
            || name == "data/gameEvents.jsonl"
            || name == "data/challengeReviews.jsonl"
            || name == "data/containerAccessEvents.jsonl"
            || name == "data/teams.jsonl"
        {
            let text = String::from_utf8(content).unwrap();
            text.replace(&seeded.users[1].to_string(), &stranger.to_string())
                .replace("player1@example.test", "stranger@example.test")
                .replace("\"player1\"", "\"stranger\"")
                .into_bytes()
        } else {
            content
        }
    });
    let games_before: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "Games""#)
        .fetch_one(&pool)
        .await
        .unwrap();
    let relocated_result = super::import::restore_archive(&st, relocated, admin)
        .await
        .unwrap();
    assert_eq!(relocated_result.users.created, 1);
    assert_eq!(relocated_result.users.matched, 2);
    let placeholder: (Option<String>, String, bool) = sqlx::query_as(
        r#"SELECT password_hash, user_name, email_confirmed FROM "AspNetUsers" WHERE id = $1"#,
    )
    .bind(stranger)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(placeholder, (None, "stranger".to_string(), false));

    // --- Rejections leave nothing behind ------------------------------------
    let games_after: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "Games""#)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(games_after, games_before + 1);
    let dangling = rewrite_archive(&bytes, |name, content| {
        if name == "data/firstSolves.jsonl" {
            b"{\"participationId\":1,\"challengeId\":1,\"submissionId\":999999}\n".to_vec()
        } else {
            content
        }
    });
    let error = super::import::restore_archive(&st, dangling, admin)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("unknown"), "{error}");
    let unfinished = rewrite_archive(&bytes, |name, content| {
        if name == "game.json" {
            let mut value: JsonValue = serde_json::from_slice(&content).unwrap();
            value["endTimeUtc"] = json!((Utc::now() + chrono::Duration::hours(1)).to_rfc3339());
            value.to_string().into_bytes()
        } else {
            content
        }
    });
    let error = super::import::restore_archive(&st, unfinished, admin)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("end time"), "{error}");
    let not_an_archive = rewrite_archive(&bytes, |name, content| {
        if name == "manifest.json" {
            b"{\"kind\":\"rsctf-game\",\"formatVersion\":1,\"exportedAtUtc\":0,\"sourceGameId\":1,\"title\":\"x\",\"platformVersion\":\"0\",\"tables\":[]}".to_vec()
        } else {
            content
        }
    });
    assert!(super::import::restore_archive(&st, not_an_archive, admin)
        .await
        .is_err());
    let games_final: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "Games""#)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(games_final, games_after, "rejected restores create no game");
    assert_eq!(seeded.challenge_ids.len(), 3);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin_pool)
        .await
        .unwrap();
    admin_pool.close().await;
    let _ = std::fs::remove_dir_all(storage_root);
}
