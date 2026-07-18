use std::time::Duration;

use futures_util::future;
use rsctf_worker_protocol::{
    read_data_request, read_json_frame, write_json_frame, CommandErrorCode, DataHello, DataStatus,
    DataStreamRequest, DataWelcome, SessionFence,
};
use tokio::io::AsyncWriteExt;
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use uuid::Uuid;
use yamux::{Connection, Mode, Stream};

use super::{validate_revision, ClientError, SESSION_NEGOTIATION_TIMEOUT};
use crate::backoff::Backoff;
use crate::runtime::{RuntimeError, SharedRuntime};
use crate::tls::MtlsConnector;

pub async fn run_reconnecting(
    connector: MtlsConnector,
    worker_id: Uuid,
    session: SessionFence,
    runtime: SharedRuntime,
    lane_number: u16,
) {
    let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(30));
    loop {
        if let Err(error) =
            run_lane(&connector, worker_id, session, runtime.clone(), lane_number).await
        {
            tracing::warn!(lane_number, %error, "worker data lane failed");
        }
        tokio::time::sleep(backoff.next_delay()).await;
    }
}

async fn run_lane(
    connector: &MtlsConnector,
    worker_id: Uuid,
    session: SessionFence,
    runtime: SharedRuntime,
    lane_number: u16,
) -> Result<(), ClientError> {
    let mut stream = connector.connect_data().await?;
    let hello = DataHello::new(worker_id, session, lane_number);
    let welcome: DataWelcome = tokio::time::timeout(SESSION_NEGOTIATION_TIMEOUT, async {
        write_json_frame(&mut stream, &hello).await?;
        read_json_frame(&mut stream).await
    })
    .await
    .map_err(|_| ClientError::Transport("data-lane negotiation timed out".to_string()))??;
    validate_revision(welcome.protocol_revision)?;
    if welcome.session != session || welcome.lane != lane_number {
        return Err(ClientError::Protocol(
            "server accepted the data lane for a different session or lane".to_string(),
        ));
    }

    let mut connection = Connection::new(stream.compat(), yamux::Config::default(), Mode::Client);
    tracing::info!(session_id = %session.session_id, lane_number, "worker data lane established");
    loop {
        match future::poll_fn(|context| connection.poll_next_inbound(context)).await {
            Some(Ok(stream)) => {
                let runtime = runtime.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_stream(stream, runtime).await {
                        tracing::debug!(%error, "worker data stream closed with an error");
                    }
                });
            }
            Some(Err(error)) => return Err(ClientError::Transport(error.to_string())),
            None => return Err(ClientError::Transport("data lane closed".to_string())),
        }
    }
}

async fn handle_stream(stream: Stream, runtime: SharedRuntime) -> Result<(), ClientError> {
    let mut stream = stream.compat();
    let request = read_data_request(&mut stream)
        .await
        .map_err(|error| ClientError::Protocol(error.to_string()))?;
    match request {
        DataStreamRequest::TcpProxy(request) => match runtime.open_tcp(&request).await {
            Ok(mut container) => {
                DataStatus::Ready
                    .write(&mut stream)
                    .await
                    .map_err(|error| ClientError::Transport(error.to_string()))?;
                let _ = tokio::io::copy_bidirectional(&mut stream, &mut container).await;
                let _ = stream.shutdown().await;
            }
            Err(error) => {
                tracing::debug!(
                    workload_id = %request.fence.workload_id,
                    assignment_id = %request.fence.assignment_id,
                    generation = request.fence.generation,
                    service = %request.service,
                    port = %request.port,
                    code = ?error.code,
                    error = %error,
                    "worker TCP proxy target rejected"
                );
                write_error_status(&mut stream, &error).await?;
            }
        },
        DataStreamRequest::InteractiveExec(request) => match runtime.open_exec(&request).await {
            Ok(()) => {
                // Runtime implementations must take ownership of streaming before they
                // advertise this capability. The Docker v1 runtime returns unsupported.
                DataStatus::Internal
                    .write(&mut stream)
                    .await
                    .map_err(|error| ClientError::Transport(error.to_string()))?;
            }
            Err(error) => write_error_status(&mut stream, &error).await?,
        },
    }
    Ok(())
}

async fn write_error_status<W>(writer: &mut W, error: &RuntimeError) -> Result<(), ClientError>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let status = match error.code {
        CommandErrorCode::NotFound => DataStatus::NotFound,
        CommandErrorCode::StaleSession
        | CommandErrorCode::StaleAssignment
        | CommandErrorCode::StaleGeneration
        | CommandErrorCode::StaleFlagSequence
        | CommandErrorCode::SpecConflict => DataStatus::Stale,
        CommandErrorCode::RuntimeUnavailable => DataStatus::RuntimeUnavailable,
        CommandErrorCode::Unsupported => DataStatus::Unsupported,
        CommandErrorCode::Timeout => DataStatus::DialFailed,
        CommandErrorCode::InvalidSpec => DataStatus::Forbidden,
        CommandErrorCode::PartialFailure | CommandErrorCode::Internal => DataStatus::Internal,
    };
    status
        .write(writer)
        .await
        .map_err(|write_error| ClientError::Transport(write_error.to_string()))
}
