//! agent mode: the team-side end of the tunnel.
//!
//! It dials OUTBOUND to the relay's control port over a WebSocket (through RSCTF's
//! existing WS<->TCP proxy bridge), runs a yamux CLIENT over the WebSocket-as-byte-
//! stream, accepts the streams the relay forwards, and dispatches each by its
//! leading type byte:
//!
//!   'S' service — dial the team's local service and pipe raw bytes.
//!   'F' flag    — read [8-byte big-endian seq][flag] and atomically write the
//!                 newest flag to the flag file (temp file + rename).
//!   'E' exec    — open an interactive shell in the configured service container.
//!
//! Only one outbound connection is needed; no inbound port / public IP / VPN.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use bollard::exec::{CreateExecOptions, ResizeExecOptions, StartExecResults};
use bollard::Docker;
use futures::io::{AsyncRead as FAsyncRead, AsyncReadExt, AsyncWrite as FAsyncWrite};
use futures::{future, Sink, Stream};
use tokio::io::AsyncWriteExt as TokioAsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::{Bytes, Message};
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tokio_util::compat::FuturesAsyncReadCompatExt;
use tracing::{info, warn};
use yamux::{Connection, Mode, Stream as YamuxStream};

use crate::{
    env, must_env, yamux_config, RECONNECT_DELAY, STREAM_EXEC, STREAM_FLAG, STREAM_SERVICE,
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

/// Serializes flag writes and drops stale (lower-seq) flags. Reset per connection
/// so a relay restart (seq back to 0) still delivers.
struct FlagSink {
    last: Mutex<u64>,
}

pub async fn run_agent() {
    let tunnel_url = must_env("RSCTF_BYOC_TUNNEL_URL"); // wss://rsctf/api/Ad/Byoc/Agent/<token>
    let service = must_env("RSCTF_BYOC_SERVICE"); // host:port of the team's service
    let flag_file = env("RSCTF_BYOC_FLAG_FILE", "/flag"); // where to write the rotating flag
    let service_container = env("RSCTF_BYOC_SERVICE_CONTAINER", "");

    loop {
        if let Err(e) = connect_once(&tunnel_url, &service, &flag_file, &service_container).await {
            warn!("tunnel: {e}; reconnecting in 3s");
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

/// Holds one tunnel session: dial the WebSocket, run a yamux client over it, and
/// serve forwarded streams until the connection drops.
async fn connect_once(
    tunnel_url: &str,
    service: &str,
    flag_file: &str,
    service_container: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (ws, _resp) = connect_async(tunnel_url).await?;
    let socket = WsByteStream::new(ws);

    let mut conn = Connection::new(socket, yamux_config(), Mode::Client);
    info!(service, "tunnel connected");

    // Per-connection flag serializer: a monotonic seq + mutex so two flag streams
    // (e.g. a reconnect replay racing a fresh push) can't let the older flag win
    // the rename.
    let sink = Arc::new(FlagSink {
        last: Mutex::new(0),
    });

    // The accept loop is also the driver: continuously polling `poll_next_inbound`
    // is what makes the whole yamux connection (and every accepted stream's I/O)
    // progress in yamux 0.13.
    loop {
        match future::poll_fn(|cx| conn.poll_next_inbound(cx)).await {
            Some(Ok(stream)) => {
                let service = service.to_string();
                let flag_file = flag_file.to_string();
                let service_container = service_container.to_string();
                let sink = sink.clone();
                tokio::spawn(handle_stream(
                    stream,
                    service,
                    flag_file,
                    service_container,
                    sink,
                ));
            }
            Some(Err(e)) => return Err(Box::new(e)),
            None => return Err("tunnel closed".into()),
        }
    }
}

/// Dispatches one forwarded stream by its leading type byte.
async fn handle_stream(
    mut stream: YamuxStream,
    service: String,
    flag_file: String,
    service_container: String,
    sink: Arc<FlagSink>,
) {
    let mut hdr = [0u8; 1];
    if stream.read_exact(&mut hdr).await.is_err() {
        return;
    }
    match hdr[0] {
        STREAM_SERVICE => dial_and_pipe(stream, &service).await,
        STREAM_FLAG => write_flag(stream, &flag_file, &sink).await,
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

/// Reads [8-byte big-endian seq][flag] and, if seq is newer than any flag already
/// applied, writes it atomically to the flag file (temp file + rename). The sink's
/// mutex serializes the write+rename, so an older flag that raced a newer one can
/// never win the rename.
async fn write_flag(mut stream: YamuxStream, flag_file: &str, sink: &FlagSink) {
    let mut seq_buf = [0u8; 8];
    if stream.read_exact(&mut seq_buf).await.is_err() {
        return;
    }
    let seq = u64::from_be_bytes(seq_buf);

    let mut flag = Vec::new();
    let mut limited = stream.take(4096);
    if limited.read_to_end(&mut flag).await.is_err() || flag.is_empty() {
        return;
    }

    let mut last = sink.last.lock().await;
    if seq <= *last {
        return; // a newer flag already landed
    }

    let tmp = format!("{flag_file}.tmp");
    if let Some(parent) = std::path::Path::new(flag_file).parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                warn!("flag dir: {e}");
                return;
            }
        }
    }
    if let Err(e) = tokio::fs::write(&tmp, &flag).await {
        warn!("flag write: {e}");
        return;
    }
    if let Err(e) = tokio::fs::rename(&tmp, flag_file).await {
        warn!("flag rename: {e}");
        return;
    }
    *last = seq;
    info!("flag updated (seq {seq}, {} bytes)", flag.len());
}

#[cfg(test)]
mod websocket_stream_tests {
    use super::*;
    use futures::io::AsyncWriteExt as _;
    use futures::{SinkExt as _, StreamExt as _};
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

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

        let (websocket, _) = connect_async(format!("ws://{address}")).await.unwrap();
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
}
