//! Durable traffic-capture inventory and bounded monitor read models.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use chrono::{DateTime, Utc};
use sea_orm::ActiveEnum;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};

use crate::utils::enums::{ChallengeCategory, ChallengeType};
use crate::utils::error::{AppError, AppResult};

mod reconcile;

const DEFAULT_PAGE_SIZE: u64 = 50;
const MAX_PAGE_SIZE: u64 = 100;
const MAX_ARCHIVE_FILES: u64 = 256;
const INVENTORY_LOCK_NAMESPACE: i32 = 1_414_675_849;
const INVENTORY_LOCK_KEY: i32 = 1;
const INVENTORY_QUEUE_CAPACITY: usize = 512;
const INVENTORY_MUTATION_TIMEOUT: Duration = Duration::from_secs(5);
static INVENTORY_READ_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturePageQuery {
    #[serde(default = "default_page_size")]
    pub count: u64,
    #[serde(default)]
    pub cursor: Option<String>,
}

const fn default_page_size() -> u64 {
    DEFAULT_PAGE_SIZE
}

impl Default for CapturePageQuery {
    fn default() -> Self {
        Self {
            count: DEFAULT_PAGE_SIZE,
            cursor: None,
        }
    }
}

impl CapturePageQuery {
    pub(crate) fn capped(count: u64) -> Self {
        Self {
            count: count.clamp(1, MAX_PAGE_SIZE),
            cursor: None,
        }
    }

    fn limit(&self) -> i64 {
        i64::try_from(self.count.clamp(1, MAX_PAGE_SIZE)).unwrap_or(MAX_PAGE_SIZE as i64)
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturePage<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeCaptureItem {
    pub id: i32,
    pub title: String,
    pub category: ChallengeCategory,
    #[serde(rename = "type")]
    pub challenge_type: ChallengeType,
    pub is_enabled: bool,
    pub count: i64,
    pub size: i64,
    pub update_time: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamCaptureItem {
    pub id: i32,
    pub team_id: i32,
    pub name: String,
    pub division: Option<String>,
    pub avatar: Option<String>,
    pub count: i64,
    pub size: i64,
    pub update_time: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureFileItem {
    pub file_name: String,
    pub size: i64,
    pub update_time: i64,
}

#[derive(Clone, Debug)]
pub(super) struct InventoryFile {
    challenge_id: i32,
    participation_id: i32,
    file_name: String,
    size_bytes: i64,
    modified_at: DateTime<Utc>,
}

impl InventoryFile {
    pub(super) fn from_path(
        challenge_id: i32,
        participation_id: i32,
        path: &Path,
    ) -> AppResult<Self> {
        validate_ids(challenge_id, participation_id)?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| valid_capture_name(name))
            .ok_or_else(|| AppError::internal("capture inventory received an invalid file name"))?
            .to_string();
        let metadata = std::fs::symlink_metadata(path).map_err(|error| {
            AppError::internal(format!(
                "failed to inspect capture file {}: {error}",
                path.display()
            ))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(AppError::internal(format!(
                "capture inventory path is not a regular file: {}",
                path.display()
            )));
        }
        let size_bytes = i64::try_from(metadata.len())
            .map_err(|_| AppError::internal("capture file size exceeds PostgreSQL bigint"))?;
        let modified_at = metadata
            .modified()
            .map(DateTime::<Utc>::from)
            .map_err(|error| {
                AppError::internal(format!(
                    "failed to read capture modification time {}: {error}",
                    path.display()
                ))
            })?;
        Ok(Self {
            challenge_id,
            participation_id,
            file_name,
            size_bytes,
            modified_at,
        })
    }
}

#[derive(Clone)]
pub(super) struct CaptureInventoryReporter {
    queue: CaptureInventoryQueue,
    challenge_id: i32,
    participation_id: i32,
}

impl CaptureInventoryReporter {
    pub(super) fn new(
        queue: CaptureInventoryQueue,
        challenge_id: i32,
        participation_id: i32,
    ) -> Self {
        Self {
            queue,
            challenge_id,
            participation_id,
        }
    }

    pub(super) fn upsert_path(&self, path: &Path) {
        match InventoryFile::from_path(self.challenge_id, self.participation_id, path) {
            Ok(file) => self.queue.try_send(InventoryMutation::Upsert(file)),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "capture inventory snapshot failed");
                self.queue.mark_dirty();
            }
        }
    }

    pub(super) fn delete_paths(&self, paths: &[PathBuf]) {
        let names = paths
            .iter()
            .filter_map(|path| path.file_name().and_then(|name| name.to_str()))
            .filter(|name| valid_capture_name(name))
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if names.is_empty() {
            return;
        }
        self.queue.try_send(InventoryMutation::DeleteFiles {
            challenge_id: self.challenge_id,
            participation_id: self.participation_id,
            file_names: names,
        });
    }
}

enum InventoryMutation {
    Upsert(InventoryFile),
    DeleteFiles {
        challenge_id: i32,
        participation_id: i32,
        file_names: Vec<String>,
    },
}

#[derive(Clone)]
pub(super) struct CaptureInventoryQueue {
    sender: tokio::sync::mpsc::Sender<InventoryMutation>,
    dirty: Arc<AtomicBool>,
}

impl CaptureInventoryQueue {
    fn try_send(&self, mutation: InventoryMutation) {
        if let Err(error) = self.sender.try_send(mutation) {
            self.mark_dirty();
            tracing::warn!(%error, "capture inventory queue overflowed; scheduling reconciliation");
        }
    }

    fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
    }
}

pub(super) fn start_mutation_worker(
    pool: PgPool,
) -> (CaptureInventoryQueue, tokio::task::JoinHandle<()>) {
    let (sender, receiver) = tokio::sync::mpsc::channel(INVENTORY_QUEUE_CAPACITY);
    let dirty = Arc::new(AtomicBool::new(false));
    let queue = CaptureInventoryQueue {
        sender,
        dirty: dirty.clone(),
    };
    let worker = tokio::spawn(run_mutation_worker(pool, receiver, dirty));
    (queue, worker)
}

async fn run_mutation_worker(
    pool: PgPool,
    mut receiver: tokio::sync::mpsc::Receiver<InventoryMutation>,
    dirty: Arc<AtomicBool>,
) {
    let mut dirty_tick = tokio::time::interval(Duration::from_secs(1));
    dirty_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            mutation = receiver.recv() => {
                let Some(mutation) = mutation else {
                    break;
                };
                let result = tokio::time::timeout(
                    INVENTORY_MUTATION_TIMEOUT,
                    apply_mutation(&pool, mutation),
                )
                .await;
                if !matches!(result, Ok(Ok(()))) {
                    dirty.store(true, Ordering::Release);
                    tracing::warn!("capture inventory mutation failed; scheduling reconciliation");
                }
            }
            _ = dirty_tick.tick() => {
                persist_dirty_marker(&pool, &dirty).await;
            }
        }
    }
    persist_dirty_marker(&pool, &dirty).await;
}

async fn apply_mutation(pool: &PgPool, mutation: InventoryMutation) -> AppResult<()> {
    match mutation {
        InventoryMutation::Upsert(file) => upsert_files(pool, &[file]).await,
        InventoryMutation::DeleteFiles {
            challenge_id,
            participation_id,
            file_names,
        } => delete_file_names(pool, challenge_id, participation_id, &file_names).await,
    }
}

async fn persist_dirty_marker(pool: &PgPool, dirty: &AtomicBool) {
    if !dirty.swap(false, Ordering::AcqRel) {
        return;
    }
    let result =
        tokio::time::timeout(INVENTORY_MUTATION_TIMEOUT, mark_reconcile_required(pool)).await;
    if !matches!(result, Ok(Ok(()))) {
        dirty.store(true, Ordering::Release);
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct IdCursor {
    version: u8,
    time_micros: i64,
    id: i32,
}

#[derive(Debug, Serialize, Deserialize)]
struct FileCursor {
    version: u8,
    time_micros: i64,
    file_name: String,
}

#[derive(sqlx::FromRow)]
struct ChallengeCaptureRow {
    id: i32,
    title: String,
    category: i16,
    challenge_type: i16,
    is_enabled: bool,
    file_count: i64,
    total_bytes: i64,
    latest_modified_at: Option<DateTime<Utc>>,
    sort_micros: i64,
}

#[derive(sqlx::FromRow)]
struct TeamCaptureRow {
    participation_id: i32,
    team_id: i32,
    name: String,
    division: Option<String>,
    avatar_hash: Option<String>,
    file_count: i32,
    total_bytes: i64,
    latest_modified_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct CaptureFileRow {
    file_name: String,
    size_bytes: i64,
    modified_at: DateTime<Utc>,
}

pub(crate) async fn challenge_page(
    pool: &PgPool,
    capture_root: &Path,
    game_id: i32,
    query: &CapturePageQuery,
) -> AppResult<CapturePage<ChallengeCaptureItem>> {
    reconcile::ensure_reconciled(pool, capture_root).await?;
    let _permit = read_permit()?;
    let cursor = decode_optional_cursor::<IdCursor>(query.cursor.as_deref())?;
    let cursor_time = cursor.as_ref().map(|cursor| cursor.time_micros);
    let cursor_id = cursor.as_ref().map(|cursor| cursor.id);
    let limit = query.limit();
    let mut rows = sqlx::query_as::<_, ChallengeCaptureRow>(
        r#"SELECT challenge.id,
                  challenge.title,
                  challenge.category,
                  challenge."Type" AS challenge_type,
                  challenge.is_enabled,
                  COALESCE(summary.file_count, 0)::BIGINT AS file_count,
                  COALESCE(summary.total_bytes, 0)::BIGINT AS total_bytes,
                  summary.latest_modified_at,
                  COALESCE(
                      (EXTRACT(EPOCH FROM summary.latest_modified_at) * 1000000)::BIGINT,
                      -1
                  ) AS sort_micros
             FROM "GameChallenges" challenge
             LEFT JOIN LATERAL (
                 SELECT SUM(bucket.file_count)::BIGINT AS file_count,
                        SUM(bucket.total_bytes)::BIGINT AS total_bytes,
                        MAX(bucket.latest_modified_at_utc) AS latest_modified_at
                   FROM "TrafficCaptureBuckets" bucket
                   JOIN "Participations" participation
                     ON participation.id = bucket.participation_id
                    AND participation.game_id = challenge.game_id
                  WHERE bucket.challenge_id = challenge.id
                    AND bucket.file_count > 0
             ) summary ON TRUE
            WHERE challenge.game_id = $1
              AND challenge.enable_traffic_capture = TRUE
              AND (
                  $2::BIGINT IS NULL
                  OR (
                      COALESCE(
                          (EXTRACT(EPOCH FROM summary.latest_modified_at) * 1000000)::BIGINT,
                          -1
                      ),
                      challenge.id
                  ) < ($2, $3)
              )
            ORDER BY sort_micros DESC, challenge.id DESC
            LIMIT $4"#,
    )
    .bind(game_id)
    .bind(cursor_time)
    .bind(cursor_id)
    .bind(limit + 1)
    .fetch_all(pool)
    .await
    .map_err(database_error)?;

    let has_more = rows.len() > limit as usize;
    if has_more {
        rows.pop();
    }
    let next_cursor = has_more.then(|| {
        let row = rows
            .last()
            .expect("a capped page with an extra row is non-empty");
        encode_cursor(&IdCursor {
            version: 1,
            time_micros: row.sort_micros,
            id: row.id,
        })
    });
    let items = rows
        .into_iter()
        .map(|row| {
            Ok(ChallengeCaptureItem {
                id: row.id,
                title: row.title,
                category: <ChallengeCategory as ActiveEnum>::try_from_value(&row.category)
                    .map_err(|error| AppError::internal(error.to_string()))?,
                challenge_type: <ChallengeType as ActiveEnum>::try_from_value(&row.challenge_type)
                    .map_err(|error| AppError::internal(error.to_string()))?,
                is_enabled: row.is_enabled,
                count: row.file_count,
                size: row.total_bytes,
                update_time: row
                    .latest_modified_at
                    .map_or(0, |value| value.timestamp_millis()),
            })
        })
        .collect::<AppResult<Vec<_>>>()?;
    Ok(CapturePage { items, next_cursor })
}

pub(crate) async fn team_page(
    pool: &PgPool,
    capture_root: &Path,
    challenge_id: i32,
    query: &CapturePageQuery,
) -> AppResult<CapturePage<TeamCaptureItem>> {
    reconcile::ensure_reconciled(pool, capture_root).await?;
    let _permit = read_permit()?;
    let cursor = decode_optional_cursor::<IdCursor>(query.cursor.as_deref())?;
    let cursor_time = cursor
        .as_ref()
        .map(|cursor| cursor_timestamp(cursor.time_micros))
        .transpose()?;
    let cursor_id = cursor.as_ref().map(|cursor| cursor.id);
    let limit = query.limit();
    let mut rows = sqlx::query_as::<_, TeamCaptureRow>(
        r#"SELECT bucket.participation_id,
                  participation.team_id,
                  team.name,
                  division.name AS division,
                  team.avatar_hash,
                  bucket.file_count,
                  bucket.total_bytes,
                  bucket.latest_modified_at_utc AS latest_modified_at
             FROM "TrafficCaptureBuckets" bucket
             JOIN "GameChallenges" challenge
               ON challenge.id = bucket.challenge_id
             JOIN "Participations" participation
               ON participation.id = bucket.participation_id
              AND participation.game_id = challenge.game_id
             JOIN "Teams" team
               ON team.id = participation.team_id
             LEFT JOIN "Divisions" division
               ON division.id = participation.division_id
              AND division.game_id = challenge.game_id
            WHERE bucket.challenge_id = $1
              AND challenge.enable_traffic_capture = TRUE
              AND bucket.file_count > 0
              AND bucket.latest_modified_at_utc IS NOT NULL
              AND (
                  $2::TIMESTAMPTZ IS NULL
                  OR (bucket.latest_modified_at_utc, bucket.participation_id) < ($2, $3)
              )
            ORDER BY bucket.latest_modified_at_utc DESC, bucket.participation_id DESC
            LIMIT $4"#,
    )
    .bind(challenge_id)
    .bind(cursor_time)
    .bind(cursor_id)
    .bind(limit + 1)
    .fetch_all(pool)
    .await
    .map_err(database_error)?;

    let has_more = rows.len() > limit as usize;
    if has_more {
        rows.pop();
    }
    let next_cursor = has_more.then(|| {
        let row = rows
            .last()
            .expect("a capped page with an extra row is non-empty");
        encode_cursor(&IdCursor {
            version: 1,
            time_micros: row.latest_modified_at.timestamp_micros(),
            id: row.participation_id,
        })
    });
    let items = rows
        .into_iter()
        .map(|row| TeamCaptureItem {
            id: row.participation_id,
            team_id: row.team_id,
            name: row.name,
            division: row.division,
            avatar: row.avatar_hash.map(|hash| format!("/assets/{hash}/avatar")),
            count: i64::from(row.file_count),
            size: row.total_bytes,
            update_time: row.latest_modified_at.timestamp_millis(),
        })
        .collect();
    Ok(CapturePage { items, next_cursor })
}

pub(crate) async fn file_page(
    pool: &PgPool,
    capture_root: &Path,
    challenge_id: i32,
    participation_id: i32,
    query: &CapturePageQuery,
) -> AppResult<CapturePage<CaptureFileItem>> {
    validate_ids(challenge_id, participation_id)?;
    reconcile::ensure_reconciled(pool, capture_root).await?;
    let _permit = read_permit()?;
    let cursor = decode_optional_cursor::<FileCursor>(query.cursor.as_deref())?;
    if cursor
        .as_ref()
        .is_some_and(|cursor| !valid_capture_name(&cursor.file_name))
    {
        return Err(AppError::bad_request("Invalid capture inventory cursor"));
    }
    let cursor_time = cursor
        .as_ref()
        .map(|cursor| cursor_timestamp(cursor.time_micros))
        .transpose()?;
    let cursor_name = cursor.as_ref().map(|cursor| cursor.file_name.as_str());
    let limit = query.limit();
    let mut rows = sqlx::query_as::<_, CaptureFileRow>(
        r#"SELECT file_name, size_bytes, modified_at_utc AS modified_at
             FROM "TrafficCaptureFiles"
            WHERE challenge_id = $1
              AND participation_id = $2
              AND (
                  $3::TIMESTAMPTZ IS NULL
                  OR (modified_at_utc, file_name) < ($3, $4)
              )
            ORDER BY modified_at_utc DESC, file_name DESC
            LIMIT $5"#,
    )
    .bind(challenge_id)
    .bind(participation_id)
    .bind(cursor_time)
    .bind(cursor_name)
    .bind(limit + 1)
    .fetch_all(pool)
    .await
    .map_err(database_error)?;

    let has_more = rows.len() > limit as usize;
    if has_more {
        rows.pop();
    }
    let next_cursor = has_more.then(|| {
        let row = rows
            .last()
            .expect("a capped page with an extra row is non-empty");
        encode_cursor(&FileCursor {
            version: 1,
            time_micros: row.modified_at.timestamp_micros(),
            file_name: row.file_name.clone(),
        })
    });
    let items = rows
        .into_iter()
        .map(|row| CaptureFileItem {
            file_name: row.file_name,
            size: row.size_bytes,
            update_time: row.modified_at.timestamp_millis(),
        })
        .collect();
    Ok(CapturePage { items, next_cursor })
}

pub(crate) async fn archive_file_names(
    pool: &PgPool,
    capture_root: &Path,
    challenge_id: i32,
    participation_id: i32,
) -> AppResult<Vec<String>> {
    validate_ids(challenge_id, participation_id)?;
    reconcile::ensure_reconciled(pool, capture_root).await?;
    let _permit = read_permit()?;
    let rows = sqlx::query_scalar::<_, String>(
        r#"SELECT file_name
             FROM "TrafficCaptureFiles"
            WHERE challenge_id = $1 AND participation_id = $2
            ORDER BY modified_at_utc DESC, file_name DESC
            LIMIT $3"#,
    )
    .bind(challenge_id)
    .bind(participation_id)
    .bind(i64::try_from(MAX_ARCHIVE_FILES + 1).unwrap_or(257))
    .fetch_all(pool)
    .await
    .map_err(database_error)?;
    if rows.len() > MAX_ARCHIVE_FILES as usize {
        return Err(AppError::bad_request(
            "Too many captures to archive; download them individually",
        ));
    }
    Ok(rows)
}

pub(crate) async fn delete_file(
    pool: &PgPool,
    challenge_id: i32,
    participation_id: i32,
    file_name: &str,
) -> AppResult<()> {
    if !valid_capture_name(file_name) {
        return Err(AppError::bad_request("Invalid capture file name"));
    }
    delete_file_names(
        pool,
        challenge_id,
        participation_id,
        &[file_name.to_string()],
    )
    .await
}

pub(crate) async fn delete_bucket(
    pool: &PgPool,
    challenge_id: i32,
    participation_id: i32,
) -> AppResult<()> {
    validate_ids(challenge_id, participation_id)?;
    let mut transaction = locked_transaction(pool).await?;
    sqlx::query(
        r#"DELETE FROM "TrafficCaptureFiles"
            WHERE challenge_id = $1 AND participation_id = $2"#,
    )
    .bind(challenge_id)
    .bind(participation_id)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    sqlx::query(
        r#"DELETE FROM "TrafficCaptureBuckets"
            WHERE challenge_id = $1 AND participation_id = $2"#,
    )
    .bind(challenge_id)
    .bind(participation_id)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    commit(transaction).await
}

pub(crate) async fn delete_challenges(pool: &PgPool, challenge_ids: &[i32]) -> AppResult<()> {
    if challenge_ids.is_empty() {
        return Ok(());
    }
    if challenge_ids.iter().any(|id| *id <= 0) {
        return Err(AppError::internal(
            "capture inventory received an invalid challenge id",
        ));
    }
    let mut transaction = locked_transaction(pool).await?;
    sqlx::query(r#"DELETE FROM "TrafficCaptureFiles" WHERE challenge_id = ANY($1)"#)
        .bind(challenge_ids)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    sqlx::query(r#"DELETE FROM "TrafficCaptureBuckets" WHERE challenge_id = ANY($1)"#)
        .bind(challenge_ids)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    commit(transaction).await
}

pub(crate) async fn mark_reconcile_required(pool: &PgPool) -> AppResult<()> {
    let mut transaction = locked_transaction(pool).await?;
    let updated = sqlx::query(
        r#"UPDATE "TrafficCaptureInventoryState"
              SET reconciled_at_utc = NULL,
                  updated_at_utc = clock_timestamp()
            WHERE singleton = TRUE"#,
    )
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    if updated.rows_affected() != 1 {
        return Err(AppError::internal(
            "traffic capture inventory state row is missing",
        ));
    }
    commit(transaction).await
}

pub(crate) async fn mark_reconcile_required_after_failure(pool: &PgPool) {
    let result =
        tokio::time::timeout(INVENTORY_MUTATION_TIMEOUT, mark_reconcile_required(pool)).await;
    if !matches!(result, Ok(Ok(()))) {
        tracing::error!(
            "traffic capture inventory could not persist its reconciliation-required marker"
        );
    }
}

async fn delete_file_names(
    pool: &PgPool,
    challenge_id: i32,
    participation_id: i32,
    file_names: &[String],
) -> AppResult<()> {
    validate_ids(challenge_id, participation_id)?;
    if file_names.iter().any(|name| !valid_capture_name(name)) {
        return Err(AppError::internal(
            "capture inventory received an invalid file name",
        ));
    }
    let mut transaction = locked_transaction(pool).await?;
    sqlx::query(
        r#"DELETE FROM "TrafficCaptureFiles"
            WHERE challenge_id = $1
              AND participation_id = $2
              AND file_name = ANY($3)"#,
    )
    .bind(challenge_id)
    .bind(participation_id)
    .bind(file_names)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    commit(transaction).await
}

async fn upsert_files(pool: &PgPool, files: &[InventoryFile]) -> AppResult<()> {
    if files.is_empty() {
        return Ok(());
    }
    let mut transaction = locked_transaction(pool).await?;
    upsert_files_in(&mut transaction, files).await?;
    commit(transaction).await
}

pub(super) async fn upsert_files_in(
    transaction: &mut Transaction<'_, Postgres>,
    files: &[InventoryFile],
) -> AppResult<()> {
    if files.is_empty() {
        return Ok(());
    }
    let challenge_ids = files
        .iter()
        .map(|file| file.challenge_id)
        .collect::<Vec<_>>();
    let participation_ids = files
        .iter()
        .map(|file| file.participation_id)
        .collect::<Vec<_>>();
    let file_names = files
        .iter()
        .map(|file| file.file_name.clone())
        .collect::<Vec<_>>();
    let size_bytes = files.iter().map(|file| file.size_bytes).collect::<Vec<_>>();
    let modified_at = files
        .iter()
        .map(|file| file.modified_at)
        .collect::<Vec<_>>();
    sqlx::query(
        r#"INSERT INTO "TrafficCaptureFiles"
               (challenge_id, participation_id, file_name, size_bytes, modified_at_utc)
           SELECT input.challenge_id, input.participation_id, input.file_name,
                  input.size_bytes, input.modified_at_utc
             FROM UNNEST(
                 $1::INTEGER[], $2::INTEGER[], $3::TEXT[], $4::BIGINT[], $5::TIMESTAMPTZ[]
             ) AS input(
                 challenge_id, participation_id, file_name, size_bytes, modified_at_utc
             )
           ON CONFLICT (challenge_id, participation_id, file_name) DO UPDATE
             SET size_bytes = GREATEST("TrafficCaptureFiles".size_bytes, EXCLUDED.size_bytes),
                 modified_at_utc = GREATEST(
                     "TrafficCaptureFiles".modified_at_utc,
                     EXCLUDED.modified_at_utc
                 )
           WHERE ("TrafficCaptureFiles".size_bytes, "TrafficCaptureFiles".modified_at_utc)
                 IS DISTINCT FROM (
                     GREATEST("TrafficCaptureFiles".size_bytes, EXCLUDED.size_bytes),
                     GREATEST(
                         "TrafficCaptureFiles".modified_at_utc,
                         EXCLUDED.modified_at_utc
                     )
                 )"#,
    )
    .bind(&challenge_ids)
    .bind(&participation_ids)
    .bind(&file_names)
    .bind(&size_bytes)
    .bind(&modified_at)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

pub(super) async fn locked_transaction(pool: &PgPool) -> AppResult<Transaction<'_, Postgres>> {
    let mut transaction = pool.begin().await.map_err(database_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
        .bind(INVENTORY_LOCK_NAMESPACE)
        .bind(INVENTORY_LOCK_KEY)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    Ok(transaction)
}

pub(super) async fn commit(transaction: Transaction<'_, Postgres>) -> AppResult<()> {
    transaction.commit().await.map_err(database_error)
}

fn read_permit() -> AppResult<tokio::sync::SemaphorePermit<'static>> {
    INVENTORY_READ_SLOTS
        .try_acquire()
        .map_err(|_| AppError::unavailable("Capture inventory capacity is busy; retry shortly"))
}

fn validate_ids(challenge_id: i32, participation_id: i32) -> AppResult<()> {
    if challenge_id <= 0 || participation_id <= 0 {
        return Err(AppError::bad_request("Invalid capture inventory scope"));
    }
    Ok(())
}

pub(super) fn valid_capture_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && !name.chars().any(char::is_control)
        && name
            .rsplit_once('.')
            .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("pcap"))
}

fn encode_cursor<T: Serialize>(cursor: &T) -> String {
    let encoded = serde_json::to_vec(cursor).expect("traffic cursor serialization cannot fail");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(encoded)
}

fn decode_optional_cursor<T: DeserializeOwned>(cursor: Option<&str>) -> AppResult<Option<T>> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    if cursor.is_empty() || cursor.len() > 1_024 {
        return Err(AppError::bad_request("Invalid capture inventory cursor"));
    }
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| AppError::bad_request("Invalid capture inventory cursor"))?;
    let value: serde_json::Value = serde_json::from_slice(&decoded)
        .map_err(|_| AppError::bad_request("Invalid capture inventory cursor"))?;
    if value.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
        return Err(AppError::bad_request(
            "Unsupported capture inventory cursor",
        ));
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(|_| AppError::bad_request("Invalid capture inventory cursor"))
}

fn cursor_timestamp(micros: i64) -> AppResult<DateTime<Utc>> {
    DateTime::from_timestamp_micros(micros)
        .ok_or_else(|| AppError::bad_request("Invalid capture inventory cursor"))
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(format!("traffic capture inventory database error: {error}"))
}

#[cfg(test)]
mod tests;
