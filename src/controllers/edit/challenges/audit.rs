use super::*;

/// `GET /api/edit/games/{id}/challenges/{cId}/auditmeta` — parsed audit metadata
/// (`ChallengeAuditModel`). Mirrors `EditController.GetChallengeAuditMeta`: opens
/// the challenge's persisted `original_archive_blob_path`, extracts it, and returns
/// the raw yaml, the file tree, and previews of reviewer-targeted files.
///
/// `archiveAvailable` is false only when no archive is on file or the blob is
/// missing; a corrupt/unparseable archive still reports `true` (with empty
/// files/previews), matching RSCTF's catch behavior. `buildStatus`/`lastBuildLog`
/// are always carried through so the modal renders its rebuild button + log panel.
pub async fn get_challenge_audit_meta(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<JsonValue>> {
    manager_or_admin(&st, &user, id).await?;
    let challenge = load_challenge(&st, id, c_id).await?;

    // Empty shape (archive absent/missing): valid `ChallengeAuditModel` with the
    // build fields still populated.
    let empty = |available: bool| {
        json!({
            "archiveAvailable": available,
            "files": [],
            "previews": {},
            "yamlText": JsonValue::Null,
            "buildStatus": challenge.build_status,
            "lastBuildLog": challenge.last_build_log,
        })
    };

    let Some(hash) = challenge
        .original_archive_blob_path
        .as_deref()
        .filter(|h| !h.is_empty())
    else {
        return Ok(RequestResponse::ok(empty(false)));
    };
    // Blob gone from storage -> archive not available.
    let bytes = match st.storage.load(hash).await {
        Ok(b) => b,
        Err(_) => return Ok(RequestResponse::ok(empty(false))),
    };

    let mut model = parse_audit_archive(&bytes);
    if let Some(obj) = model.as_object_mut() {
        obj.insert(
            "buildStatus".into(),
            serde_json::to_value(challenge.build_status).unwrap_or(JsonValue::Null),
        );
        obj.insert(
            "lastBuildLog".into(),
            serde_json::to_value(&challenge.last_build_log).unwrap_or(JsonValue::Null),
        );
    }
    Ok(RequestResponse::ok(model))
}

/// Parse a challenge source archive into the `ChallengeAuditModel` core
/// (`archiveAvailable`/`files`/`previews`/`yamlText`). Best-effort and infallible:
/// a corrupt archive yields `archiveAvailable: true` with empty files (the blob
/// was on file, so the archive "exists" — it just couldn't be read), mirroring
/// RSCTF `FillAuditModel`. Previews are keyed by relative path so the modal can
/// blue-highlight previewed entries in the file tree.
fn parse_audit_archive(bytes: &[u8]) -> JsonValue {
    // Eligibility/size caps mirror RSCTF FillAuditModel.
    const PREVIEW_MAX: u64 = 64 * 1024;
    const PREVIEW_TRUNC: usize = 8 * 1024;
    const PREVIEW_KEYWORDS: [&str; 6] =
        ["readme", "writeup", "solution", "solve", "solver", "notes"];

    let mut files: Vec<(String, u64)> = Vec::new();
    let mut yaml_text: Option<String> = None;
    let mut previews = serde_json::Map::new();

    if let Ok(mut archive) = zip::ZipArchive::new(Cursor::new(bytes)) {
        for i in 0..archive.len() {
            let Ok(mut entry) = archive.by_index(i) else {
                continue;
            };
            if entry.is_dir() {
                continue;
            }
            // Audit display is best-effort, but never present a normalized alias
            // as though it were the raw reviewed archive path.
            let Some(name_path) = crate::utils::archive::canonical_zip_entry_path(&entry) else {
                continue;
            };
            let rel = name_path.to_string_lossy().replace('\\', "/");
            let size = entry.size();
            files.push((rel.clone(), size));

            let file_name = name_path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            let lower = file_name.to_ascii_lowercase();

            // First challenge.yml/.yaml wins; it isn't also emitted as a preview.
            if yaml_text.is_none() && (lower == "challenge.yaml" || lower == "challenge.yml") {
                let mut buf = String::new();
                if std::io::Read::read_to_string(&mut entry, &mut buf).is_ok() {
                    yaml_text = Some(buf);
                }
                continue;
            }

            if size <= PREVIEW_MAX && PREVIEW_KEYWORDS.iter().any(|k| lower.contains(k)) {
                let mut data = Vec::new();
                if std::io::Read::read_to_end(&mut entry, &mut data).is_ok() {
                    if let Ok(mut s) = String::from_utf8(data) {
                        if s.len() > PREVIEW_TRUNC {
                            // Truncate on a char boundary to avoid a panic.
                            let mut end = PREVIEW_TRUNC;
                            while end > 0 && !s.is_char_boundary(end) {
                                end -= 1;
                            }
                            s.truncate(end);
                            s.push_str("\n…(truncated)");
                        }
                        previews.insert(rel, JsonValue::String(s));
                    }
                }
            }
        }
    }

    files.sort_by_key(|file| file.0.to_ascii_lowercase());
    let files_json: Vec<JsonValue> = files
        .into_iter()
        .map(|(path, size)| json!({ "path": path, "size": size }))
        .collect();

    json!({
        "archiveAvailable": true,
        "files": files_json,
        "previews": JsonValue::Object(previews),
        "yamlText": yaml_text,
    })
}

/// `POST /api/edit/games/{id}/challenges/{cId}/rebuild` — (re)build the
/// challenge's container image. Mirrors `EditController.RebuildChallengeImage`.
///
/// When the challenge carries a persisted build-context selector
/// (`build_context_subdir`), the selected subtree of its immutable source archive
/// is built with `Docker::build_image`; otherwise the referenced
/// `container_image` is pulled with `Docker::create_image`. `build_status`
/// moves `Building -> Success/Failed` accordingly. Degrades to a valid 200 when
/// the daemon is unreachable (the build stays enqueued/`Queued`), never a 5xx.
///
/// Contract preserved: the response is a `ChallengeAuditModel`-shaped object —
/// the original `files`/`previews`/`archiveAvailable` keys plus the build
/// outcome (`buildStatus`/`lastBuildLog`) the UI polls for.
pub async fn rebuild_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<JsonValue>> {
    manager_or_admin(&st, &user, id).await?;
    let challenge = load_challenge(&st, id, c_id).await?;

    // Whether a usable build context is on file (present in blob storage).
    let archive_available = match challenge.original_archive_blob_path.as_deref() {
        Some(path) if !path.is_empty() => st.storage.exists(path).await,
        _ => false,
    };

    // Run the build/pull seam, then persist the terminal outcome in one write.
    // The audit metadata response does not poll the DB for an intermediate
    // `Building` state, so return the synchronous outcome below.
    let (outcome, _record) = run_challenge_build(&st, &challenge, "Manual", 1).await;

    // The build seam persists status/log before releasing its distributed
    // per-challenge lock, so a concurrent replica cannot publish stale state.

    Ok(RequestResponse::ok(json!({
        "files": [],
        "previews": {},
        "archiveAvailable": archive_available,
        "buildStatus": outcome.status,
        "lastBuildLog": outcome.log,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, data) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn audit_skips_noncanonical_archive_aliases() {
        let model = parse_audit_archive(&archive(&[
            ("checker/../challenge.yml", b"name: forged\n"),
            ("checker\\notes.txt", b"ambiguous"),
            ("checker/./solution.txt", b"normalized"),
            ("checker/notes.txt", b"reviewed"),
        ]));

        let files = model["files"].as_array().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0]["path"], "checker/notes.txt");
        assert!(model["yamlText"].is_null());
        assert_eq!(model["previews"]["checker/notes.txt"], "reviewed");
    }
}
