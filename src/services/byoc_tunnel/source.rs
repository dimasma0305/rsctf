//! Private, bounded snapshots from agent-declared source files.
use super::{authorization, AuthorizationGeneration, RelayEndpoint, TunnelHandle};
use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};
use futures::io::{AsyncReadExt, AsyncWriteExt};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

const WIRE_LIMIT: usize = 64 * 1024;
const KEEP: i64 = 128;
mod history;
pub(crate) use history::{history, ssh_history, Page};
#[cfg(test)]
mod tests;
static COLLECTORS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

async fn poll_reports<F: std::future::Future<Output = ()>>(
    collectors: &tokio::sync::Semaphore,
    interval: Duration,
    mut collect: impl FnMut() -> F,
) {
    loop {
        // One waiter per connected session. Disconnecting cancels its waiter.
        let Ok(permit) = collectors.acquire().await else {
            return;
        };
        collect().await;
        drop(permit);
        tokio::time::sleep(interval).await;
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileIdentity {
    pub(crate) blob: String,
    pub(crate) executable: bool,
}

#[derive(Clone, Copy)]
enum HistoryTable {
    Byoc,
    HostedSsh,
}

impl HistoryTable {
    fn sql_name(self) -> &'static str {
        match self {
            Self::Byoc => r#""AdByocSourceHistory""#,
            Self::HostedSsh => r#""AdHostedSshSourceHistory""#,
        }
    }
}
pub(crate) type Files = BTreeMap<String, Option<FileIdentity>>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WireReport {
    status: String,
    #[serde(rename = "sampledAt")]
    _sampled_at: Option<u64>,
    files: Option<Files>,
}

struct Snapshot {
    state: &'static str,
    files: Files,
}

impl Snapshot {
    fn empty(state: &'static str) -> Self {
        Self {
            state,
            files: Files::new(),
        }
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let report: WireReport = serde_json::from_slice(bytes).ok()?;
        match (report.status.as_str(), report.files) {
            ("observed", Some(files)) => {
                if files.len() > 128
                    || files.iter().any(|(name, id)| {
                        name.len() > 240
                            || name.trim() != name
                            || name.contains('\\')
                            || name.chars().any(char::is_control)
                            || name == ".rsctf-files"
                            || name.split('/').any(|s| {
                                matches!(s, "" | "." | "..") || s.eq_ignore_ascii_case(".git")
                            })
                            || id.as_ref().is_some_and(|id| {
                                id.blob.len() != 64
                                    || !id
                                        .blob
                                        .bytes()
                                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                            })
                    })
                {
                    return None;
                }
                Some(Self {
                    state: "observed",
                    files,
                })
            }
            ("notRegistered", None) => Some(Self::empty("notRegistered")),
            ("unavailable" | "pending", None) => Some(Self::empty("unavailable")),
            _ => None,
        }
    }
}

async fn read_report(handle: &TunnelHandle) -> Option<Snapshot> {
    let mut stream = handle.open_stream().await?;
    stream.write_all(b"P").await.ok()?;
    stream.flush().await.ok()?;
    let mut bytes = Vec::new();
    stream
        .take((WIRE_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .ok()?;
    if bytes.len() > WIRE_LIMIT {
        return None;
    }
    Snapshot::decode(&bytes)
}

pub(super) struct Session<'a> {
    pub st: &'a SharedState,
    pub game: i32,
    pub participation: i32,
    pub challenge: i32,
    pub token: &'a str,
    pub generation: AuthorizationGeneration,
    pub endpoint: &'a RelayEndpoint,
    pub handle: &'a TunnelHandle,
    pub supported: bool,
}

impl Session<'_> {
    pub async fn observe(&self) {
        // Stagger synchronized reconnects. At most four source streams can be
        // in flight on the network owner, independently of checker/flag traffic.
        let offset = (u64::from(self.participation.unsigned_abs()) * 317
            + u64::from(self.challenge.unsigned_abs()) * 101)
            % 5000;
        tokio::time::sleep(Duration::from_millis(1000 + offset)).await;
        poll_reports(&COLLECTORS, Duration::from_millis(60000 + offset), || async {
            let snapshot = if self.supported {
                tokio::time::timeout(Duration::from_secs(5), read_report(self.handle))
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| Snapshot::empty("unavailable"))
            } else {
                Snapshot::empty("legacyAgent")
            };
            if let Ok(Err(error)) =
                tokio::time::timeout(Duration::from_secs(3), self.save(snapshot)).await
            {
                tracing::debug!(challenge=self.challenge,participation=self.participation,%error,"BYOC source observation deferred");
            }
        }).await;
    }

    async fn save(&self, snapshot: Snapshot) -> AppResult<()> {
        let mut lock = crate::services::ad::service_lifecycle::acquire_publication_lock(
            self.st.pg(),
            self.game,
            self.participation,
            self.challenge,
        )
        .await?;
        let _generation = self
            .st
            .byoc
            .publication_guard(self.participation, self.challenge, self.generation)
            .await
            .ok_or(AppError::Forbidden)?;
        if self
            .endpoint
            .raw_current()
            .await
            .is_none_or(|h| h.id != self.handle.id)
            || !self
                .st
                .byoc
                .contains_endpoint(self.participation, self.challenge, self.endpoint.id())
                .await
            || !authorization::live_tunnel_authorized_on(
                &mut **lock.transaction_mut(),
                self.game,
                self.participation,
                self.challenge,
                self.token,
            )
            .await
        {
            return Err(AppError::Forbidden);
        }
        let db = &mut **lock.transaction_mut();
        let service=sqlx::query_scalar::<_,i32>(r#"SELECT id FROM "AdTeamServices" WHERE game_id=$1 AND participation_id=$2 AND challenge_id=$3 AND container_id IS NULL AND port=$4 FOR SHARE"#)
            .bind(self.game).bind(self.participation).bind(self.challenge).bind(self.endpoint.port()).fetch_optional(&mut *db).await.map_err(db_error)?.ok_or(AppError::Forbidden)?;
        store(db, service, &snapshot).await?;
        lock.release()
            .await
            .map_err(|e| AppError::internal(e.to_string()))
    }
}

fn db_error(e: sqlx::Error) -> AppError {
    AppError::internal(e.to_string())
}

// The caller holds the same per-service transaction lock as tunnel replacement.
async fn store(db: &mut sqlx::PgConnection, service: i32, snapshot: &Snapshot) -> AppResult<()> {
    store_in(db, service, snapshot, HistoryTable::Byoc).await
}

pub(crate) async fn store_ssh(
    db: &mut sqlx::PgConnection,
    service: i32,
    observed: Option<BTreeMap<String, Option<(String, bool)>>>,
) -> AppResult<()> {
    let snapshot = match observed {
        Some(files) => Snapshot {
            state: "observed",
            files: files
                .into_iter()
                .map(|(path, entry)| {
                    (
                        path,
                        entry.map(|(blob, executable)| FileIdentity { blob, executable }),
                    )
                })
                .collect(),
        },
        None => Snapshot::empty("unavailable"),
    };
    store_in(db, service, &snapshot, HistoryTable::HostedSsh).await
}

async fn store_in(
    db: &mut sqlx::PgConnection,
    service: i32,
    snapshot: &Snapshot,
    table: HistoryTable,
) -> AppResult<()> {
    let table = table.sql_name();
    let files =
        serde_json::to_string(&snapshot.files).map_err(|e| AppError::internal(e.to_string()))?;
    let previous = sqlx::query_as::<_,(i64,bool)>(&format!("SELECT revision, state=$2 AND files=$3 FROM {table} WHERE service_id=$1 ORDER BY revision DESC LIMIT 1"))
        .bind(service).bind(snapshot.state).bind(&files).fetch_optional(&mut *db).await.map_err(db_error)?;
    if let Some((revision, true)) = previous {
        sqlx::query(&format!(
            "UPDATE {table} SET last_seen=clock_timestamp() WHERE service_id=$1 AND revision=$2"
        ))
        .bind(service)
        .bind(revision)
        .execute(db)
        .await
        .map_err(db_error)?;
        return Ok(());
    }
    let revision = previous.map_or(1, |(n, _)| n + 1);
    sqlx::query(&format!(
        "INSERT INTO {table}(service_id,revision,state,files) VALUES ($1,$2,$3,$4)"
    ))
    .bind(service)
    .bind(revision)
    .bind(snapshot.state)
    .bind(files)
    .execute(&mut *db)
    .await
    .map_err(db_error)?;
    if revision > KEEP {
        sqlx::query(&format!(
            "DELETE FROM {table} WHERE service_id=$1 AND revision <= $2"
        ))
        .bind(service)
        .bind(revision - KEEP)
        .execute(db)
        .await
        .map_err(db_error)?;
    }
    Ok(())
}
