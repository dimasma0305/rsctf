use std::io;

/// Errors returned by the trusted worker plane.
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error("worker authentication failed")]
    Authentication,
    #[error("worker is not authorized for this operation")]
    Authorization,
    #[error("worker protocol violation: {0}")]
    Protocol(&'static str),
    #[error("worker protocol violation: {0}")]
    ProtocolOwned(String),
    #[error("worker registry is at capacity")]
    RegistryFull,
    #[error("worker is offline")]
    Offline,
    #[error("worker session is stale")]
    StaleSession,
    #[error("worker data lanes are busy")]
    Busy,
    #[error("worker authority failed: {0}")]
    Authority(String),
    #[error("worker returned data status {0:?}")]
    DataStatus(rsctf_worker_protocol::DataStatus),
    #[error("worker frame error: {0}")]
    Frame(#[from] rsctf_worker_protocol::FrameError),
    #[error("worker data stream error: {0}")]
    DataStream(#[from] rsctf_worker_protocol::DataStreamError),
    #[error("worker TLS error: {0}")]
    Tls(#[from] tokio_rustls::rustls::Error),
    #[error("worker I/O error: {0}")]
    Io(#[from] io::Error),
}

pub type WorkerResult<T> = Result<T, WorkerError>;
