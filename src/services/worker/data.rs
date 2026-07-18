use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use futures::io::{AsyncRead, AsyncWrite};
use rsctf_worker_protocol::{
    read_data_status, write_data_request, write_json_frame, DataStatus, DataStreamRequest,
    DataWelcome, PROTOCOL_REVISION,
};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch, OwnedSemaphorePermit, Semaphore};
use tokio_rustls::server::TlsStream;
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use uuid::Uuid;

use super::registry::{SessionContext, WorkerRegistry};
use super::{WorkerError, WorkerResult};

const DEFAULT_MAX_STREAMS: usize = 64;
const DEFAULT_RECEIVE_WINDOW: usize = 16 * 1024 * 1024;

pub struct WorkerDataStream {
    inner: Compat<yamux::Stream>,
    _permit: OwnedSemaphorePermit,
}

impl tokio::io::AsyncRead for WorkerDataStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buffer)
    }
}

impl tokio::io::AsyncWrite for WorkerDataStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buffer)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[derive(Clone, Debug)]
pub struct DataConfig {
    pub max_streams_per_lane: usize,
    pub max_connection_receive_window: usize,
    pub pending_open_capacity: usize,
    pub open_timeout: Duration,
    pub status_timeout: Duration,
}

impl Default for DataConfig {
    fn default() -> Self {
        Self {
            max_streams_per_lane: DEFAULT_MAX_STREAMS,
            max_connection_receive_window: DEFAULT_RECEIVE_WINDOW,
            pending_open_capacity: DEFAULT_MAX_STREAMS,
            open_timeout: Duration::from_secs(5),
            status_timeout: Duration::from_secs(5),
        }
    }
}

type OpenResult = Result<(yamux::Stream, OwnedSemaphorePermit), ()>;

struct OpenRequest {
    response: oneshot::Sender<OpenResult>,
    permit: OwnedSemaphorePermit,
}

#[derive(Clone)]
pub(crate) struct DataLane {
    id: Uuid,
    open: mpsc::Sender<OpenRequest>,
    shutdown: watch::Sender<bool>,
    streams: Arc<Semaphore>,
    config: DataConfig,
}

impl DataLane {
    fn new(config: DataConfig) -> (Self, mpsc::Receiver<OpenRequest>, watch::Receiver<bool>) {
        let (open, requests) = mpsc::channel(config.pending_open_capacity);
        let (shutdown, shutdown_rx) = watch::channel(false);
        (
            Self {
                id: Uuid::new_v4(),
                open,
                shutdown,
                streams: Arc::new(Semaphore::new(config.max_streams_per_lane)),
                config,
            },
            requests,
            shutdown_rx,
        )
    }

    pub(crate) fn id(&self) -> Uuid {
        self.id
    }

    pub(crate) fn shutdown(&self) {
        let _ = self.shutdown.send(true);
    }

    pub(crate) async fn open(&self, request: DataStreamRequest) -> WorkerResult<WorkerDataStream> {
        // yamux 0.14 treats `TooManyStreams` as a connection error and starts
        // tearing the connection down before returning it. Admission therefore
        // happens outside yamux so saturation rejects only this open request.
        let permit = self
            .streams
            .clone()
            .try_acquire_owned()
            .map_err(|_| WorkerError::Busy)?;
        let (response, stream) = oneshot::channel();
        self.open
            .try_send(OpenRequest { response, permit })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => WorkerError::Busy,
                mpsc::error::TrySendError::Closed(_) => WorkerError::Offline,
            })?;
        // The queued request owns its admission permit until the driver hands
        // both back. A caller timeout therefore cannot over-admit a replacement
        // while its stale request is still waiting to be drained.
        let (raw, permit) = tokio::time::timeout(self.config.open_timeout, stream)
            .await
            .map_err(|_| WorkerError::Busy)?
            .map_err(|_| WorkerError::Offline)?
            .map_err(|_| WorkerError::Offline)?;
        let mut stream = raw.compat();
        write_data_request(&mut stream, &request).await?;
        let status =
            tokio::time::timeout(self.config.status_timeout, read_data_status(&mut stream))
                .await
                .map_err(|_| WorkerError::Busy)??;
        if status != DataStatus::Ready {
            return Err(WorkerError::DataStatus(status));
        }
        Ok(WorkerDataStream {
            inner: stream,
            _permit: permit,
        })
    }
}

pub(crate) async fn drive_data_lane(
    registry: WorkerRegistry,
    session: SessionContext,
    lane_number: u16,
    mut tls: TlsStream<TcpStream>,
    config: DataConfig,
) -> WorkerResult<()> {
    if config.max_streams_per_lane == 0
        || config.pending_open_capacity == 0
        || config.max_connection_receive_window < 256 * 1024 * config.max_streams_per_lane
    {
        return Err(WorkerError::Protocol("invalid data-lane limits"));
    }

    let (lane, requests, mut lane_shutdown) = DataLane::new(config.clone());
    let lane_id = lane.id();
    let mut session_shutdown = registry
        .register_lane(session.worker_id, &session.fence, lane_number, lane.clone())
        .await?;

    let result = async {
        let welcome = DataWelcome {
            protocol_revision: PROTOCOL_REVISION,
            session: session.fence,
            lane: lane_number,
        };
        tokio::time::timeout(config.status_timeout, write_json_frame(&mut tls, &welcome))
            .await
            .map_err(|_| WorkerError::Protocol("data welcome timed out"))??;

        let mut yamux_config = yamux::Config::default();
        yamux_config.set_max_num_streams(config.max_streams_per_lane);
        yamux_config.set_max_connection_receive_window(Some(config.max_connection_receive_window));
        let connection = yamux::Connection::new(tls.compat(), yamux_config, yamux::Mode::Server);
        tokio::select! {
            result = drive_yamux(connection, requests, config.pending_open_capacity) => result,
            changed = lane_shutdown.changed() => {
                let _ = changed;
                Ok(())
            }
            changed = session_shutdown.changed() => {
                let _ = changed;
                Ok(())
            }
        }
    }
    .await;
    registry
        .remove_lane_if(session.worker_id, &session.fence, lane_number, lane_id)
        .await;
    result
}

async fn drive_yamux<T>(
    mut connection: yamux::Connection<T>,
    mut requests: mpsc::Receiver<OpenRequest>,
    max_waiting: usize,
) -> WorkerResult<()>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let mut waiting = VecDeque::<OpenRequest>::new();
    std::future::poll_fn(|cx| {
        // Pump stream close/reset notifications before considering new opens. This
        // lets a permit become reusable as soon as its previous stream is dropped.
        // A trusted worker is the yamux client and may only accept streams.
        match connection.poll_next_inbound(cx) {
            Poll::Ready(Some(Ok(_))) => {
                return Poll::Ready(Err(WorkerError::Protocol(
                    "worker opened an inbound yamux stream",
                )));
            }
            Poll::Ready(Some(Err(_))) => return Poll::Ready(Err(WorkerError::Offline)),
            Poll::Ready(None) => return Poll::Ready(Ok(())),
            Poll::Pending => {}
        }

        while waiting.len() < max_waiting {
            let Poll::Ready(request) = requests.poll_recv(cx) else {
                break;
            };
            match request {
                Some(request) => waiting.push_back(request),
                None => return Poll::Ready(Ok(())),
            }
        }
        while !waiting.is_empty() {
            match connection.poll_new_outbound(cx) {
                Poll::Ready(Ok(stream)) => {
                    if let Some(OpenRequest { response, permit }) = waiting.pop_front() {
                        let _ = response.send(Ok((stream, permit)));
                    }
                }
                Poll::Ready(Err(_)) => {
                    if let Some(OpenRequest { response, .. }) = waiting.pop_front() {
                        let _ = response.send(Err(()));
                    }
                    return Poll::Ready(Err(WorkerError::Offline));
                }
                Poll::Pending => break,
            }
        }
        Poll::Pending
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future;
    use rsctf_worker_protocol::{read_data_request, DataStatus};
    use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};

    #[test]
    fn default_window_respects_yamux_floor() {
        let config = DataConfig::default();
        assert!(config.max_connection_receive_window >= 256 * 1024 * config.max_streams_per_lane);
    }

    #[tokio::test]
    async fn full_open_queue_fails_closed() {
        let config = DataConfig {
            pending_open_capacity: 1,
            open_timeout: Duration::from_millis(1),
            ..DataConfig::default()
        };
        let (lane, _requests, _shutdown) = DataLane::new(config);
        let request = test_request();
        let first = lane.open(request.clone());
        let second = lane.open(request);
        let (first, second) = tokio::join!(first, second);
        assert!(matches!(first, Err(WorkerError::Busy)));
        assert!(matches!(second, Err(WorkerError::Busy)));
    }

    #[tokio::test]
    async fn timed_out_open_keeps_its_permit_until_the_queue_is_drained() {
        let config = DataConfig {
            max_streams_per_lane: 1,
            pending_open_capacity: 1,
            open_timeout: Duration::from_millis(1),
            ..DataConfig::default()
        };
        let (lane, mut requests, _shutdown) = DataLane::new(config);

        assert!(matches!(
            lane.open(test_request()).await,
            Err(WorkerError::Busy)
        ));
        assert!(matches!(
            lane.open(test_request()).await,
            Err(WorkerError::Busy)
        ));

        drop(requests.recv().await.unwrap());
        assert_eq!(lane.streams.available_permits(), 1);
    }

    #[tokio::test]
    async fn stream_saturation_rejects_only_that_open_and_lane_recovers() {
        let config = DataConfig {
            max_streams_per_lane: 1,
            max_connection_receive_window: 256 * 1024,
            pending_open_capacity: 4,
            open_timeout: Duration::from_secs(1),
            status_timeout: Duration::from_secs(1),
        };
        let (lane, requests, _shutdown) = DataLane::new(config.clone());
        let (server_io, client_io) = tokio::io::duplex(64 * 1024);

        let mut yamux_config = yamux::Config::default();
        yamux_config.set_max_num_streams(config.max_streams_per_lane);
        yamux_config.set_max_connection_receive_window(Some(config.max_connection_receive_window));
        let server = yamux::Connection::new(server_io.compat(), yamux_config, yamux::Mode::Server);
        let driver = tokio::spawn(drive_yamux(server, requests, config.pending_open_capacity));

        let (release, release_rx) = watch::channel(false);
        let (accepted_tx, mut accepted_rx) = mpsc::unbounded_channel();
        let client = tokio::spawn(async move {
            let mut connection = yamux::Connection::new(
                client_io.compat(),
                yamux::Config::default(),
                yamux::Mode::Client,
            );
            let mut stream_number = 0_u32;
            loop {
                match future::poll_fn(|cx| connection.poll_next_inbound(cx)).await {
                    Some(Ok(stream)) => {
                        let current = stream_number;
                        stream_number += 1;
                        let accepted_tx = accepted_tx.clone();
                        let mut release_rx = release_rx.clone();
                        tokio::spawn(async move {
                            let mut stream = stream.compat();
                            read_data_request(&mut stream).await.unwrap();
                            DataStatus::Ready.write(&mut stream).await.unwrap();
                            accepted_tx.send(current).unwrap();
                            let _ = release_rx.changed().await;
                        });
                    }
                    Some(Err(error)) => panic!("client yamux failed: {error}"),
                    None => break,
                }
            }
        });

        let first = lane.open(test_request()).await.unwrap();
        assert_eq!(accepted_rx.recv().await, Some(0));

        let saturated = lane.open(test_request()).await;
        assert!(matches!(saturated, Err(WorkerError::Busy)));
        assert!(!driver.is_finished(), "saturation tore down the data lane");

        drop(first);
        tokio::task::yield_now().await;
        let reopened = lane.open(test_request()).await.unwrap();
        assert_eq!(accepted_rx.recv().await, Some(1));
        assert!(!driver.is_finished());

        drop(reopened);
        let _ = release.send(true);
        drop(lane);
        assert!(driver.await.unwrap().is_ok());
        client.abort();
    }

    fn test_request() -> DataStreamRequest {
        use rsctf_worker_protocol::{TcpProxyRequest, WorkloadFence};
        DataStreamRequest::TcpProxy(TcpProxyRequest {
            fence: WorkloadFence {
                workload_id: Uuid::new_v4(),
                assignment_id: Uuid::new_v4(),
                generation: 1,
            },
            service: "challenge".to_string(),
            port: "service".to_string(),
            replica: None,
        })
    }
}
