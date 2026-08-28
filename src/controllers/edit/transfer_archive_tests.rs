use super::*;

#[test]
fn game_export_admits_before_projection_or_blob_loading() {
    let source = include_str!("transfer_export.rs");
    let handler = source.find("pub async fn export_game(").unwrap();
    let body = &source[handler..];
    let admission = body.find("bulk_export_admission").unwrap();
    let first_query = body.find("let game = load_game").unwrap();
    let first_blob = body.find("forward_attachment_sources").unwrap();
    assert!(admission < first_query);
    assert!(admission < first_blob);
}

fn archive_with(name: &str, data: &[u8]) -> Vec<u8> {
    archive_with_entries(&[(name, data)], zip::CompressionMethod::Deflated)
}

fn archive_with_entries(entries: &[(&str, &[u8])], method: zip::CompressionMethod) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(method);
    for (name, data) in entries {
        writer.start_file(*name, options).unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn patch_uncompressed_sizes(archive: &mut [u8], declared_size: u32) {
    let declared_size = declared_size.to_le_bytes();
    for index in 0..archive.len().saturating_sub(4) {
        match archive[index..index + 4] {
            [0x50, 0x4b, 0x03, 0x04] if index + 26 <= archive.len() => {
                archive[index + 22..index + 26].copy_from_slice(&declared_size);
            }
            [0x50, 0x4b, 0x01, 0x02] if index + 28 <= archive.len() => {
                archive[index + 24..index + 28].copy_from_slice(&declared_size);
            }
            _ => {}
        }
    }
}

#[test]
fn sparse_game_exports_receive_crown_cycle_defaults() {
    let model: ExportGameModel = serde_json::from_value(serde_json::json!({})).unwrap();
    assert_eq!(model.koth_epoch_ticks, 12);
    assert_eq!(model.koth_cycle_ticks, 3);
    assert_eq!(model.koth_champion_cooldown_ticks, 1);
    assert_eq!(model.koth_claim_confirmation_ticks, 2);
}

#[test]
fn game_exports_omit_discord_webhook_credentials() {
    let mut model: ExportGameModel = serde_json::from_value(serde_json::json!({})).unwrap();
    model.discord_webhook = Some(
        "https://discord.com/api/webhooks/123456789012345678/abcdefghijklmnopqrstuvwxyz_1234567890"
            .to_string(),
    );
    let serialized = serde_json::to_value(model).unwrap();
    assert!(serialized.get("discordWebhook").is_none());
}

#[test]
fn sparse_challenge_imports_disable_blood_bonus() {
    let model: ExportChallengeModel = serde_json::from_value(serde_json::json!({})).unwrap();
    assert!(model.disable_blood_bonus);

    let enabled: ExportChallengeModel =
        serde_json::from_value(serde_json::json!({ "disableBloodBonus": false })).unwrap();
    assert!(!enabled.disable_blood_bonus);
}

#[test]
fn game_import_archive_rejects_traversal_and_zip_bombs() {
    let traversal = archive_with("../game.json", b"{}");
    assert!(read_game_import_archive(&traversal).is_err());

    let backslash_traversal = archive_with("..\\game.json", b"{}");
    assert!(read_game_import_archive(&backslash_traversal).is_err());

    let parent_alias = archive_with("package/../game.json", b"{}");
    assert!(read_game_import_archive(&parent_alias).is_err());

    let compressed_bomb = archive_with("game.json", &vec![0u8; 1024 * 1024]);
    assert!(read_game_import_archive(&compressed_bomb).is_err());
}

#[test]
fn game_import_archive_counts_actual_bytes_across_forged_entries() {
    let mut archive = archive_with_entries(
        &[("first.bin", b"123456"), ("second.bin", b"abcdef")],
        zip::CompressionMethod::Stored,
    );
    patch_uncompressed_sizes(&mut archive, 1);

    let limits = GameImportLimits {
        entries: 10,
        file_bytes: 8,
        total_bytes: 8,
        compression_ratio: 200,
        path_components: 4,
    };
    assert!(read_game_import_archive_with_limits(&archive, limits).is_err());
}

fn valid_import_challenge(id: i32) -> ExportChallengeModel {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "originalScore": 100,
        "minScoreRate": 0.2,
        "difficulty": 5.0,
        "submissionLimit": 0,
        "adScoringWeight": 1.0
    }))
    .unwrap()
}

#[test]
fn archive_import_reuses_shared_schedule_validation() {
    let mut model: ExportGameModel = serde_json::from_value(serde_json::json!({})).unwrap();
    model.start_time_utc = chrono::Utc::now();
    model.end_time_utc = model.start_time_utc + chrono::Duration::hours(1);
    assert!(model.configuration().validate().is_ok());

    model.end_time_utc = model.start_time_utc;
    assert!(model.configuration().validate().is_err());
}

#[test]
fn archive_import_rejects_duplicate_ids_and_invalid_engine_weights() {
    let challenge = valid_import_challenge(9);
    assert!(validate_import_challenges(std::slice::from_ref(&challenge)).is_ok());
    assert!(validate_import_challenges(&[challenge.clone(), challenge.clone()]).is_err());

    let mut invalid = challenge;
    invalid.id = 10;
    invalid.ad_scoring_weight = f64::NAN;
    assert!(validate_import_challenges(&[invalid]).is_err());
}

#[test]
fn archive_import_rejects_impossible_flags_and_template_expansion() {
    let mut challenge = valid_import_challenge(12);
    challenge.flags.push(ExportFlagModel {
        flag: "x".repeat(128),
        attachment_type: None,
        file_hash: None,
        remote_url: None,
        file_name: None,
    });
    assert!(validate_import_challenges(std::slice::from_ref(&challenge)).is_err());

    challenge.flags.clear();
    challenge.challenge_type = ChallengeType::DynamicContainer;
    challenge.flag_template = Some(format!("flag{{{}}}", "[GUID]".repeat(4)));
    assert!(validate_import_challenges(&[challenge]).is_err());
}

#[test]
fn archive_import_preserves_supported_isolation_and_rejects_unsafe_modes() {
    let mut challenge = valid_import_challenge(10);
    challenge.challenge_type = ChallengeType::DynamicContainer;
    challenge.network_mode = Some(NetworkMode::Isolated);
    assert!(validate_import_challenges(std::slice::from_ref(&challenge)).is_ok());

    challenge.challenge_type = ChallengeType::AttackDefense;
    assert!(validate_import_challenges(std::slice::from_ref(&challenge)).is_err());

    challenge.challenge_type = ChallengeType::DynamicContainer;
    challenge.network_mode = Some(NetworkMode::Custom);
    assert!(validate_import_challenges(&[challenge]).is_err());
}
