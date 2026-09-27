//! Streaming export of a competition data archive.
//!
//! The response owns bulk-export admission for its whole life. Rows are read
//! in bounded pages and forwarded as JSON Lines through the same
//! response-owned ZIP stream that carries bundled blobs, so neither a table
//! nor the archive is ever retained in memory.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use bytes::Bytes;

use super::spec::{catalog, TableSpec};
use super::*;
use crate::controllers::edit::{
    forward_attachment_sources, project_game_definition_with_blobs, write_streamed_zip,
    ArchiveInput, ArchiveSource, DefinitionProjection, GameZipChunk,
};

/// Admission weight: pages, one rendered board set, and stream buffers.
const DATA_EXPORT_RETAINED_BYTES: usize = 32 * 1024 * 1024;
/// Bundled attachment plus writeup blobs share this cap.
pub(crate) const MAX_DATA_EXPORT_BLOB_BYTES: usize = 128 * 1024 * 1024;
pub(crate) const MAX_DATA_EXPORT_ROWS_PER_TABLE: u64 = 500_000;
const DATA_EXPORT_PAGE_ROWS: i64 = 2_000;
const MAX_WRITEUP_FILES: usize = 4_096;

#[derive(sqlx::FromRow)]
struct WriteupBlobRow {
    hash: String,
    file_size: i64,
}

/// `?attachments=skip` exports attachment metadata without bundling blobs, for
/// events whose files exceed the bundle cap.
#[derive(Debug, Default, Deserialize)]
pub struct DataExportQuery {
    #[serde(default)]
    pub attachments: Option<String>,
}

impl DataExportQuery {
    pub(super) fn bundle_attachments(&self) -> AppResult<bool> {
        match self.attachments.as_deref() {
            None | Some("bundle") => Ok(true),
            Some("skip") => Ok(false),
            Some(other) => Err(AppError::bad_request(format!(
                "Unknown attachments mode {other:?}; use \"bundle\" or \"skip\""
            ))),
        }
    }
}

/// `POST /api/edit/games/{id}/export/data` — stream the complete competition
/// data archive of one game. Manager or admin.
pub async fn export_game_data(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    axum::extract::Query(query): axum::extract::Query<DataExportQuery>,
) -> AppResult<Response> {
    let bundle_attachments = query.bundle_attachments()?;
    manager_or_admin(&st, &user, id).await?;
    let permit = match st
        .bulk_export_admission
        .try_acquire(Arc::clone(&st.cache), DATA_EXPORT_RETAINED_BYTES)
        .await
    {
        Ok(permit) => Arc::new(permit),
        Err(_) => return Ok(crate::services::bulk_export::overload_response()),
    };
    let game = load_game(&st, id).await?;
    let (output_receiver, prepared) = prepare_archive(&st, &game, bundle_attachments).await?;
    // Re-prove authorization after the complete relational projection and
    // before any response bytes or storage reads can escape.
    manager_or_admin(&st, &user, id).await?;
    start_archive(&st, &game, prepared, Arc::clone(&permit));

    let filename = format!("game-{id}-data.zip");
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
        crate::services::bulk_export::permitted_stream_body(
            tokio_stream::wrappers::ReceiverStream::new(output_receiver),
            permit,
        ),
    )
        .into_response())
}

/// Everything an archive stream needs before any byte leaves the process.
pub(super) struct PreparedArchive {
    projection: DefinitionProjection,
    attachments_bundled: bool,
    sources: Vec<ArchiveSource>,
    boards: Vec<(String, Bytes)>,
    input_sender: tokio::sync::mpsc::Sender<ArchiveInput>,
    input_receiver: tokio::sync::mpsc::Receiver<ArchiveInput>,
    output_sender: tokio::sync::mpsc::Sender<GameZipChunk>,
}

/// Project the definition, bundled blobs, and rendered boards; returns the
/// receiving end of the ZIP stream that [`start_archive`] will fill.
pub(super) async fn prepare_archive(
    st: &SharedState,
    game: &game::Model,
    bundle_attachments: bool,
) -> AppResult<(tokio::sync::mpsc::Receiver<GameZipChunk>, PreparedArchive)> {
    let projection = project_game_definition_with_blobs(st, game, bundle_attachments).await?;
    let sources = with_writeup_sources(st.pg(), game.id, Vec::new()).await?;
    let sources = merge_sources(projection.sources.clone(), sources)?;
    let boards = render_scoreboards(st, game, &projection.challenges).await;
    let (input_sender, input_receiver) = tokio::sync::mpsc::channel::<ArchiveInput>(8);
    let (output_sender, output_receiver) = tokio::sync::mpsc::channel::<GameZipChunk>(8);
    Ok((
        output_receiver,
        PreparedArchive {
            projection,
            attachments_bundled: bundle_attachments,
            sources,
            boards,
            input_sender,
            input_receiver,
            output_sender,
        },
    ))
}

/// Spawn the ZIP writer and the row/blob producer. Both hold `permit` until
/// they finish, so admission outlives every database and storage read.
pub(super) fn start_archive(
    st: &SharedState,
    game: &game::Model,
    prepared: PreparedArchive,
    permit: Arc<crate::services::bulk_export::BulkExportPermit>,
) {
    let PreparedArchive {
        projection,
        attachments_bundled,
        sources,
        boards,
        input_sender,
        input_receiver,
        output_sender,
    } = prepared;
    let error_sender = output_sender.clone();
    let worker_permit = Arc::clone(&permit);
    tokio::task::spawn_blocking(move || {
        let _permit = worker_permit;
        if let Err(error) = write_streamed_zip(
            output_sender,
            input_receiver,
            projection.game,
            projection.challenges,
        ) {
            let _ = error_sender.blocking_send(Err(std::io::Error::other(error)));
        }
    });
    let pool = st.pg().clone();
    let storage = Arc::clone(&st.storage);
    let game_id = game.id;
    let title = game.title.clone();
    tokio::spawn(async move {
        let _permit = permit;
        stream_archive(
            pool,
            storage,
            game_id,
            title,
            attachments_bundled,
            boards,
            sources,
            input_sender,
        )
        .await;
    });
}

/// Collect a complete archive in memory. Used by tests and never by the
/// request path, which streams.
#[cfg(test)]
pub(super) async fn export_archive_bytes(
    st: &SharedState,
    game: &game::Model,
    bundle_attachments: bool,
) -> AppResult<Vec<u8>> {
    let permit = st
        .bulk_export_admission
        .try_acquire(Arc::clone(&st.cache), DATA_EXPORT_RETAINED_BYTES)
        .await
        .map_err(|_| AppError::unavailable("bulk export busy"))?;
    let (mut receiver, prepared) = prepare_archive(st, game, bundle_attachments).await?;
    start_archive(st, game, prepared, Arc::new(permit));
    let mut bytes = Vec::new();
    while let Some(chunk) = receiver.recv().await {
        bytes.extend_from_slice(&chunk.map_err(|error| AppError::internal(error.to_string()))?);
    }
    Ok(bytes)
}

fn merge_sources(
    attachments: Vec<ArchiveSource>,
    writeups: Vec<ArchiveSource>,
) -> AppResult<Vec<ArchiveSource>> {
    let mut by_hash = attachments
        .into_iter()
        .map(|source| (source.hash, source.size))
        .collect::<BTreeMap<_, _>>();
    for source in writeups {
        by_hash.entry(source.hash).or_insert(source.size);
    }
    let total = by_hash
        .values()
        .try_fold(0usize, |total, size| total.checked_add(*size))
        .filter(|total| *total <= MAX_DATA_EXPORT_BLOB_BYTES);
    if total.is_none() {
        return Err(AppError::payload_too_large(
            "Competition data export attachments and writeups exceed the 128 MiB limit",
        ));
    }
    Ok(by_hash
        .into_iter()
        .map(|(hash, size)| ArchiveSource { hash, size })
        .collect())
}

/// Every participation writeup blob, validated and bounded by count.
async fn with_writeup_sources(
    pool: &sqlx::PgPool,
    game_id: i32,
    mut sources: Vec<ArchiveSource>,
) -> AppResult<Vec<ArchiveSource>> {
    let rows = sqlx::query_as::<_, WriteupBlobRow>(
        r#"SELECT DISTINCT file.hash, file.file_size
             FROM "Participations" participation
             JOIN "Files" file ON file.id = participation.writeup_id
            WHERE participation.game_id = $1
            ORDER BY file.hash
            LIMIT $2"#,
    )
    .bind(game_id)
    .bind(i64::try_from(MAX_WRITEUP_FILES + 1).unwrap_or(i64::MAX))
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if rows.len() > MAX_WRITEUP_FILES {
        return Err(AppError::payload_too_large(format!(
            "Competition data export is limited to {MAX_WRITEUP_FILES} writeups"
        )));
    }
    for row in rows {
        let valid = row.hash.len() == 64 && row.hash.bytes().all(|byte| byte.is_ascii_hexdigit());
        if !valid {
            return Err(AppError::bad_request(
                "Writeup has an invalid stored content hash",
            ));
        }
        let size = usize::try_from(row.file_size)
            .map_err(|_| AppError::bad_request("Writeup has an invalid stored size"))?;
        sources.push(ArchiveSource {
            hash: row.hash,
            size,
        });
    }
    Ok(sources)
}

/// Render the monitor view of every applicable board. A board that cannot be
/// rendered is logged and omitted; the tables remain the source of truth.
async fn render_scoreboards(
    st: &SharedState,
    game: &game::Model,
    challenges: &[ExportChallengeModel],
) -> Vec<(String, Bytes)> {
    let mut boards = Vec::with_capacity(4);
    match crate::controllers::game::build_scoreboard_json(st, game, true).await {
        Ok(bytes) => boards.push(("scoreboards/jeopardy.json".to_string(), bytes)),
        Err(error) => {
            tracing::warn!(%error, game = game.id, "omitting jeopardy board from data archive")
        }
    }
    match crate::controllers::game::build_combined_scoreboard(st, game, true).await {
        Ok(board) => match serde_json::to_vec(&board) {
            Ok(bytes) => boards.push(("scoreboards/overall.json".to_string(), Bytes::from(bytes))),
            Err(error) => {
                tracing::warn!(%error, game = game.id, "omitting overall board from data archive")
            }
        },
        Err(error) => {
            tracing::warn!(%error, game = game.id, "omitting overall board from data archive")
        }
    }
    if challenges
        .iter()
        .any(|challenge| challenge.challenge_type.is_attack_defense())
    {
        match crate::controllers::game::ad::build_ad_scoreboard_cached(st, game.id, true).await {
            Ok(board) => match serde_json::to_vec(&board) {
                Ok(bytes) => boards.push((
                    "scoreboards/attack-defense.json".to_string(),
                    Bytes::from(bytes),
                )),
                Err(error) => {
                    tracing::warn!(%error, game = game.id, "omitting A&D board from data archive")
                }
            },
            Err(error) => {
                tracing::warn!(%error, game = game.id, "omitting A&D board from data archive")
            }
        }
    }
    if challenges
        .iter()
        .any(|challenge| challenge.challenge_type.is_king_of_the_hill())
    {
        match crate::controllers::game::koth::build_koth_scoreboard_cached(st, game, true).await {
            Ok(board) => match serde_json::to_vec(&board) {
                Ok(bytes) => boards.push(("scoreboards/koth.json".to_string(), Bytes::from(bytes))),
                Err(error) => {
                    tracing::warn!(%error, game = game.id, "omitting KotH board from data archive")
                }
            },
            Err(error) => {
                tracing::warn!(%error, game = game.id, "omitting KotH board from data archive")
            }
        }
    }
    boards
}

async fn send_sized(
    sender: &tokio::sync::mpsc::Sender<ArchiveInput>,
    entry: String,
    bytes: Bytes,
) -> Result<(), String> {
    let closed = |_| "client disconnected".to_string();
    sender
        .send(ArchiveInput::Start {
            entry,
            size: Some(bytes.len()),
        })
        .await
        .map_err(closed)?;
    sender
        .send(ArchiveInput::Chunk(bytes))
        .await
        .map_err(closed)?;
    sender.send(ArchiveInput::End).await.map_err(closed)
}

/// Producer side of the archive: boards, tables, bundled blobs, manifest.
#[allow(clippy::too_many_arguments)]
async fn stream_archive(
    pool: sqlx::PgPool,
    storage: Arc<dyn crate::storage::BlobStorage>,
    game_id: i32,
    title: String,
    attachments_bundled: bool,
    boards: Vec<(String, Bytes)>,
    sources: Vec<ArchiveSource>,
    sender: tokio::sync::mpsc::Sender<ArchiveInput>,
) {
    if let Err(error) = stream_archive_inner(
        &pool,
        storage,
        game_id,
        title,
        attachments_bundled,
        boards,
        sources,
        &sender,
    )
    .await
    {
        let _ = sender.send(ArchiveInput::Failed(error)).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn stream_archive_inner(
    pool: &sqlx::PgPool,
    storage: Arc<dyn crate::storage::BlobStorage>,
    game_id: i32,
    title: String,
    attachments_bundled: bool,
    boards: Vec<(String, Bytes)>,
    sources: Vec<ArchiveSource>,
    sender: &tokio::sync::mpsc::Sender<ArchiveInput>,
) -> Result<(), String> {
    for (entry, bytes) in boards {
        send_sized(sender, entry, bytes).await?;
    }
    let mut tables = Vec::new();
    for spec in catalog() {
        let rows = stream_table(pool, game_id, spec, sender).await?;
        tables.push(DataArchiveTable {
            name: spec.name.to_string(),
            file: spec.entry(),
            rows,
        });
    }
    forward_attachment_sources(storage, sources, sender.clone()).await;
    let manifest = DataArchiveManifest {
        kind: ARCHIVE_KIND.to_string(),
        format_version: ARCHIVE_FORMAT_VERSION,
        exported_at_utc: Utc::now(),
        source_game_id: game_id,
        title,
        platform_version: env!("CARGO_PKG_VERSION").to_string(),
        attachments_bundled,
        tables,
    };
    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("serialize manifest.json: {error}"))?;
    send_sized(sender, "manifest.json".to_string(), Bytes::from(bytes)).await
}

/// Stream one table as JSON Lines in bounded pages; returns the row count.
async fn stream_table(
    pool: &sqlx::PgPool,
    game_id: i32,
    spec: &TableSpec,
    sender: &tokio::sync::mpsc::Sender<ArchiveInput>,
) -> Result<u64, String> {
    let closed = |_| "client disconnected".to_string();
    let sql = spec.select_sql();
    sender
        .send(ArchiveInput::Start {
            entry: spec.entry(),
            size: None,
        })
        .await
        .map_err(closed)?;
    let mut offset = 0i64;
    let mut rows = 0u64;
    loop {
        let page = sqlx::query_scalar::<_, String>(&sql)
            .bind(game_id)
            .bind(DATA_EXPORT_PAGE_ROWS)
            .bind(offset)
            .fetch_all(pool)
            .await
            .map_err(|error| format!("{}: {error}", spec.name))?;
        if page.is_empty() {
            break;
        }
        rows += page.len() as u64;
        if rows > MAX_DATA_EXPORT_ROWS_PER_TABLE {
            return Err(format!(
                "{} exceeds the {MAX_DATA_EXPORT_ROWS_PER_TABLE} row export limit",
                spec.name
            ));
        }
        let mut buffer = Vec::with_capacity(page.iter().map(|line| line.len() + 1).sum());
        for line in &page {
            buffer.extend_from_slice(line.as_bytes());
            buffer.push(b'\n');
        }
        sender
            .send(ArchiveInput::Chunk(Bytes::from(buffer)))
            .await
            .map_err(closed)?;
        if (page.len() as i64) < DATA_EXPORT_PAGE_ROWS {
            break;
        }
        offset += DATA_EXPORT_PAGE_ROWS;
    }
    sender.send(ArchiveInput::End).await.map_err(closed)?;
    Ok(rows)
}
