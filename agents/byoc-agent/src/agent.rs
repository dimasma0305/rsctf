//! agent mode: the team-side end of the tunnel.
//!
//! It dials OUTBOUND to the relay's control port over a WebSocket (through RSCTF's
//! existing WS<->TCP proxy bridge), runs a yamux CLIENT over the WebSocket-as-byte-
//! stream, accepts the streams the relay forwards, and dispatches each by its
//! leading type byte:
//!
//!   'S' service — dial the team's local service and pipe raw bytes.
//!   'F' flag    — read [8-byte big-endian seq][flag] and atomically write the
//!                 newest flag to the flag file (temp file + rename), then send
//!                 ['A'][the same seq] as its install acknowledgement.
//!   'E' exec    — open an interactive shell in the configured service container.
//!
//! Only one outbound connection is needed; no inbound port / public IP / VPN.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bollard::exec::{CreateExecOptions, ResizeExecOptions, StartExecResults};
use bollard::Docker;
use futures::io::{
    AsyncRead as FAsyncRead, AsyncReadExt, AsyncWrite as FAsyncWrite,
    AsyncWriteExt as FuturesAsyncWriteExt,
};
use futures::{future, Sink, Stream};
use tokio::io::AsyncWriteExt as TokioAsyncWriteExt;
use tokio::net::TcpStream;
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::{Bytes, Error as WebSocketError, Message};
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tokio_util::compat::FuturesAsyncReadCompatExt;
use tracing::{error, info, warn};
use yamux::{Connection, Mode, Stream as YamuxStream};

use crate::{
    env, must_env, yamux_config, AGENT_PROTOCOL, AGENT_PROTOCOL_HEADER, STREAM_EXEC, STREAM_FLAG,
    STREAM_SERVICE,
};

// ---------------------------------------------------------------------------
// WebSocket <-> byte-stream adapter
// ---------------------------------------------------------------------------

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Adapts a tungstenite WebSocket into a raw byte stream implementing the
/// futures-io `AsyncRead`/`AsyncWrite` traits yamux runs over — the analogue of
/// Go's `websocket.NetConn(..., MessageBinary)`. Each write becomes one binary
/// WebSocket message; reads flatten inbound binary messages into a byte stream,
/// buffering any bytes not consumed by a single `poll_read`.
struct WsByteStream {
    ws: Ws,
    read_buf: Bytes,
    read_pos: usize,
}

impl WsByteStream {
    fn new(ws: Ws) -> Self {
        Self {
            ws,
            read_buf: Bytes::new(),
            read_pos: 0,
        }
    }
}

impl FAsyncRead for WsByteStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        loop {
            // Serve leftover bytes from a partially consumed message first.
            if this.read_pos < this.read_buf.len() {
                let n = std::cmp::min(buf.len(), this.read_buf.len() - this.read_pos);
                buf[..n].copy_from_slice(&this.read_buf[this.read_pos..this.read_pos + n]);
                this.read_pos += n;
                return Poll::Ready(Ok(n));
            }
            match Pin::new(&mut this.ws).poll_next(cx) {
                Poll::Ready(Some(Ok(msg))) => match msg {
                    Message::Binary(d) => {
                        if d.is_empty() {
                            continue;
                        }
                        this.read_buf = d;
                        this.read_pos = 0;
                    }
                    Message::Text(s) => {
                        let d: Bytes = s.into();
                        if d.is_empty() {
                            continue;
                        }
                        this.read_buf = d;
                        this.read_pos = 0;
                    }
                    // Control frames are handled by tungstenite internally.
                    Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
                    Message::Close(_) => return Poll::Ready(Ok(0)), // EOF
                },
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Err(io::Error::other(e))),
                Poll::Ready(None) => return Poll::Ready(Ok(0)), // stream ended -> EOF
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl FAsyncWrite for WsByteStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        match Pin::new(&mut this.ws).poll_ready(cx) {
            Poll::Ready(Ok(())) => {
                match Pin::new(&mut this.ws)
                    .start_send(Message::Binary(Bytes::copy_from_slice(buf)))
                {
                    Ok(()) => Poll::Ready(Ok(buf.len())),
                    Err(e) => Poll::Ready(Err(io::Error::other(e))),
                }
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(io::Error::other(e))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        Pin::new(&mut this.ws)
            .poll_flush(cx)
            .map_err(io::Error::other)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        Pin::new(&mut this.ws)
            .poll_close(cx)
            .map_err(io::Error::other)
    }
}

// ---------------------------------------------------------------------------
// agent driver
// ---------------------------------------------------------------------------

const MAX_FLAG_BYTES: usize = 4096;
const FLAG_ACK: u8 = b'A';
const AGENT_STATE_HEADER: &str = "x-rsctf-byoc-state";
const STABLE_TUNNEL_LIFETIME: Duration = Duration::from_secs(60);
const MINIMUM_RECONNECT_DELAY: Duration = Duration::from_millis(250);
const BASE_RECONNECT_DELAY: Duration = Duration::from_secs(1);
const MAXIMUM_RECONNECT_DELAY: Duration = Duration::from_secs(5 * 60);
const MAXIMUM_SERVER_RETRY_AFTER: Duration = Duration::from_secs(30 * 24 * 60 * 60);
static RECONNECT_ENTROPY_STATE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
struct ConnectFailure {
    message: String,
    state: Option<String>,
    terminal: bool,
    retry_after: Option<Duration>,
    connected_for: Option<Duration>,
}

impl ConnectFailure {
    fn transport(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            state: None,
            terminal: false,
            retry_after: None,
            connected_for: None,
        }
    }

    fn connected(message: impl Into<String>, connected_for: Duration) -> Self {
        Self {
            connected_for: Some(connected_for),
            ..Self::transport(message)
        }
    }
}

#[derive(Default)]
struct ReconnectPolicy {
    attempt: u32,
}

impl ReconnectPolicy {
    fn delay(&mut self, failure: &ConnectFailure) -> Option<Duration> {
        self.delay_with_sample(failure, reconnect_entropy())
    }

    fn delay_with_sample(&mut self, failure: &ConnectFailure, sample: u64) -> Option<Duration> {
        if failure.terminal {
            return None;
        }
        if failure
            .connected_for
            .is_some_and(|lifetime| lifetime >= STABLE_TUNNEL_LIFETIME)
        {
            self.attempt = 0;
        } else {
            self.attempt = self.attempt.saturating_add(1);
        }
        let jitter = jittered_reconnect_delay(self.attempt, sample);
        Some(
            failure
                .retry_after
                .map_or(jitter, |minimum| minimum.saturating_add(jitter))
                .min(MAXIMUM_SERVER_RETRY_AFTER),
        )
    }
}

fn jittered_reconnect_delay(attempt: u32, sample: u64) -> Duration {
    let exponent = attempt.saturating_sub(1).min(20);
    let ceiling_millis = BASE_RECONNECT_DELAY
        .as_millis()
        .saturating_mul(1_u128 << exponent)
        .min(MAXIMUM_RECONNECT_DELAY.as_millis()) as u64;
    let minimum_millis = MINIMUM_RECONNECT_DELAY.as_millis() as u64;
    let span = ceiling_millis.saturating_sub(minimum_millis);
    Duration::from_millis(minimum_millis + sample % span.saturating_add(1))
}

fn reconnect_entropy() -> u64 {
    let seed = RECONNECT_ENTROPY_STATE.load(Ordering::Relaxed);
    if seed == 0 {
        seed_reconnect_entropy("");
    }
    let mut current = RECONNECT_ENTROPY_STATE.load(Ordering::Relaxed);
    loop {
        let mut next = current;
        next ^= next << 13;
        next ^= next >> 7;
        next ^= next << 17;
        match RECONNECT_ENTROPY_STATE.compare_exchange_weak(
            current,
            next.max(1),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return next,
            Err(observed) => current = observed,
        }
    }
}

fn seed_reconnect_entropy(agent_scope: &str) {
    if RECONNECT_ENTROPY_STATE.load(Ordering::Relaxed) == 0 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let mut scope = DefaultHasher::new();
        agent_scope.hash(&mut scope);
        let initialized = now
            ^ u64::from(std::process::id()).rotate_left(17)
            ^ scope.finish().rotate_left(31)
            ^ 0x9e37_79b9_7f4a_7c15;
        let _ = RECONNECT_ENTROPY_STATE.compare_exchange(
            0,
            initialized.max(1),
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }
}

fn retry_after(
    response: &tokio_tungstenite::tungstenite::http::Response<Option<Vec<u8>>>,
) -> Option<Duration> {
    let seconds = response
        .headers()
        .get(tokio_tungstenite::tungstenite::http::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .parse::<u64>()
        .ok()?;
    Some(Duration::from_secs(seconds).min(MAXIMUM_SERVER_RETRY_AFTER))
}

fn handshake_failure(
    response: tokio_tungstenite::tungstenite::http::Response<Option<Vec<u8>>>,
) -> ConnectFailure {
    let status = response.status();
    let state = response
        .headers()
        .get(AGENT_STATE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let terminal = state
        .as_deref()
        .is_some_and(|state| state.starts_with("terminal-"))
        || status == tokio_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED
        || status == tokio_tungstenite::tungstenite::http::StatusCode::FORBIDDEN
        || status == tokio_tungstenite::tungstenite::http::StatusCode::NOT_FOUND
        || status == tokio_tungstenite::tungstenite::http::StatusCode::GONE
        || status == tokio_tungstenite::tungstenite::http::StatusCode::UPGRADE_REQUIRED
        || status.is_client_error()
            && status != tokio_tungstenite::tungstenite::http::StatusCode::TOO_MANY_REQUESTS
            && status != tokio_tungstenite::tungstenite::http::StatusCode::TOO_EARLY;
    ConnectFailure {
        message: format!("WebSocket handshake returned {status}"),
        state,
        terminal,
        retry_after: retry_after(&response),
        connected_for: None,
    }
}

#[derive(Clone)]
struct AppliedFlag {
    sequence: u64,
    value: Vec<u8>,
}

#[derive(Default)]
struct FlagSinkState {
    generation: u64,
    applied: Option<AppliedFlag>,
}

/// Process-wide serializer for every connection generation. File replacement
/// runs under this lock on a blocking worker, so beginning a newer generation
/// strictly follows every older rename even when its async handler was aborted.
#[derive(Default)]
struct FlagSink {
    state: std::sync::Mutex<FlagSinkState>,
}

impl FlagSink {
    async fn begin_connection(self: &Arc<Self>) -> Result<u64, String> {
        let sink = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut state = sink
                .state
                .lock()
                .map_err(|_| "flag sink lock is poisoned".to_string())?;
            state.generation = state
                .generation
                .checked_add(1)
                .ok_or_else(|| "flag sink exhausted its local connection generation".to_string())?;
            Ok(state.generation)
        })
        .await
        .map_err(|error| format!("flag sink generation task failed: {error}"))?
    }
}

pub async fn run_agent() {
    let tunnel_url = must_env("RSCTF_BYOC_TUNNEL_URL"); // wss://rsctf/api/Ad/Byoc/Agent/<token>
    let service = must_env("RSCTF_BYOC_SERVICE"); // host:port of the team's service
    let flag_file = env("RSCTF_BYOC_FLAG_FILE", "/flag"); // where to write the rotating flag
    let service_container = env("RSCTF_BYOC_SERVICE_CONTAINER", "");
    let flag_sink = Arc::new(FlagSink::default());
    let mut reconnect = ReconnectPolicy::default();
    // The bearer-bearing URL is not logged, but its hash de-correlates agents
    // that start under the same clock/PID conditions (for example PID 1 in
    // hundreds of Compose containers after a synchronized outage).
    seed_reconnect_entropy(&tunnel_url);

    loop {
        let failure = match flag_sink.begin_connection().await {
            Ok(generation) => connect_once(
                &tunnel_url,
                &service,
                &flag_file,
                &service_container,
                flag_sink.clone(),
                generation,
            )
            .await
            .expect_err("a tunnel session only returns after it closes"),
            Err(error) => ConnectFailure::transport(format!(
                "could not begin flag connection generation: {error}"
            )),
        };
        let Some(delay) = reconnect.delay(&failure) else {
            error!(
                state = failure.state.as_deref().unwrap_or("terminal"),
                error = %failure.message,
                "tunnel authorization is terminal; stopping the agent"
            );
            return;
        };
        warn!(
            state = failure.state.as_deref().unwrap_or("transient"),
            error = %failure.message,
            retry_after_ms = delay.as_millis(),
            "tunnel disconnected; reconnecting after backoff"
        );
        tokio::time::sleep(delay).await;
    }
}

/// Holds one tunnel session: dial the WebSocket, run a yamux client over it, and
/// serve forwarded streams until the connection drops.
async fn connect_once(
    tunnel_url: &str,
    service: &str,
    flag_file: &str,
    service_container: &str,
    flag_sink: Arc<FlagSink>,
    generation: u64,
) -> Result<(), ConnectFailure> {
    let request =
        tunnel_request(tunnel_url).map_err(|error| ConnectFailure::transport(error.to_string()))?;
    let (ws, _response) = match connect_async(request).await {
        Ok(connected) => connected,
        Err(WebSocketError::Http(response)) => return Err(handshake_failure(*response)),
        Err(error) => return Err(ConnectFailure::transport(error.to_string())),
    };
    let connected_at = Instant::now();
    let socket = WsByteStream::new(ws);

    let mut conn = Connection::new(socket, yamux_config(), Mode::Client);
    info!(service, "tunnel connected");

    let mut handlers = JoinSet::new();

    // The accept loop is also the driver: continuously polling `poll_next_inbound`
    // is what makes the whole yamux connection (and every accepted stream's I/O)
    // progress in yamux 0.13.
    let result = loop {
        match future::poll_fn(|cx| conn.poll_next_inbound(cx)).await {
            Some(Ok(stream)) => {
                let service = service.to_string();
                let flag_file = flag_file.to_string();
                let service_container = service_container.to_string();
                let sink = flag_sink.clone();
                handlers.spawn(handle_stream(
                    stream,
                    service,
                    flag_file,
                    service_container,
                    sink,
                    generation,
                ));
            }
            Some(Err(error)) => {
                break Err(ConnectFailure::connected(
                    error.to_string(),
                    connected_at.elapsed(),
                ));
            }
            None => {
                break Err(ConnectFailure::connected(
                    "tunnel closed",
                    connected_at.elapsed(),
                ))
            }
        }
    };
    handlers.abort_all();
    while handlers.join_next().await.is_some() {}
    result
}

fn tunnel_request(
    tunnel_url: &str,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, tokio_tungstenite::tungstenite::Error>
{
    let mut request = tunnel_url.into_client_request()?;
    request.headers_mut().insert(
        HeaderName::from_static(AGENT_PROTOCOL_HEADER),
        HeaderValue::from_static(AGENT_PROTOCOL),
    );
    Ok(request)
}

/// Dispatches one forwarded stream by its leading type byte.
async fn handle_stream(
    mut stream: YamuxStream,
    service: String,
    flag_file: String,
    service_container: String,
    sink: Arc<FlagSink>,
    generation: u64,
) {
    let mut hdr = [0u8; 1];
    if stream.read_exact(&mut hdr).await.is_err() {
        return;
    }
    match hdr[0] {
        STREAM_SERVICE => dial_and_pipe(stream, &service).await,
        STREAM_FLAG => write_flag(stream, &flag_file, sink, generation).await,
        STREAM_EXEC => exec_shell(stream, &service_container).await,
        other => warn!("unknown stream type {:?}", other as char),
    }
    // Dropping `stream` here closes it (the Go `defer stream.Close()`).
}

/// Open a TTY shell in the service container and bridge it to an interactive
/// RSCTF admin/SSH stream. The Docker socket is optional; service and flag
/// forwarding continue to work when it is not mounted.
async fn exec_shell(mut stream: YamuxStream, service_container: &str) {
    let mut size = [0_u8; 4];
    if stream.read_exact(&mut size).await.is_err() {
        return;
    }
    if service_container.is_empty() {
        warn!("exec requested without RSCTF_BYOC_SERVICE_CONTAINER configured");
        return;
    }
    if let Err(error) = exec_shell_inner(stream, service_container, size).await {
        warn!(container = service_container, %error, "container exec failed");
    }
}

async fn exec_shell_inner(
    stream: YamuxStream,
    service_container: &str,
    size: [u8; 4],
) -> Result<(), String> {
    let cols = u16::from_be_bytes([size[0], size[1]]);
    let rows = u16::from_be_bytes([size[2], size[3]]);
    let docker = Docker::connect_with_local_defaults().map_err(|error| error.to_string())?;
    let exec = docker
        .create_exec(
            service_container,
            CreateExecOptions {
                attach_stdin: Some(true),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                tty: Some(true),
                cmd: Some(vec!["/bin/sh"]),
                ..Default::default()
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    let StartExecResults::Attached {
        mut output,
        mut input,
    } = docker
        .start_exec(&exec.id, None)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Err("Docker unexpectedly detached the interactive exec".to_string());
    };
    docker
        .resize_exec(
            &exec.id,
            ResizeExecOptions {
                height: rows,
                width: cols,
            },
        )
        .await
        .map_err(|error| error.to_string())?;

    let (mut tunnel_read, mut tunnel_write) = tokio::io::split(stream.compat());
    let to_container = async {
        tokio::io::copy(&mut tunnel_read, &mut input)
            .await
            .map_err(|error| error.to_string())?;
        input.shutdown().await.map_err(|error| error.to_string())
    };
    let from_container = async {
        while let Some(item) = futures::StreamExt::next(&mut output).await {
            let bytes = item.map_err(|error| error.to_string())?.into_bytes();
            tunnel_write
                .write_all(&bytes)
                .await
                .map_err(|error| error.to_string())?;
            tunnel_write
                .flush()
                .await
                .map_err(|error| error.to_string())?;
        }
        tunnel_write
            .shutdown()
            .await
            .map_err(|error| error.to_string())
    };
    let (input_result, output_result) = tokio::join!(to_container, from_container);
    input_result?;
    output_result
}

/// Connects the forwarded stream to the team's local service and pipes bytes.
async fn dial_and_pipe(stream: YamuxStream, service: &str) {
    let mut svc =
        match tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(service)).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                warn!("dial service {service}: {e}");
                return;
            }
            Err(_) => {
                warn!("dial service {service}: timeout");
                return;
            }
        };
    // Adapt the futures-io yamux stream to tokio's I/O traits and relay both ways.
    let mut server = stream.compat();
    let _ = tokio::io::copy_bidirectional(&mut svc, &mut server).await;
}

/// Reads [8-byte big-endian seq][flag] and sends the exact sequence-bound ACK only
/// after the atomic replacement succeeds (or the identical sequence/value was
/// already installed successfully). The sink mutex prevents a stale concurrent
/// stream from winning the rename.
async fn write_flag<S>(mut stream: S, flag_file: &str, sink: Arc<FlagSink>, generation: u64)
where
    S: FAsyncRead + FAsyncWrite + Unpin,
{
    let mut seq_buf = [0u8; 8];
    if stream.read_exact(&mut seq_buf).await.is_err() {
        return;
    }
    let seq = u64::from_be_bytes(seq_buf);

    let mut flag = Vec::new();
    let read_result = {
        let mut limited = (&mut stream).take((MAX_FLAG_BYTES + 1) as u64);
        limited.read_to_end(&mut flag).await
    };
    if read_result.is_err() || flag.is_empty() || flag.len() > MAX_FLAG_BYTES {
        return;
    }

    if !install_flag(flag_file, sink, generation, seq, &flag).await {
        return;
    }

    let mut ack = [0_u8; 9];
    ack[0] = FLAG_ACK;
    ack[1..].copy_from_slice(&seq.to_be_bytes());
    if stream.write_all(&ack).await.is_err()
        || stream.flush().await.is_err()
        || stream.close().await.is_err()
    {
        warn!(seq, "flag installed but acknowledgement could not be sent");
    }
}

static NEXT_FLAG_TEMP_ID: AtomicU64 = AtomicU64::new(1);

async fn install_flag(
    flag_file: &str,
    sink: Arc<FlagSink>,
    generation: u64,
    seq: u64,
    flag: &[u8],
) -> bool {
    let flag_file = flag_file.to_string();
    let flag = flag.to_vec();
    match tokio::task::spawn_blocking(move || {
        install_flag_blocking(&flag_file, &sink, generation, seq, &flag)
    })
    .await
    {
        Ok(installed) => installed,
        Err(error) => {
            warn!(%error, generation, seq, "flag install task failed");
            false
        }
    }
}

fn install_flag_blocking(
    flag_file: &str,
    sink: &FlagSink,
    generation: u64,
    seq: u64,
    flag: &[u8],
) -> bool {
    use std::io::Write as _;

    let mut state = match sink.state.lock() {
        Ok(state) => state,
        Err(_) => {
            warn!(generation, seq, "flag sink lock is poisoned");
            return false;
        }
    };
    if state.generation != generation {
        return false;
    }
    if let Some(current) = state.applied.as_ref() {
        if seq < current.sequence || (seq == current.sequence && current.value != flag) {
            return false;
        }
        if seq == current.sequence
            && std::fs::read(flag_file).is_ok_and(|installed| installed == flag)
        {
            return true;
        }
    }

    let flag_path = std::path::Path::new(flag_file);
    if let Some(parent) = flag_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        if let Err(error) = std::fs::create_dir_all(parent) {
            warn!(%error, generation, seq, "flag directory creation failed");
            return false;
        }
    }
    let operation = NEXT_FLAG_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let temporary = format!(
        "{flag_file}.tmp.{}.{}.{}.{}",
        std::process::id(),
        generation,
        seq,
        operation
    );
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(flag)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, flag_path)
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temporary);
        warn!(%error, generation, seq, "flag atomic replacement failed");
        return false;
    }
    state.applied = Some(AppliedFlag {
        sequence: seq,
        value: flag.to_vec(),
    });
    info!(generation, seq, bytes = flag.len(), "flag updated");
    true
}

#[cfg(test)]
mod websocket_stream_tests {
    use super::*;
    use futures::{SinkExt as _, StreamExt as _};
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::http::{Response as HttpResponse, StatusCode};
    use tokio_util::compat::TokioAsyncReadCompatExt;

    fn test_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "rsctf-byoc-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn rejection(
        status: StatusCode,
        state: &'static str,
        retry_after: Option<u64>,
    ) -> HttpResponse<Option<Vec<u8>>> {
        let mut builder = HttpResponse::builder()
            .status(status)
            .header(AGENT_STATE_HEADER, state);
        if let Some(seconds) = retry_after {
            builder = builder.header("retry-after", seconds.to_string());
        }
        builder.body(None).unwrap()
    }

    #[test]
    fn terminal_handshakes_stop_reconnecting() {
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::UPGRADE_REQUIRED,
        ] {
            let failure = handshake_failure(rejection(status, "terminal-revoked", None));
            assert!(failure.terminal);
            assert!(ReconnectPolicy::default().delay(&failure).is_none());
        }
    }

    #[test]
    fn transient_handshakes_honor_retry_after_and_cap_it() {
        let failure = handshake_failure(rejection(
            StatusCode::TOO_MANY_REQUESTS,
            "transient-overload",
            Some(60),
        ));
        assert!(!failure.terminal);
        let delay = ReconnectPolicy::default()
            .delay_with_sample(&failure, 0)
            .unwrap();
        assert!(
            delay >= Duration::from_secs(60) + MINIMUM_RECONNECT_DELAY
                && delay <= Duration::from_secs(61)
        );

        let capped = handshake_failure(rejection(
            StatusCode::TOO_EARLY,
            "retry-at-event-start",
            Some(u64::MAX),
        ));
        let capped_delay = ReconnectPolicy::default().delay_with_sample(&capped, 0);
        assert_eq!(capped_delay, Some(MAXIMUM_SERVER_RETRY_AFTER));
    }

    #[test]
    fn retry_after_cohorts_keep_a_jittered_spread() {
        let failure = handshake_failure(rejection(
            StatusCode::SERVICE_UNAVAILABLE,
            "transient-non-network-replica",
            Some(30),
        ));
        let samples = (0..256)
            .map(|sample| {
                ReconnectPolicy::default()
                    .delay_with_sample(&failure, sample * 7919)
                    .unwrap()
            })
            .collect::<std::collections::HashSet<_>>();
        assert!(samples.len() > 100, "Retry-After cohort jitter collapsed");
        assert!(samples.iter().all(|delay| {
            *delay >= Duration::from_secs(30) + MINIMUM_RECONNECT_DELAY
                && *delay <= Duration::from_secs(31)
        }));
    }

    #[test]
    fn reconnect_jitter_is_bounded_and_stable_sessions_reset_attempts() {
        let samples = (0..256)
            .map(|sample| jittered_reconnect_delay(12, sample * 7919))
            .collect::<std::collections::HashSet<_>>();
        assert!(samples.len() > 100, "reconnect jitter collapsed");
        assert!(samples.iter().all(|delay| {
            *delay >= MINIMUM_RECONNECT_DELAY && *delay <= MAXIMUM_RECONNECT_DELAY
        }));

        let mut policy = ReconnectPolicy::default();
        let short_failure = ConnectFailure::connected("closed", Duration::from_secs(2));
        for expected in 1..=17 {
            let _ = policy.delay(&short_failure).unwrap();
            assert_eq!(policy.attempt, expected);
        }
        let stable_failure = ConnectFailure::connected("closed", STABLE_TUNNEL_LIFETIME);
        let delay = policy.delay(&stable_failure).unwrap();
        assert_eq!(policy.attempt, 0);
        assert!(delay <= BASE_RECONNECT_DELAY);
    }

    #[tokio::test]
    async fn byte_stream_flattens_frames_and_writes_binary_messages() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut websocket = accept_async(socket).await.unwrap();
            websocket
                .send(Message::Binary(Bytes::from_static(b"binary-")))
                .await
                .unwrap();
            websocket.send(Message::Text("text".into())).await.unwrap();

            let message = websocket.next().await.unwrap().unwrap();
            assert_eq!(message, Message::Binary(Bytes::from_static(b"client")));
        });

        // An old server accepts the offered capability without selecting it.
        // This is the agent-first half of the rollout contract.
        let request = tunnel_request(&format!("ws://{address}")).unwrap();
        assert_eq!(
            request.headers().get(AGENT_PROTOCOL_HEADER),
            Some(&HeaderValue::from_static(AGENT_PROTOCOL))
        );
        let (websocket, response) = connect_async(request).await.unwrap();
        assert!(response.headers().get(AGENT_PROTOCOL_HEADER).is_none());
        let mut stream = WsByteStream::new(websocket);
        let mut first = [0_u8; 3];
        stream.read_exact(&mut first).await.unwrap();
        assert_eq!(&first, b"bin");

        let mut remainder = [0_u8; 8];
        stream.read_exact(&mut remainder).await.unwrap();
        assert_eq!(&remainder, b"ary-text");

        stream.write_all(b"client").await.unwrap();
        stream.flush().await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn flag_install_is_atomic_ordered_and_idempotent() {
        let directory = test_path("flag-install");
        let flag_file = directory.join("flag");
        let sink = Arc::new(FlagSink::default());
        let generation = sink.begin_connection().await.unwrap();
        let path = flag_file.to_str().unwrap();

        assert!(install_flag(path, sink.clone(), generation, 10, b"first").await);
        assert_eq!(tokio::fs::read(&flag_file).await.unwrap(), b"first");
        assert!(install_flag(path, sink.clone(), generation, 10, b"first").await);
        assert!(!install_flag(path, sink.clone(), generation, 10, b"forged").await);
        assert!(!install_flag(path, sink.clone(), generation, 9, b"stale").await);
        assert_eq!(tokio::fs::read(&flag_file).await.unwrap(), b"first");
        assert!(install_flag(path, sink, generation, 11, b"second").await);
        assert_eq!(tokio::fs::read(&flag_file).await.unwrap(), b"second");

        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn reconnect_keeps_monotonic_flag_state_and_fences_the_old_generation() {
        let directory = test_path("flag-generation");
        let flag_file = directory.join("flag");
        let path = flag_file.to_str().unwrap();
        let sink = Arc::new(FlagSink::default());
        let old_generation = sink.begin_connection().await.unwrap();
        assert!(install_flag(path, sink.clone(), old_generation, 80, b"old").await);

        let new_generation = sink.begin_connection().await.unwrap();
        assert!(install_flag(path, sink.clone(), new_generation, 80, b"old").await);
        assert!(!install_flag(path, sink.clone(), new_generation, 79, b"stale").await);
        assert!(install_flag(path, sink.clone(), new_generation, 81, b"new").await);
        assert!(!install_flag(path, sink, old_generation, 82, b"late-old").await);
        assert_eq!(tokio::fs::read(&flag_file).await.unwrap(), b"new");

        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn equal_retry_repairs_mutated_or_deleted_flag_and_cleans_temporaries() {
        let directory = test_path("flag-repair");
        let flag_file = directory.join("flag");
        let path = flag_file.to_str().unwrap();
        let sink = Arc::new(FlagSink::default());
        let generation = sink.begin_connection().await.unwrap();
        assert!(install_flag(path, sink.clone(), generation, 9, b"expected").await);

        tokio::fs::write(&flag_file, b"mutated").await.unwrap();
        assert!(install_flag(path, sink.clone(), generation, 9, b"expected").await);
        assert_eq!(tokio::fs::read(&flag_file).await.unwrap(), b"expected");

        tokio::fs::remove_file(&flag_file).await.unwrap();
        assert!(install_flag(path, sink, generation, 9, b"expected").await);
        assert_eq!(tokio::fs::read(&flag_file).await.unwrap(), b"expected");
        let entries = std::fs::read_dir(&directory).unwrap().count();
        assert_eq!(entries, 1, "flag temporary files were not cleaned");

        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn flag_stream_acks_only_after_install_with_the_exact_sequence() {
        let directory = test_path("flag-ack");
        let flag_file = directory.join("flag");
        let path = flag_file.to_string_lossy().into_owned();
        let sink = Arc::new(FlagSink::default());
        let generation = sink.begin_connection().await.unwrap();
        let (server, agent) = tokio::io::duplex(128);
        let handler = tokio::spawn({
            let sink = sink.clone();
            async move { write_flag(agent.compat(), &path, sink, generation).await }
        });
        let mut server = server.compat();
        server.write_all(&71_u64.to_be_bytes()).await.unwrap();
        server.write_all(b"installed").await.unwrap();
        server.close().await.unwrap();
        let mut ack = [0_u8; 9];
        server.read_exact(&mut ack).await.unwrap();
        assert_eq!(ack[0], FLAG_ACK);
        assert_eq!(u64::from_be_bytes(ack[1..].try_into().unwrap()), 71);
        handler.await.unwrap();
        assert_eq!(tokio::fs::read(&flag_file).await.unwrap(), b"installed");

        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn invalid_flag_stream_sends_no_ack() {
        let directory = test_path("flag-no-ack");
        let flag_file = directory.join("flag");
        let path = flag_file.to_string_lossy().into_owned();
        let sink = Arc::new(FlagSink::default());
        let generation = sink.begin_connection().await.unwrap();
        let (server, agent) = tokio::io::duplex(128);
        let handler = tokio::spawn({
            let sink = sink.clone();
            async move { write_flag(agent.compat(), &path, sink, generation).await }
        });
        let mut server = server.compat();
        server.write_all(&72_u64.to_be_bytes()).await.unwrap();
        server.close().await.unwrap();
        let mut ack = [0_u8; 1];
        assert_eq!(server.read(&mut ack).await.unwrap(), 0);
        handler.await.unwrap();
        assert!(!flag_file.exists());
    }
}
