use std::collections::VecDeque;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rsctf::services::event_security::TelemetryBatch;
use tokio::io::AsyncWriteExt;

const MAX_SPOOL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SPOOL_BATCHES: usize = 2_048;
const MAX_SPOOL_AGE: Duration = Duration::from_secs(24 * 60 * 60);

pub struct DurableSpool {
    directory: PathBuf,
    entries: VecDeque<(PathBuf, u64)>,
    bytes: u64,
}

impl DurableSpool {
    pub async fn open(directory: PathBuf) -> anyhow::Result<Self> {
        tokio::fs::create_dir_all(&directory).await?;
        let mut discovered = Vec::new();
        let mut reader = tokio::fs::read_dir(&directory).await?;
        while let Some(entry) = reader.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let metadata = entry.metadata().await?;
            if metadata.is_file() {
                discovered.push((path, metadata.len()));
            }
        }
        discovered.sort_by(|left, right| left.0.cmp(&right.0));
        let bytes = discovered.iter().map(|entry| entry.1).sum();
        Ok(Self {
            directory,
            entries: discovered.into(),
            bytes,
        })
    }

    pub async fn enqueue(&mut self, batch: &TelemetryBatch) -> anyhow::Result<()> {
        let mut durable = batch.clone();
        let mut encoded = serde_json::to_vec(&durable)?;
        if encoded.len() as u64 > MAX_SPOOL_BYTES {
            merge_own_payload_shed(&mut durable, encoded.len());
            encoded = serde_json::to_vec(&durable)?;
        }
        let final_path = self.directory.join(format!(
            "{:020}-{}.json",
            chrono::Utc::now().timestamp_millis(),
            batch.batch_id
        ));
        let temporary = final_path.with_extension("pending");
        write_synced_new(&temporary, &encoded).await?;
        tokio::fs::rename(&temporary, &final_path).await?;
        sync_directory(&self.directory).await?;
        let size = u64::try_from(encoded.len()).unwrap_or(u64::MAX);
        self.entries.push_back((final_path, size));
        self.bytes = self.bytes.saturating_add(size);

        let mut removed = false;
        loop {
            let mut changed = false;
            while self.entries.len() > MAX_SPOOL_BATCHES
                || self.bytes > MAX_SPOOL_BYTES
                || self.front_expired().await?
            {
                let Some((path, size)) = self.entries.pop_front() else {
                    break;
                };
                if path == final_path {
                    anyhow::bail!("bounded telemetry loss report exceeds its spool budget");
                }
                let dropped = read_batch(&path).await?;
                merge_dropped_batch(&mut durable, &dropped, size);
                tokio::fs::remove_file(path).await?;
                self.bytes = self.bytes.saturating_sub(size);
                removed = true;
                changed = true;
            }
            if !changed {
                break;
            }
            let replacement = serde_json::to_vec(&durable)?;
            let replacement_size =
                replace_synced(&self.directory, &final_path, &replacement).await?;
            let Some((path, current_size)) = self.entries.back_mut() else {
                anyhow::bail!("telemetry loss report lost its spool owner");
            };
            if *path != final_path {
                anyhow::bail!("telemetry loss report is not the newest spool entry");
            }
            self.bytes = self
                .bytes
                .saturating_sub(*current_size)
                .saturating_add(replacement_size);
            *current_size = replacement_size;
        }
        if removed {
            sync_directory(&self.directory).await?;
        }
        Ok(())
    }

    async fn front_expired(&self) -> anyhow::Result<bool> {
        let Some((path, _)) = self.entries.front() else {
            return Ok(false);
        };
        let modified = tokio::fs::metadata(path).await?.modified()?;
        Ok(std::time::SystemTime::now()
            .duration_since(modified)
            .unwrap_or_default()
            > MAX_SPOOL_AGE)
    }

    pub async fn front(&self) -> anyhow::Result<Option<TelemetryBatch>> {
        let Some((path, _)) = self.entries.front() else {
            return Ok(None);
        };
        Ok(Some(read_batch(path).await?))
    }

    pub async fn acknowledge_front(&mut self, batch_id: uuid::Uuid) -> anyhow::Result<()> {
        let Some((path, size)) = self.entries.front() else {
            anyhow::bail!("telemetry spool acknowledgement has no owner");
        };
        let size = *size;
        let stored = read_batch(path).await?;
        if stored.batch_id != batch_id {
            anyhow::bail!("telemetry spool acknowledgement is out of order");
        }
        tokio::fs::remove_file(path).await?;
        self.entries.pop_front();
        self.bytes = self.bytes.saturating_sub(size);
        sync_directory(&self.directory).await?;
        Ok(())
    }
}

#[cfg(unix)]
async fn sync_directory(path: &Path) -> std::io::Result<()> {
    tokio::fs::File::open(path).await?.sync_all().await
}

#[cfg(not(unix))]
async fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

async fn read_batch(path: &Path) -> anyhow::Result<TelemetryBatch> {
    let bytes = tokio::fs::read(path).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn write_synced_new(path: &Path, encoded: &[u8]) -> anyhow::Result<()> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options.open(path).await?;
    file.write_all(encoded).await?;
    file.flush().await?;
    file.sync_all().await?;
    Ok(())
}

async fn replace_synced(directory: &Path, path: &Path, encoded: &[u8]) -> anyhow::Result<u64> {
    let temporary = directory.join(format!(".rewrite-{}.pending", uuid::Uuid::new_v4()));
    write_synced_new(&temporary, encoded).await?;
    tokio::fs::rename(&temporary, path).await?;
    sync_directory(directory).await?;
    Ok(u64::try_from(encoded.len()).unwrap_or(u64::MAX))
}

fn merge_own_payload_shed(batch: &mut TelemetryBatch, encoded_bytes: usize) {
    let rows = batch.flows.len()
        + batch.dns_providers.len()
        + batch.peer_networks.len()
        + batch.flag_transports.len();
    batch.flows.clear();
    batch.dns_providers.clear();
    batch.peer_networks.clear();
    batch.flag_transports.clear();
    batch.sensor_dropped_rows = batch
        .sensor_dropped_rows
        .saturating_add(i64::try_from(rows).unwrap_or(i64::MAX));
    batch.sensor_dropped_bytes = batch
        .sensor_dropped_bytes
        .saturating_add(i64::try_from(encoded_bytes).unwrap_or(i64::MAX));
}

fn merge_dropped_batch(report: &mut TelemetryBatch, dropped: &TelemetryBatch, encoded_bytes: u64) {
    let rows = row_count(dropped);
    let bytes = encoded_bytes
        .saturating_add(u64::try_from(dropped.sensor_dropped_bytes.max(0)).unwrap_or(u64::MAX));
    report.sensor_dropped_rows = report
        .sensor_dropped_rows
        .saturating_add(i64::try_from(rows).unwrap_or(i64::MAX));
    report.sensor_dropped_bytes = report
        .sensor_dropped_bytes
        .saturating_add(i64::try_from(bytes).unwrap_or(i64::MAX));
}

fn row_count(batch: &TelemetryBatch) -> u64 {
    u64::try_from(
        batch.flows.len()
            + batch.dns_providers.len()
            + batch.peer_networks.len()
            + batch.flag_transports.len(),
    )
    .unwrap_or(u64::MAX)
    .saturating_add(u64::try_from(batch.sensor_dropped_rows).unwrap_or(u64::MAX))
}

#[derive(Debug, thiserror::Error)]
enum UploadError {
    #[error("{0}")]
    Permanent(String),
    #[error("{0}")]
    Transient(String, Option<Duration>),
}

#[derive(Debug, thiserror::Error)]
pub enum DrainError {
    #[error("permanent sensor upload rejection; spool quarantined: {0}")]
    Permanent(String),
    #[error(transparent)]
    Transient(#[from] anyhow::Error),
}

async fn upload_batch(
    client: &reqwest::Client,
    api: &str,
    token: &str,
    batch: &TelemetryBatch,
) -> Result<(), UploadError> {
    let response = client
        .post(format!("{api}/api/internal/event-security/telemetry"))
        .bearer_auth(token)
        .json(batch)
        .send()
        .await
        .map_err(|error| UploadError::Transient(error.to_string(), None))?;
    if response.status().is_success() {
        return Ok(());
    }
    let status = response.status();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .map(|delay| delay.min(Duration::from_secs(30)));
    let message = format!("event sensor upload returned {status}");
    if status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
    {
        Err(UploadError::Transient(message, retry_after))
    } else {
        Err(UploadError::Permanent(message))
    }
}

async fn upload_with_retry(
    client: &reqwest::Client,
    api: &str,
    token: &str,
    batch: &TelemetryBatch,
) -> Result<(), UploadError> {
    let mut cap = Duration::from_millis(250);
    for attempt in 0..6_u32 {
        match upload_batch(client, api, token, batch).await {
            Ok(()) => return Ok(()),
            Err(error @ UploadError::Permanent(_)) => return Err(error),
            Err(UploadError::Transient(message, retry_after)) if attempt == 5 => {
                return Err(UploadError::Transient(message, retry_after));
            }
            Err(UploadError::Transient(_, retry_after)) => {
                let fraction = u64::from(batch.batch_id.as_bytes()[attempt as usize]) + 1;
                let jitter = Duration::from_millis(
                    u64::try_from(cap.as_millis())
                        .unwrap_or(u64::MAX)
                        .saturating_mul(fraction)
                        / 256,
                );
                tokio::time::sleep(retry_after.unwrap_or(jitter)).await;
                cap = cap.saturating_mul(2).min(Duration::from_secs(10));
            }
        }
    }
    unreachable!("bounded upload attempts return from every terminal branch")
}

pub async fn enqueue_batch(spool: &mut DurableSpool, batch: TelemetryBatch) -> anyhow::Result<()> {
    spool.enqueue(&batch).await
}

pub async fn drain_spool(
    client: &reqwest::Client,
    api: &str,
    token: &str,
    spool: &mut DurableSpool,
) -> Result<(), DrainError> {
    for _ in 0..16 {
        let Some(batch) = spool.front().await? else {
            return Ok(());
        };
        match upload_with_retry(client, api, token, &batch).await {
            Ok(()) => spool.acknowledge_front(batch.batch_id).await?,
            Err(UploadError::Permanent(error)) => {
                return Err(DrainError::Permanent(error));
            }
            Err(error) => return Err(DrainError::Transient(error.into())),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_batch() -> TelemetryBatch {
        TelemetryBatch {
            batch_id: uuid::Uuid::new_v4(),
            game_id: 1,
            flows: Vec::new(),
            dns_providers: Vec::new(),
            peer_networks: Vec::new(),
            flag_transports: Vec::new(),
            sensor_dropped_rows: 0,
            sensor_dropped_bytes: 0,
        }
    }

    #[test]
    fn spool_limits_are_explicit_and_small() {
        assert_eq!(MAX_SPOOL_BYTES, 64 * 1024 * 1024);
        assert_eq!(MAX_SPOOL_BATCHES, 2_048);
        assert_eq!(MAX_SPOOL_AGE, Duration::from_secs(24 * 60 * 60));
    }

    #[test]
    fn shed_counters_move_into_the_newest_durable_batch() {
        let mut report = empty_batch();
        let mut dropped = empty_batch();
        dropped.sensor_dropped_rows = 7;
        dropped.sensor_dropped_bytes = 11;
        merge_dropped_batch(&mut report, &dropped, 13);
        assert_eq!(report.sensor_dropped_rows, 7);
        assert_eq!(report.sensor_dropped_bytes, 24);
    }
}
