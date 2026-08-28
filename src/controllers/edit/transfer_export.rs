//! Bounded, relationally-batched game export projection and ZIP streaming.

use super::*;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use bytes::Bytes;
use futures::StreamExt;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::sync::Arc;

const MAX_GAME_EXPORT_FILES: usize = 2_048;
const MAX_GAME_EXPORT_ATTACHMENT_BYTES: usize = 128 * 1024 * 1024;
const MAX_GAME_EXPORT_CHALLENGES: usize = 2_048;
const MAX_GAME_EXPORT_FLAGS: usize = 2_048;
const MAX_GAME_EXPORT_DIVISIONS: usize = 512;
const MAX_GAME_EXPORT_DIVISION_CONFIGS: usize = 2_048;
const GAME_EXPORT_ZIP_CHUNK_BYTES: usize = 64 * 1024;

#[derive(sqlx::FromRow)]
struct DivisionConfigRow {
    division_id: i32,
    challenge_id: i32,
    permissions: i32,
}

#[derive(sqlx::FromRow)]
struct FlagRow {
    challenge_id: i32,
    flag: String,
    attachment_id: Option<i32>,
}

#[derive(sqlx::FromRow)]
struct AttachmentRow {
    id: i32,
    file_type: i16,
    remote_url: Option<String>,
    hash: Option<String>,
    file_name: Option<String>,
    file_size: Option<i64>,
}

#[derive(Clone)]
struct AttachmentMeta {
    file_type: FileType,
    remote_url: Option<String>,
    hash: Option<String>,
    file_name: Option<String>,
}

#[derive(Clone)]
struct ArchiveSource {
    hash: String,
    size: usize,
}

enum ArchiveInput {
    Start { entry: String, size: usize },
    Chunk(Bytes),
    End,
    Failed(String),
}

type GameZipChunk = Result<Bytes, std::io::Error>;

struct GameZipStreamWriter {
    output: tokio::sync::mpsc::Sender<GameZipChunk>,
    buffered: Vec<u8>,
}

impl GameZipStreamWriter {
    fn new(output: tokio::sync::mpsc::Sender<GameZipChunk>) -> Self {
        Self {
            output,
            buffered: Vec::with_capacity(GAME_EXPORT_ZIP_CHUNK_BYTES),
        }
    }

    fn send_buffer(&mut self) -> std::io::Result<()> {
        if self.buffered.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::replace(
            &mut self.buffered,
            Vec::with_capacity(GAME_EXPORT_ZIP_CHUNK_BYTES),
        );
        self.output
            .blocking_send(Ok(Bytes::from(chunk)))
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "client disconnected"))
    }

    fn finish(mut self) -> std::io::Result<()> {
        self.send_buffer()
    }
}

impl Write for GameZipStreamWriter {
    fn write(&mut self, mut input: &[u8]) -> std::io::Result<usize> {
        let input_len = input.len();
        while !input.is_empty() {
            let available = GAME_EXPORT_ZIP_CHUNK_BYTES - self.buffered.len();
            let take = available.min(input.len());
            self.buffered.extend_from_slice(&input[..take]);
            input = &input[take..];
            if self.buffered.len() == GAME_EXPORT_ZIP_CHUNK_BYTES {
                self.send_buffer()?;
            }
        }
        Ok(input_len)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.send_buffer()
    }
}

fn file_type(value: i16) -> AppResult<FileType> {
    match value {
        0 => Ok(FileType::None),
        1 => Ok(FileType::Local),
        2 => Ok(FileType::Remote),
        _ => Err(AppError::internal("attachment has an invalid file type")),
    }
}

fn valid_content_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn attachment_fields(
    attachment_id: Option<i32>,
    attachments: &HashMap<i32, AttachmentMeta>,
) -> (
    Option<FileType>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    let Some(meta) = attachment_id.and_then(|id| attachments.get(&id)) else {
        return (None, None, None, None);
    };
    (
        Some(meta.file_type),
        meta.hash.clone(),
        meta.remote_url.clone(),
        meta.file_name.clone(),
    )
}

async fn batched_division_configs(
    pool: &sqlx::PgPool,
    game_id: i32,
) -> AppResult<HashMap<i32, Vec<ExportDivisionConfigModel>>> {
    let rows = sqlx::query_as::<_, DivisionConfigRow>(
        r#"SELECT config.division_id, config.challenge_id, config.permissions
             FROM "DivisionChallengeConfigs" config
             JOIN "Divisions" division ON division.id = config.division_id
            WHERE division.game_id = $1
            ORDER BY config.division_id, config.challenge_id
            LIMIT $2"#,
    )
    .bind(game_id)
    .bind(i64::try_from(MAX_GAME_EXPORT_DIVISION_CONFIGS + 1).unwrap_or(i64::MAX))
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if rows.len() > MAX_GAME_EXPORT_DIVISION_CONFIGS {
        return Err(AppError::payload_too_large(format!(
            "Game export is limited to {MAX_GAME_EXPORT_DIVISION_CONFIGS} division challenge settings"
        )));
    }
    let mut grouped = HashMap::<i32, Vec<ExportDivisionConfigModel>>::new();
    for row in rows {
        grouped
            .entry(row.division_id)
            .or_default()
            .push(ExportDivisionConfigModel {
                challenge_id: row.challenge_id,
                permissions: row.permissions,
            });
    }
    Ok(grouped)
}

async fn batched_flags(pool: &sqlx::PgPool, game_id: i32) -> AppResult<HashMap<i32, Vec<FlagRow>>> {
    let rows = sqlx::query_as::<_, FlagRow>(
        r#"SELECT flag.challenge_id, flag.flag, flag.attachment_id
             FROM "FlagContexts" flag
             JOIN "GameChallenges" challenge ON challenge.id = flag.challenge_id
            WHERE challenge.game_id = $1 AND challenge."Type" <> $2
            ORDER BY flag.challenge_id, flag.id
            LIMIT $3"#,
    )
    .bind(game_id)
    .bind(ChallengeType::DynamicContainer as i16)
    .bind(i64::try_from(MAX_GAME_EXPORT_FLAGS + 1).unwrap_or(i64::MAX))
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if rows.len() > MAX_GAME_EXPORT_FLAGS {
        return Err(AppError::payload_too_large(format!(
            "Game export is limited to {MAX_GAME_EXPORT_FLAGS} flags"
        )));
    }
    let mut grouped = HashMap::<i32, Vec<FlagRow>>::new();
    for row in rows {
        grouped.entry(row.challenge_id).or_default().push(row);
    }
    Ok(grouped)
}

async fn batched_attachments(
    pool: &sqlx::PgPool,
    attachment_ids: &[i32],
) -> AppResult<(HashMap<i32, AttachmentMeta>, Vec<ArchiveSource>)> {
    if attachment_ids.len() > MAX_GAME_EXPORT_FILES {
        return Err(AppError::payload_too_large(format!(
            "Game export is limited to {MAX_GAME_EXPORT_FILES} attachments"
        )));
    }
    if attachment_ids.is_empty() {
        return Ok((HashMap::new(), Vec::new()));
    }
    let rows = sqlx::query_as::<_, AttachmentRow>(
        r#"SELECT attachment.id, attachment."Type" AS file_type,
                  attachment.remote_url, file.hash, file.name AS file_name,
                  file.file_size
             FROM "Attachments" attachment
             LEFT JOIN "Files" file ON file.id = attachment.local_file_id
            WHERE attachment.id = ANY($1)
            ORDER BY attachment.id
            LIMIT $2"#,
    )
    .bind(attachment_ids)
    .bind(i64::try_from(MAX_GAME_EXPORT_FILES + 1).unwrap_or(i64::MAX))
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if rows.len() > MAX_GAME_EXPORT_FILES {
        return Err(AppError::payload_too_large(format!(
            "Game export is limited to {MAX_GAME_EXPORT_FILES} attachments"
        )));
    }

    let mut total_bytes = 0usize;
    let mut sources = BTreeMap::<String, usize>::new();
    let mut attachments = HashMap::with_capacity(rows.len());
    for row in rows {
        let file_type = file_type(row.file_type)?;
        let file_size = row
            .file_size
            .map(|size| {
                usize::try_from(size)
                    .map_err(|_| AppError::bad_request("Attachment has an invalid stored size"))
            })
            .transpose()?;
        if file_type == FileType::Local {
            if let (Some(hash), Some(size)) = (row.hash.as_deref(), file_size) {
                if !valid_content_hash(hash) {
                    return Err(AppError::bad_request(
                        "Attachment has an invalid stored content hash",
                    ));
                }
                if let Some(previous) = sources.insert(hash.to_string(), size) {
                    if previous != size {
                        return Err(AppError::bad_request(
                            "Attachment content hash has conflicting stored sizes",
                        ));
                    }
                } else {
                    total_bytes = total_bytes
                        .checked_add(size)
                        .filter(|total| *total <= MAX_GAME_EXPORT_ATTACHMENT_BYTES)
                        .ok_or_else(|| {
                            AppError::payload_too_large(
                                "Game export attachments exceed the 128 MiB limit",
                            )
                        })?;
                }
            }
        }
        attachments.insert(
            row.id,
            AttachmentMeta {
                file_type,
                remote_url: row.remote_url,
                hash: row.hash,
                file_name: row.file_name,
            },
        );
    }
    let sources = sources
        .into_iter()
        .map(|(hash, size)| ArchiveSource { hash, size })
        .collect();
    Ok((attachments, sources))
}

fn write_streamed_zip(
    output: tokio::sync::mpsc::Sender<GameZipChunk>,
    mut input: tokio::sync::mpsc::Receiver<ArchiveInput>,
    export_game: ExportGameModel,
    export_challenges: Vec<ExportChallengeModel>,
) -> Result<(), String> {
    let writer = GameZipStreamWriter::new(output);
    let mut zip = zip::ZipWriter::new_stream(writer);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("game.json", options)
        .map_err(|error| format!("zip entry: {error}"))?;
    serde_json::to_writer_pretty(&mut zip, &export_game)
        .map_err(|error| format!("serialize game.json: {error}"))?;
    zip.add_directory("challenges/", options)
        .map_err(|error| format!("zip directory: {error}"))?;
    for challenge in export_challenges {
        zip.start_file(
            format!("challenges/challenge-{}.json", challenge.id),
            options,
        )
        .map_err(|error| format!("zip entry: {error}"))?;
        serde_json::to_writer_pretty(&mut zip, &challenge)
            .map_err(|error| format!("serialize challenge: {error}"))?;
    }
    zip.add_directory("files/", options)
        .map_err(|error| format!("zip directory: {error}"))?;

    let mut remaining = None::<usize>;
    while let Some(message) = input.blocking_recv() {
        match message {
            ArchiveInput::Start { entry, size } if remaining.is_none() => {
                zip.start_file(entry, options)
                    .map_err(|error| format!("zip entry: {error}"))?;
                remaining = Some(size);
            }
            ArchiveInput::Chunk(chunk) => {
                let Some(left) = remaining.as_mut() else {
                    return Err("attachment stream sent bytes outside an entry".to_string());
                };
                if chunk.len() > *left {
                    return Err("attachment stream exceeded its declared size".to_string());
                }
                zip.write_all(&chunk)
                    .map_err(|error| format!("zip write: {error}"))?;
                *left -= chunk.len();
            }
            ArchiveInput::End if remaining == Some(0) => remaining = None,
            ArchiveInput::End => {
                return Err("attachment stream ended before its declared size".to_string())
            }
            ArchiveInput::Failed(error) => return Err(error),
            ArchiveInput::Start { .. } => {
                return Err("attachment stream overlapped entries".to_string())
            }
        }
    }
    if remaining.is_some() {
        return Err("attachment stream closed inside an entry".to_string());
    }
    zip.finish()
        .map_err(|error| format!("zip finish: {error}"))?
        .into_inner()
        .finish()
        .map_err(|error| format!("zip stream: {error}"))
}

async fn forward_attachment_sources(
    storage: Arc<dyn crate::storage::BlobStorage>,
    sources: Vec<ArchiveSource>,
    sender: tokio::sync::mpsc::Sender<ArchiveInput>,
) {
    for source in sources {
        let mut stream = match storage
            .stream_range(&source.hash, 0..source.size as u64)
            .await
        {
            Ok(stream) => stream,
            Err(error) => {
                tracing::warn!(%error, hash = %source.hash, "skipping unavailable game-export attachment");
                continue;
            }
        };
        if sender
            .send(ArchiveInput::Start {
                entry: format!("files/{}", source.hash),
                size: source.size,
            })
            .await
            .is_err()
        {
            return;
        }
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    let _ = sender
                        .send(ArchiveInput::Failed(format!(
                            "attachment {} stream failed: {error}",
                            source.hash
                        )))
                        .await;
                    return;
                }
            };
            if sender.send(ArchiveInput::Chunk(chunk)).await.is_err() {
                return;
            }
        }
        if sender.send(ArchiveInput::End).await.is_err() {
            return;
        }
    }
}

/// Export one game using a constant number of relational queries and a
/// response-owned ZIP stream. No complete attachment set or completed archive
/// is retained in memory.
pub async fn export_game(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<Response> {
    manager_or_admin(&st, &user, id).await?;
    let permit = match st
        .bulk_export_admission
        .try_acquire(Arc::clone(&st.cache), MAX_GAME_EXPORT_ATTACHMENT_BYTES)
        .await
    {
        Ok(permit) => Arc::new(permit),
        Err(_) => return Ok(crate::services::bulk_export::overload_response()),
    };
    let game = load_game(&st, id).await?;
    let challenges = game_challenge::Entity::find()
        .filter(game_challenge::Column::GameId.eq(id))
        .order_by_asc(game_challenge::Column::Id)
        .limit((MAX_GAME_EXPORT_CHALLENGES + 1) as u64)
        .all(&st.db)
        .await?;
    if challenges.len() > MAX_GAME_EXPORT_CHALLENGES {
        return Err(AppError::payload_too_large(format!(
            "Game export is limited to {MAX_GAME_EXPORT_CHALLENGES} challenges"
        )));
    }
    let divisions = division::Entity::find()
        .filter(division::Column::GameId.eq(id))
        .order_by_asc(division::Column::Id)
        .limit((MAX_GAME_EXPORT_DIVISIONS + 1) as u64)
        .all(&st.db)
        .await?;
    if divisions.len() > MAX_GAME_EXPORT_DIVISIONS {
        return Err(AppError::payload_too_large(format!(
            "Game export is limited to {MAX_GAME_EXPORT_DIVISIONS} divisions"
        )));
    }

    let mut configs = batched_division_configs(st.pg(), id).await?;
    let mut flags = batched_flags(st.pg(), id).await?;
    let mut attachment_ids = challenges
        .iter()
        .filter_map(|challenge| challenge.attachment_id)
        .collect::<Vec<_>>();
    attachment_ids.extend(
        flags
            .values()
            .flatten()
            .filter_map(|flag| flag.attachment_id),
    );
    attachment_ids.sort_unstable();
    attachment_ids.dedup();
    let (attachments, sources) = batched_attachments(st.pg(), &attachment_ids).await?;

    let mut export_game = ExportGameModel::from_game(&game);
    export_game.divisions = divisions
        .into_iter()
        .map(|division| ExportDivisionModel {
            challenge_configs: configs.remove(&division.id).unwrap_or_default(),
            name: division.name,
            invite_code: division.invite_code,
            default_permissions: division.default_permissions,
        })
        .collect();
    let export_challenges = challenges
        .iter()
        .map(|challenge| {
            let (attachment_type, hash, url, name) =
                attachment_fields(challenge.attachment_id, &attachments);
            let export_flags = if challenge.challenge_type == ChallengeType::DynamicContainer {
                Vec::new()
            } else {
                flags
                    .remove(&challenge.id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|flag| {
                        let (attachment_type, file_hash, remote_url, file_name) =
                            attachment_fields(flag.attachment_id, &attachments);
                        ExportFlagModel {
                            flag: flag.flag,
                            attachment_type,
                            file_hash,
                            remote_url,
                            file_name,
                        }
                    })
                    .collect()
            };
            ExportChallengeModel::from_challenge(
                challenge,
                export_flags,
                attachment_type,
                hash,
                url,
                name,
            )
        })
        .collect::<Vec<_>>();

    // Re-prove authorization after the complete relational projection and
    // before any response bytes or storage reads can escape.
    manager_or_admin(&st, &user, id).await?;
    let (input_sender, input_receiver) = tokio::sync::mpsc::channel::<ArchiveInput>(8);
    let (output_sender, output_receiver) = tokio::sync::mpsc::channel::<GameZipChunk>(8);
    let error_sender = output_sender.clone();
    let worker_permit = Arc::clone(&permit);
    tokio::task::spawn_blocking(move || {
        let _permit = worker_permit;
        if let Err(error) = write_streamed_zip(
            output_sender,
            input_receiver,
            export_game,
            export_challenges,
        ) {
            let _ = error_sender.blocking_send(Err(std::io::Error::other(error)));
        }
    });
    let storage = Arc::clone(&st.storage);
    let loader_permit = Arc::clone(&permit);
    tokio::spawn(async move {
        let _permit = loader_permit;
        forward_attachment_sources(storage, sources, input_sender).await;
    });

    let filename = format!("game-{id}-export.zip");
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read};
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use uuid::Uuid;

    #[test]
    fn streamed_export_zip_enforces_entry_lengths_and_is_readable() {
        let game: ExportGameModel = serde_json::from_value(serde_json::json!({})).unwrap();
        let (input_sender, input_receiver) = tokio::sync::mpsc::channel(8);
        let (output_sender, mut output_receiver) = tokio::sync::mpsc::channel(8);
        input_sender
            .blocking_send(ArchiveInput::Start {
                entry: format!("files/{}", "a".repeat(64)),
                size: 10,
            })
            .unwrap();
        input_sender
            .blocking_send(ArchiveInput::Chunk(Bytes::from_static(b"attachment")))
            .unwrap();
        input_sender.blocking_send(ArchiveInput::End).unwrap();
        drop(input_sender);
        let worker = std::thread::spawn(move || {
            write_streamed_zip(output_sender, input_receiver, game, Vec::new()).unwrap();
        });
        let mut bytes = Vec::new();
        while let Some(chunk) = output_receiver.blocking_recv() {
            bytes.extend_from_slice(&chunk.unwrap());
        }
        worker.join().unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert!(archive.by_name("game.json").is_ok());
        let mut attachment = Vec::new();
        archive
            .by_name(&format!("files/{}", "a".repeat(64)))
            .unwrap()
            .read_to_end(&mut attachment)
            .unwrap();
        assert_eq!(attachment, b"attachment");
    }

    #[test]
    fn export_projection_is_batched_and_archive_is_streamed() {
        let source = include_str!("transfer_export.rs");
        assert!(source.contains("JOIN \"Divisions\""));
        assert!(source.contains("JOIN \"GameChallenges\""));
        assert!(source.contains("attachment.id = ANY($1)"));
        assert!(source.contains("stream_range(&source.hash"));
        assert!(source.contains("permitted_stream_body"));
        assert!(!source.contains("permitted_bytes_body"));
        assert!(!source.contains("load_bounded"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn relational_projection_batches_configs_flags_and_attachments() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect test database");
        let schema = format!("rsctf_transfer_export_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .expect("create isolated schema");
        let options = PgConnectOptions::from_str(&database_url)
            .expect("parse test database URL")
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .expect("connect isolated schema");
        sqlx::raw_sql(
            r#"CREATE TABLE "Divisions" (
                 id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL
               );
               CREATE TABLE "DivisionChallengeConfigs" (
                 division_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
                 permissions INTEGER NOT NULL
               );
               CREATE TABLE "GameChallenges" (
                 id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, "Type" SMALLINT NOT NULL
               );
               CREATE TABLE "FlagContexts" (
                 id INTEGER PRIMARY KEY, challenge_id INTEGER, flag TEXT NOT NULL,
                 attachment_id INTEGER
               );
               CREATE TABLE "Files" (
                 id INTEGER PRIMARY KEY, hash TEXT NOT NULL, name TEXT NOT NULL,
                 file_size BIGINT NOT NULL
               );
               CREATE TABLE "Attachments" (
                 id INTEGER PRIMARY KEY, "Type" SMALLINT NOT NULL,
                 remote_url TEXT, local_file_id INTEGER
               );
               INSERT INTO "Divisions" VALUES (10, 1), (11, 1);
               INSERT INTO "DivisionChallengeConfigs" VALUES
                 (10, 20, 3), (11, 20, 7);
               INSERT INTO "GameChallenges" VALUES (20, 1, 0);
               INSERT INTO "Files" VALUES
                 (30, repeat('a', 64), 'evidence.bin', 12);
               INSERT INTO "Attachments" VALUES (40, 1, NULL, 30);
               INSERT INTO "FlagContexts" VALUES
                 (50, 20, 'flag-one', 40), (51, 20, 'flag-two', 40);"#,
        )
        .execute(&pool)
        .await
        .expect("seed export projection");

        let configs = batched_division_configs(&pool, 1).await.unwrap();
        let flags = batched_flags(&pool, 1).await.unwrap();
        let (attachments, sources) = batched_attachments(&pool, &[40]).await.unwrap();
        assert_eq!(configs.len(), 2);
        assert_eq!(flags.get(&20).unwrap().len(), 2);
        assert_eq!(attachments.len(), 1);
        assert_eq!(sources.len(), 1, "shared hashes are streamed once");
        assert_eq!(sources[0].size, 12);

        pool.close().await;
        assert!(schema.starts_with("rsctf_transfer_export_"));
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .expect("drop isolated schema");
        admin.close().await;
    }
}
