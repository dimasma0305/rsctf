use super::*;

#[test]
fn private_report_accepts_only_bounded_registered_metadata() {
    assert!(Snapshot::decode(br#"{"status":"observed","files":{}}"#).is_some());
    let hash = "a".repeat(64);
    let file = serde_json::json!({"blob":hash,"executable":true});
    let sample = |name: &str, id: serde_json::Value| {
        serde_json::to_vec(
            &serde_json::json!({"sampledAt":42,"status":"observed","files":{name:id}}),
        )
        .unwrap()
    };
    assert!(Snapshot::decode(&sample("src/main", file.clone())).is_some());
    assert!(Snapshot::decode(&sample("deleted", serde_json::Value::Null)).is_some());
    for name in [".git/config", "../secret", "/outside", "x\ny", ""] {
        assert!(Snapshot::decode(&sample(name, file.clone())).is_none());
    }
    assert!(Snapshot::decode(&sample(
        "src/main",
        serde_json::json!({"blob":hash,"executable":true,"contents":"hidden"})
    ))
    .is_none());
    assert!(Snapshot::decode(br#"{"status":"legacyAgent"}"#).is_none());
    assert!(Snapshot::decode(br#"{"status":"unavailable","files":{"a":null}}"#).is_none());
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn byoc_source_history_coalesces_samples_and_bounds_private_retention() {
    let db = crate::utils::test_database::TestDatabase::new("byoc_source").await;
    sqlx::raw_sql(r#"CREATE TABLE "AdTeamServices"(id INTEGER PRIMARY KEY); INSERT INTO "AdTeamServices" VALUES (1),(2)"#)
        .execute(&db.pool).await.unwrap();
    sqlx::raw_sql(crate::migrations::m0368_byoc_source_history::UP_SQL)
        .execute(&db.pool)
        .await
        .unwrap();
    let mut tx = db.pool.begin().await.unwrap();
    let file = FileIdentity {
        blob: "a".repeat(64),
        executable: false,
    };
    let mut snap = Snapshot {
        state: "observed",
        files: BTreeMap::from([("hello".into(), Some(file))]),
    };
    for _ in 0..3 {
        store(&mut tx, 1, &snap).await.unwrap();
    }
    for i in 0..131 {
        snap.files.insert(
            "hello".into(),
            Some(FileIdentity {
                blob: format!("{i:064x}"),
                executable: i % 2 == 0,
            }),
        );
        store(&mut tx, 1, &snap).await.unwrap();
    }
    store(&mut tx, 2, &Snapshot::empty("legacyAgent"))
        .await
        .unwrap();
    let counts:(i64,i64,i64)=sqlx::query_as(r#"SELECT count(*),min(revision),max(revision) FROM "AdByocSourceHistory" WHERE service_id=1"#)
        .fetch_one(&mut *tx).await.unwrap();
    assert_eq!(counts, (128, 5, 132));
    let page = serde_json::from_str("{}").unwrap();
    let response = history(&mut tx, Some(1), page).await.unwrap();
    let body = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["events"].as_array().unwrap().len(), 8);
    assert_eq!(json["before"], 125);
    assert_eq!(
        json["events"][0]["changes"][0],
        serde_json::json!({"path":"hello","kind":"modified"})
    );
    let response = history(&mut tx, Some(2), serde_json::from_str("{}").unwrap())
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["events"][0]["status"], "legacyAgent");
    assert_eq!(json["events"][0]["changes"], serde_json::json!([]));
    tx.rollback().await.unwrap();
    db.close().await;
}
