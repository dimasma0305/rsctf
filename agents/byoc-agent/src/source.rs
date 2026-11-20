//! Sample only the source files the team explicitly registered in its read-only mount.
use ring::digest::{Context, SHA256};
use rustix::fs::{openat2, Mode, OFlags, ResolveFlags};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Bytes;

const SOURCE_ROOT: &str = "/rsctf-source";
const REGISTRATION: &str = ".rsctf-files";
const MAX_MANIFEST: u64 = 16 * 1024;
const MAX_FILES: usize = 128;
const MAX_FILE: u64 = 8 * 1024 * 1024;
const MAX_TOTAL: u64 = 64 * 1024 * 1024;
const INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct Identity {
    blob: String,
    executable: bool,
}

type Files = BTreeMap<String, Option<Identity>>;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub(crate) enum Observation {
    Pending,
    NotRegistered,
    Unavailable,
    Observed { files: Files },
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct Stamp {
    device: u64,
    inode: u64,
    size: u64,
    ctime: (i64, i64),
    mtime: (i64, i64),
    mode: u32,
}

impl From<&Metadata> for Stamp {
    fn from(m: &Metadata) -> Self {
        Self {
            device: m.dev(),
            inode: m.ino(),
            size: m.len(),
            ctime: (m.ctime(), m.ctime_nsec()),
            mtime: (m.mtime(), m.mtime_nsec()),
            mode: m.mode(),
        }
    }
}

#[derive(Default)]
struct Scanner {
    previous: BTreeMap<String, (Stamp, Identity)>,
    #[cfg(test)]
    hashed_bytes: u64,
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid registered source")
}

// Pin the entry before inspecting its type. A FIFO, socket, device, hard link,
// symbolic link or nested bind mount must never be opened for reading.
fn pin(root: &File, name: &str, limit: u64) -> io::Result<(File, Stamp)> {
    let fd = openat2(
        root,
        name,
        OFlags::PATH | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_XDEV,
    )?;
    let pinned = File::from(fd);
    let meta = pinned.metadata()?;
    if !meta.is_file() || meta.nlink() != 1 || meta.len() > limit {
        return Err(invalid());
    }
    Ok((pinned, Stamp::from(&meta)))
}

fn reader(pinned: &File) -> io::Result<File> {
    File::open(format!("/proc/self/fd/{}", pinned.as_raw_fd()))
}

fn paths(text: &str) -> io::Result<BTreeSet<&str>> {
    let mut paths = BTreeSet::new();
    for path in text.lines().filter(|s| !s.is_empty()) {
        if path.len() > 240
            || path != path.trim()
            || path.contains('\\')
            || path.chars().any(char::is_control)
            || path == REGISTRATION
            || path
                .split('/')
                .any(|s| matches!(s, "" | "." | "..") || s.eq_ignore_ascii_case(".git"))
            || !paths.insert(path)
            || paths.len() > MAX_FILES
        {
            return Err(invalid());
        }
    }
    Ok(paths)
}

fn hash(pinned: &File, stamp: Stamp) -> io::Result<Identity> {
    let mut hash = Context::new(&SHA256);
    hash.update(format!("blob {}\0", stamp.size).as_bytes());
    let mut file = reader(pinned)?.take(stamp.size + 1);
    let mut buffer = [0; 32 * 1024];
    let mut bytes = 0;
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        hash.update(&buffer[..n]);
    }
    if bytes != stamp.size || Stamp::from(&pinned.metadata()?) != stamp {
        return Err(invalid());
    }
    Ok(Identity {
        blob: hex::encode(hash.finish().as_ref()),
        executable: stamp.mode & 0o111 != 0,
    })
}

impl Scanner {
    fn sample(&mut self, path: &Path) -> io::Result<Observation> {
        let root = OpenOptions::new()
            .read(true)
            .custom_flags((OFlags::DIRECTORY | OFlags::NOFOLLOW).bits() as i32)
            .open(path);
        let registration = root.and_then(|root| {
            let (file, stamp) = pin(&root, REGISTRATION, MAX_MANIFEST)?;
            Ok((root, file, stamp))
        });
        let (root, manifest, stamp) = match registration {
            Ok(registered) => registered,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.previous.clear();
                return Ok(Observation::NotRegistered);
            }
            Err(e) => return Err(e),
        };
        let mut text = String::new();
        reader(&manifest)?
            .take(MAX_MANIFEST + 1)
            .read_to_string(&mut text)?;
        if text.len() as u64 != stamp.size {
            return Err(invalid());
        }
        let paths = paths(&text)?;
        let mut files = Files::new();
        let mut next = BTreeMap::new();
        let mut total = 0;
        for path in paths {
            let (pinned, stamp) = match pin(&root, path, MAX_FILE) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    files.insert(path.to_owned(), None);
                    continue;
                }
                Err(e) => return Err(e),
            };
            total += stamp.size;
            if total > MAX_TOTAL {
                return Err(invalid());
            }
            let identity = match self.previous.get(path) {
                Some((old, identity)) if *old == stamp => identity.clone(),
                _ => {
                    let id = hash(&pinned, stamp)?;
                    #[cfg(test)]
                    {
                        self.hashed_bytes += stamp.size;
                    }
                    id
                }
            };
            files.insert(path.to_owned(), Some(identity.clone()));
            next.insert(path.to_owned(), (stamp, identity));
        }
        // An editor may replace the registration while a large file is being
        // hashed. Discard that scan and retry with the new list on the next tick.
        if pin(&root, REGISTRATION, MAX_MANIFEST)?.1 != stamp {
            return Err(invalid());
        }
        self.previous = next;
        Ok(if files.is_empty() {
            Observation::NotRegistered
        } else {
            Observation::Observed { files }
        })
    }
}

fn log_changes(previous: &Observation, current: &Observation) {
    match (previous, current) {
        (Observation::Observed { files: before }, Observation::Observed { files: after }) => {
            for path in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
                let change = match (before.get(path), after.get(path)) {
                    (None, Some(_)) => "registered",
                    (Some(_), None) => "unregistered",
                    (Some(None), Some(Some(_))) => "created",
                    (Some(Some(_)), Some(None)) => "deleted",
                    (Some(a), Some(b)) if a != b => "modified",
                    _ => continue,
                };
                tracing::info!(path = ?path, change, "registered source changed");
            }
        }
        (_, Observation::Observed { files }) => {
            tracing::info!(files = files.len(), "registered source baseline recorded")
        }
        (_, Observation::Unavailable) => tracing::warn!("registered source could not be observed"),
        (_, Observation::NotRegistered) => {
            tracing::info!("no source files registered for observation")
        }
        (_, Observation::Pending) => {}
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Report<'a> {
    sampled_at: u64,
    #[serde(flatten)]
    observation: &'a Observation,
}

pub(crate) struct CachedReport {
    bytes: Bytes,
    received: std::time::Instant,
}

impl CachedReport {
    pub(crate) fn new(bytes: Bytes) -> Self {
        Self {
            bytes,
            received: std::time::Instant::now(),
        }
    }
    fn current(&self) -> Bytes {
        if self.received.elapsed() > 2 * INTERVAL {
            Bytes::from_static(br#"{"status":"unavailable"}"#)
        } else {
            self.bytes.clone()
        }
    }
}

pub(crate) type Reports = watch::Receiver<CachedReport>;

pub(crate) async fn send(mut stream: yamux::Stream, reports: &Reports) {
    use futures::io::AsyncWriteExt;
    let snapshot = reports.borrow().current();
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        stream.write_all(&snapshot).await?;
        stream.close().await
    })
    .await;
}

pub(crate) async fn monitor(sender: watch::Sender<CachedReport>) {
    let mut scanner = Scanner::default();
    let mut previous = Observation::Pending;
    loop {
        let next = tokio::task::spawn_blocking(move || {
            let result = scanner
                .sample(Path::new(SOURCE_ROOT))
                .unwrap_or(Observation::Unavailable);
            (scanner, result)
        })
        .await;
        let current;
        (scanner, current) = match next {
            Ok(pair) => pair,
            Err(_) => (Scanner::default(), Observation::Unavailable),
        };
        if previous != current {
            log_changes(&previous, &current);
        }
        let sampled_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let report = Report {
            sampled_at,
            observation: &current,
        };
        if let Ok(bytes) = serde_json::to_vec(&report) {
            sender.send_replace(CachedReport::new(Bytes::from(bytes)));
        }
        previous = current;
        tokio::time::sleep(INTERVAL).await;
    }
}

#[cfg(test)]
mod tests;
