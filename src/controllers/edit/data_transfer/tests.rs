use std::collections::{BTreeMap, HashSet};

use serde_json::json;

use super::import::{parse_manifest, parse_rows, parse_tables, rewrite_json, RestoreContext};
use super::spec::{archive_key, catalog, JsonRewrite, Map, Restore};
use super::*;

const SECRET_COLUMNS: &[&str] = &[
    "password_hash",
    "security_stamp",
    "concurrency_stamp",
    "private_key",
    "private_key_ciphertext",
    "public_key",
    "hmac_secret",
    "token_hash",
    "invite_token",
    "lease_token",
    "pipeline_lease_token",
    "github_token",
    "token_ciphertext",
];

#[test]
fn archive_keys_are_camel_case_with_the_pascal_type_column_lowered() {
    assert_eq!(archive_key("participation_id"), "participationId");
    assert_eq!(archive_key("id"), "id");
    assert_eq!(archive_key("Type"), "type");
    assert_eq!(
        archive_key("cumulative_sla_numerator"),
        "cumulativeSlaNumerator"
    );
}

#[test]
fn table_catalog_is_unique_bounded_and_secret_free() {
    let mut names = HashSet::new();
    for spec in catalog() {
        assert!(
            names.insert(spec.name),
            "duplicate archive table {}",
            spec.name
        );
        assert!(!spec.columns.is_empty(), "{} exports no columns", spec.name);
        for column in spec.columns() {
            if let Some(identifier) = column.identifier {
                assert!(
                    !SECRET_COLUMNS.contains(&identifier),
                    "{} would export secret column {identifier}",
                    spec.name
                );
                assert_ne!(
                    identifier, "game_id",
                    "{} lists game_id explicitly",
                    spec.name
                );
            }
        }
        let sql = spec.select_sql();
        assert!(
            sql.contains("$1") && sql.contains("LIMIT $2 OFFSET $3"),
            "{sql}"
        );
        assert!(
            sql.contains("ORDER BY"),
            "{} is not deterministically ordered",
            spec.name
        );
        assert_eq!(spec.entry(), format!("data/{}.jsonl", spec.name));
    }
}

#[test]
fn every_restored_reference_is_produced_earlier_in_catalog_order() {
    let mut produced: HashSet<Map> = [
        Map::Challenge,
        Map::Division,
        Map::Participation,
        Map::Team,
        Map::User,
    ]
    .into_iter()
    .collect();
    for spec in catalog() {
        let Restore::Rows(restore) = spec.restore else {
            continue;
        };
        let identifiers = spec
            .columns()
            .iter()
            .filter_map(|column| column.identifier)
            .collect::<Vec<_>>();
        if let Some(map) = restore.id {
            assert!(
                identifiers.contains(&"id"),
                "{} allocates ids without an id column",
                spec.name
            );
            assert!(produced.insert(map), "{} re-produces {:?}", spec.name, map);
        }
        for (column, map) in restore.remaps {
            assert!(
                identifiers.contains(column),
                "{} remaps unknown column {column}",
                spec.name
            );
            assert!(
                produced.contains(map),
                "{} references {:?} before it is restored",
                spec.name,
                map
            );
        }
        if restore.json_rewrite.is_some() {
            assert!(produced.contains(&Map::Participation) && produced.contains(&Map::Challenge));
        }
        assert!(
            spec.from.is_none(),
            "{} joins other tables but restores generic rows",
            spec.name
        );
    }
}

#[test]
fn export_only_tables_hold_deployment_local_references() {
    let export_only = catalog()
        .filter(|spec| matches!(spec.restore, Restore::ExportOnly))
        .map(|spec| spec.name)
        .collect::<Vec<_>>();
    assert_eq!(
        export_only,
        [
            "solveReceiptAudit",
            "adFlagDeliveryResults",
            "buildRecords",
            "challengeVariants",
            "vpnFlagTransportEvents",
            "vpnFlowTelemetryBuckets",
            "vpnDnsProviderBuckets",
            "vpnPeerNetworkObservations",
        ]
    );
}

fn entries_with_manifest(manifest: serde_json::Value) -> BTreeMap<String, Vec<u8>> {
    let mut entries = BTreeMap::new();
    entries.insert(
        "manifest.json".to_string(),
        manifest.to_string().into_bytes(),
    );
    entries
}

#[test]
fn manifest_must_declare_the_supported_kind_and_version() {
    let valid = json!({
        "kind": ARCHIVE_KIND,
        "formatVersion": ARCHIVE_FORMAT_VERSION,
        "exportedAtUtc": 1_727_400_000_000i64,
        "sourceGameId": 19,
        "title": "Event",
        "platformVersion": "0.1.0",
        "tables": []
    });
    let manifest = parse_manifest(&entries_with_manifest(valid.clone())).unwrap();
    assert_eq!(manifest.source_game_id, 19);
    assert_eq!(
        manifest.exported_at_utc.timestamp_millis(),
        1_727_400_000_000
    );

    let mut wrong_kind = valid.clone();
    wrong_kind["kind"] = json!("rsctf-game");
    assert!(parse_manifest(&entries_with_manifest(wrong_kind)).is_err());
    let mut wrong_version = valid;
    wrong_version["formatVersion"] = json!(2);
    assert!(parse_manifest(&entries_with_manifest(wrong_version)).is_err());
    assert!(parse_manifest(&BTreeMap::new()).is_err());
}

#[test]
fn json_lines_are_parsed_per_row_and_unknown_tables_are_rejected() {
    let rows = parse_rows("submissions", b"{\"id\":1}\n\n{\"id\":2}\n").unwrap();
    assert_eq!(rows.len(), 2);
    assert!(parse_rows("submissions", b"[1,2]\n").is_err());
    assert!(parse_rows("submissions", b"{\"id\":1}\nnot json\n").is_err());

    let mut entries = BTreeMap::new();
    entries.insert("data/nope.jsonl".to_string(), Vec::new());
    assert!(parse_tables(&entries).is_err());
    let mut entries = BTreeMap::new();
    entries.insert(
        "data/submissions.jsonl".to_string(),
        b"{\"id\":7}\n".to_vec(),
    );
    entries.insert("scoreboards/jeopardy.json".to_string(), b"{}".to_vec());
    let tables = parse_tables(&entries).unwrap();
    assert_eq!(tables.rows("submissions").len(), 1);
    assert!(tables.rows("adFlags").is_empty());
}

#[test]
fn identifier_maps_pass_null_and_reject_unknown_references() {
    let mut context = RestoreContext::new(42);
    context.record(Map::Participation, &json!(5), json!(50));
    assert_eq!(
        context
            .mapped(Map::Participation, &json!(5), "t", "c")
            .unwrap(),
        json!(50)
    );
    assert_eq!(
        context
            .mapped(Map::Participation, &json!(null), "t", "c")
            .unwrap(),
        json!(null)
    );
    let error = context
        .mapped(
            Map::Participation,
            &json!(6),
            "submissions",
            "participationId",
        )
        .unwrap_err();
    assert!(error.to_string().contains("unknown participation 6"));
}

#[test]
fn koth_official_config_snapshots_are_rewritten_through_the_maps() {
    let mut context = RestoreContext::new(42);
    context.record(Map::Participation, &json!(7), json!(70));
    context.record(Map::Challenge, &json!(5), json!(55));
    let mut object = json!({
        "roster_snapshot": [7],
        "hills_snapshot": [{"challengeId": 5, "serviceWeight": 1.0}]
    })
    .as_object()
    .cloned()
    .unwrap();
    rewrite_json(
        JsonRewrite::KothOfficialConfig,
        "kothOfficialConfigs",
        &mut object,
        &context,
    )
    .unwrap();
    assert_eq!(object["roster_snapshot"], json!([70]));
    assert_eq!(object["hills_snapshot"][0]["challengeId"], json!(55));

    let mut unknown = json!({"roster_snapshot": [8], "hills_snapshot": []})
        .as_object()
        .cloned()
        .unwrap();
    assert!(rewrite_json(JsonRewrite::KothOfficialConfig, "k", &mut unknown, &context).is_err());
}

#[test]
fn wire_models_are_camel_case_with_millisecond_timestamps() {
    let result = GameDataImportResult {
        game_id: 3,
        title: "Event".to_string(),
        source_game_id: 19,
        exported_at_utc: DateTime::<Utc>::from_timestamp_millis(1_727_400_000_000).unwrap(),
        tables: vec![GameDataImportTable {
            name: "submissions".to_string(),
            rows: 8,
            restored: true,
        }],
        users: RosterOutcome {
            matched: 1,
            created: 2,
        },
        teams: RosterOutcome::default(),
    };
    let value = serde_json::to_value(&result).unwrap();
    assert_eq!(value["gameId"], json!(3));
    assert_eq!(value["sourceGameId"], json!(19));
    assert_eq!(value["exportedAtUtc"], json!(1_727_400_000_000i64));
    assert_eq!(value["tables"][0]["restored"], json!(true));
    assert_eq!(value["users"]["created"], json!(2));
}

#[test]
fn handlers_admit_before_reading_and_restore_stays_admin_only() {
    let export = include_str!("export.rs");
    let handler = export.find("pub async fn export_game_data(").unwrap();
    let body = &export[handler..];
    let authorization = body.find("manager_or_admin").unwrap();
    let admission = body.find("bulk_export_admission").unwrap();
    let first_query = body.find("load_game").unwrap();
    assert!(authorization < admission && admission < first_query);
    assert_eq!(
        body.matches("manager_or_admin").count(),
        2,
        "authorization is re-proven"
    );

    let import = include_str!("import.rs");
    let handler = import.find("pub async fn import_game_data(").unwrap();
    let signature = &import[handler..handler + 200];
    assert!(signature.contains("AdminUser(admin): AdminUser"));
    assert!(import.contains("end_time_utc > Utc::now()"));
}

#[test]
fn export_query_selects_attachment_bundling() {
    let default = super::export::DataExportQuery::default();
    assert!(default.bundle_attachments().unwrap());
    let skip = super::export::DataExportQuery {
        attachments: Some("skip".to_string()),
    };
    assert!(!skip.bundle_attachments().unwrap());
    let unknown = super::export::DataExportQuery {
        attachments: Some("maybe".to_string()),
    };
    assert!(unknown.bundle_attachments().is_err());
    let manifest: DataArchiveManifest = serde_json::from_value(json!({
        "kind": ARCHIVE_KIND, "formatVersion": 1, "exportedAtUtc": 0, "sourceGameId": 1,
        "title": "x", "platformVersion": "0", "tables": []
    }))
    .unwrap();
    assert!(
        manifest.attachments_bundled,
        "older manifests default to bundled"
    );
}
