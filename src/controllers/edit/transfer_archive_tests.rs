use super::*;

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
