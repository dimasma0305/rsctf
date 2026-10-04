use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use sqlx::postgres::PgPoolOptions;

use super::*;

struct ScratchCaptureRoot(PathBuf);

impl ScratchCaptureRoot {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("rsctf-traffic-inventory-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn capture(&self, challenge_id: i32, participation_id: i32, name: &str, bytes: &[u8]) {
        let directory = self
            .0
            .join(challenge_id.to_string())
            .join(participation_id.to_string());
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(name), bytes).unwrap();
    }

    fn path(&self, challenge_id: i32, participation_id: i32, name: &str) -> PathBuf {
        self.0
            .join(challenge_id.to_string())
            .join(participation_id.to_string())
            .join(name)
    }
}

impl Drop for ScratchCaptureRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn inventory_scan_is_scope_safe_and_ignores_non_pcaps() {
    let root = ScratchCaptureRoot::new();
    root.capture(7, 21, "capture.pcap", b"pcap");
    root.capture(7, 21, "notes.txt", b"not a capture");
    root.capture(7, 22, "second.PCAP", b"other");
    root.capture(-1, 23, "ignored.pcap", b"invalid scope");

    let mut files = reconcile::scan_for_test(&root.0).unwrap();
    files.sort();

    assert_eq!(
        files,
        vec![
            (7, 21, "capture.pcap".to_string(), 4),
            (7, 22, "second.PCAP".to_string(), 5),
        ]
    );
}

#[test]
fn opaque_cursor_round_trips_and_rejects_the_wrong_version() {
    let cursor = IdCursor {
        version: 1,
        time_micros: 1_700_000_000_123_456,
        id: 42,
    };
    let encoded = encode_cursor(&cursor);
    let decoded = decode_optional_cursor::<IdCursor>(Some(&encoded))
        .unwrap()
        .unwrap();
    assert_eq!(decoded.time_micros, cursor.time_micros);
    assert_eq!(decoded.id, 42);

    let wrong = encode_cursor(&IdCursor {
        version: 2,
        ..cursor
    });
    assert!(decode_optional_cursor::<IdCursor>(Some(&wrong)).is_err());
}

#[test]
fn capture_names_reject_traversal_and_non_pcap_extensions() {
    assert!(valid_capture_name("capture-1.pcap"));
    assert!(!valid_capture_name("../capture.pcap"));
    assert!(!valid_capture_name("nested/capture.pcap"));
    assert!(!valid_capture_name("capture.zip"));
}

#[test]
fn a_full_writer_queue_marks_the_durable_inventory_dirty_without_blocking() {
    let (sender, _receiver) = tokio::sync::mpsc::channel(1);
    let dirty = Arc::new(AtomicBool::new(false));
    let queue = CaptureInventoryQueue {
        sender,
        dirty: dirty.clone(),
    };
    let mutation = || InventoryMutation::DeleteFiles {
        challenge_id: 7,
        participation_id: 21,
        file_names: vec!["capture.pcap".to_string()],
    };

    queue.try_send(mutation());
    queue.try_send(mutation());

    assert!(dirty.load(Ordering::Acquire));
}

#[tokio::test]
async fn reconcile_listing_permit_outlives_a_cancelled_waiter() {
    let gate = Arc::new(tokio::sync::Semaphore::new(1));
    let permit = gate.clone().acquire_owned().await.unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (finish_tx, finish_rx) = std::sync::mpsc::channel();
    let waiter = tokio::spawn(reconcile::spawn_blocking_with_permit(permit, move || {
        let _ = started_tx.send(());
        let _ = finish_rx.recv();
    }));

    started_rx.await.unwrap();
    waiter.abort();
    let _ = waiter.await;
    assert!(gate.clone().try_acquire_owned().is_err());

    finish_tx.send(()).unwrap();
    let released = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let Ok(permit) = gate.clone().try_acquire_owned() {
                break permit;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("blocking scan retained the permit after it completed");
    drop(released);
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn postgres_inventory_reconciles_counts_pages_and_mutations() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(crate::migrations::test_pg_connect_options(&database_url))
        .await
        .unwrap();
    let schema = format!("rsctf_traffic_inventory_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = crate::migrations::test_pg_connect_options(&database_url)
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TABLE "GameChallenges" (
            id INTEGER PRIMARY KEY,
            game_id INTEGER NOT NULL,
            title TEXT NOT NULL,
            category SMALLINT NOT NULL,
            "Type" SMALLINT NOT NULL,
            is_enabled BOOLEAN NOT NULL,
            enable_traffic_capture BOOLEAN NOT NULL
        );
        CREATE INDEX ix_test_game_challenges_game ON "GameChallenges" (game_id, id);
        CREATE TABLE "Participations" (
            id INTEGER PRIMARY KEY,
            game_id INTEGER NOT NULL,
            team_id INTEGER NOT NULL,
            division_id INTEGER NULL
        );
        CREATE TABLE "Teams" (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            avatar_hash TEXT NULL
        );
        CREATE TABLE "Divisions" (
            id INTEGER PRIMARY KEY,
            game_id INTEGER NOT NULL,
            name TEXT NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(crate::migrations::TRAFFIC_CAPTURE_INVENTORY_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(crate::migrations::TRAFFIC_CAPTURE_INVENTORY_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        INSERT INTO "GameChallenges"
            (id, game_id, title, category, "Type", is_enabled, enable_traffic_capture)
        VALUES
            (7, 3, 'Traffic', 6, 4, TRUE, TRUE),
            (8, 3, 'Empty', 0, 0, TRUE, TRUE);
        INSERT INTO "Divisions" (id, game_id, name)
        VALUES (5, 3, 'Open'), (6, 4, 'Wrong game');
        INSERT INTO "Teams" (id, name, avatar_hash) VALUES (9, 'Blue', 'avatar-hash');
        INSERT INTO "Participations" (id, game_id, team_id, division_id)
        VALUES (21, 3, 9, 5), (22, 4, 9, 5), (23, 3, 9, 6);
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let root = ScratchCaptureRoot::new();
    root.capture(7, 21, "older.pcap", b"old");
    std::thread::sleep(Duration::from_millis(5));
    root.capture(7, 21, "newer.pcap", b"newer");
    root.capture(7, 22, "wrong-game.pcap", b"hidden");
    root.capture(7, 23, "wrong-division.pcap", b"visible");
    reconcile::reconcile_once(&pool, &root.0).await.unwrap();

    let bucket = sqlx::query_as::<_, (i32, i64)>(
        r#"SELECT file_count, total_bytes
             FROM "TrafficCaptureBuckets"
            WHERE challenge_id = 7 AND participation_id = 21"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(bucket, (2, 8));

    let challenges = challenge_page(&pool, &root.0, 3, &CapturePageQuery::capped(100))
        .await
        .unwrap();
    assert_eq!(challenges.items.len(), 2);
    assert_eq!(challenges.items[0].id, 7);
    assert_eq!(challenges.items[0].count, 3);

    let teams = team_page(&pool, &root.0, 7, &CapturePageQuery::capped(100))
        .await
        .unwrap();
    assert_eq!(teams.items.len(), 2);
    assert!(!teams.items.iter().any(|item| item.id == 22));
    assert_eq!(
        teams
            .items
            .iter()
            .find(|item| item.id == 21)
            .and_then(|item| item.division.as_deref()),
        Some("Open")
    );
    assert_eq!(
        teams
            .items
            .iter()
            .find(|item| item.id == 23)
            .and_then(|item| item.division.as_deref()),
        None
    );
    assert_eq!(
        teams
            .items
            .iter()
            .find(|item| item.id == 21)
            .and_then(|item| item.avatar.as_deref()),
        Some("/assets/avatar-hash/avatar")
    );

    let first = file_page(
        &pool,
        &root.0,
        7,
        21,
        &CapturePageQuery {
            count: 1,
            cursor: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(first.items.len(), 1);
    assert!(first.next_cursor.is_some());
    let second = file_page(
        &pool,
        &root.0,
        7,
        21,
        &CapturePageQuery {
            count: 1,
            cursor: first.next_cursor,
        },
    )
    .await
    .unwrap();
    assert_eq!(second.items.len(), 1);
    assert_ne!(first.items[0].file_name, second.items[0].file_name);

    let newer = root.path(7, 21, "newer.pcap");
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&newer)
        .unwrap();
    file.write_all(b"-grown").unwrap();
    file.sync_all().unwrap();
    upsert_files(&pool, &[InventoryFile::from_path(7, 21, &newer).unwrap()])
        .await
        .unwrap();
    let total: i64 = sqlx::query_scalar(
        r#"SELECT total_bytes FROM "TrafficCaptureBuckets"
            WHERE challenge_id = 7 AND participation_id = 21"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(total, 14);

    std::fs::remove_file(&newer).unwrap();
    delete_file(&pool, 7, 21, "newer.pcap").await.unwrap();
    let bucket = sqlx::query_as::<_, (i32, i64)>(
        r#"SELECT file_count, total_bytes
             FROM "TrafficCaptureBuckets"
            WHERE challenge_id = 7 AND participation_id = 21"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(bucket, (1, 3));

    std::fs::remove_file(root.path(7, 21, "older.pcap")).unwrap();
    delete_file(&pool, 7, 21, "older.pcap").await.unwrap();
    let bucket_exists: bool = sqlx::query_scalar(
        r#"SELECT EXISTS (
               SELECT 1 FROM "TrafficCaptureBuckets"
                WHERE challenge_id = 7 AND participation_id = 21
           )"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!bucket_exists);

    let index_count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::BIGINT
             FROM pg_indexes
            WHERE schemaname = current_schema()
              AND indexname IN (
                  'ix_trafficcapturefiles_newest',
                  'ix_trafficcapturebuckets_challenge_newest'
              )"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(index_count, 2);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}
