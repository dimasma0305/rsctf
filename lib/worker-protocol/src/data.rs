use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

use crate::{SessionFence, WorkloadFence, PROTOCOL_REVISION};

pub const DATA_MAGIC: &[u8; 4] = b"RSD1";
pub const MAX_DATA_HEADER: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataHello {
    pub protocol_revision: u16,
    pub worker_id: Uuid,
    #[serde(flatten)]
    pub session: SessionFence,
    pub lane: u16,
}

impl DataHello {
    pub fn new(worker_id: Uuid, session: SessionFence, lane: u16) -> Self {
        Self {
            protocol_revision: PROTOCOL_REVISION,
            worker_id,
            session,
            lane,
        }
    }
}

/// Explicit acceptance sent before both peers switch the TLS stream to yamux.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataWelcome {
    pub protocol_revision: u16,
    #[serde(flatten)]
    pub session: SessionFence,
    pub lane: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum DataStreamKind {
    TcpProxy = 1,
    InteractiveExec = 2,
}

impl TryFrom<u8> for DataStreamKind {
    type Error = DataStreamError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::TcpProxy),
            2 => Ok(Self::InteractiveExec),
            other => Err(DataStreamError::UnknownKind(other)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TcpProxyRequest {
    #[serde(flatten)]
    pub fence: WorkloadFence,
    pub service: String,
    pub port: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replica: Option<u16>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InteractiveExecRequest {
    #[serde(flatten)]
    pub fence: WorkloadFence,
    pub service: String,
    pub replica: u16,
    pub columns: u16,
    pub rows: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataStreamRequest {
    TcpProxy(TcpProxyRequest),
    InteractiveExec(InteractiveExecRequest),
}

impl DataStreamRequest {
    pub fn kind(&self) -> DataStreamKind {
        match self {
            Self::TcpProxy(_) => DataStreamKind::TcpProxy,
            Self::InteractiveExec(_) => DataStreamKind::InteractiveExec,
        }
    }

    pub fn workload_fence(&self) -> WorkloadFence {
        match self {
            Self::TcpProxy(request) => request.fence,
            Self::InteractiveExec(request) => request.fence,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum DataStatus {
    Ready = 0,
    NotFound = 1,
    Stale = 2,
    Busy = 3,
    Forbidden = 4,
    RuntimeUnavailable = 5,
    DialFailed = 6,
    Unsupported = 7,
    Internal = 8,
}

impl DataStatus {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    pub async fn write<W>(self, writer: &mut W) -> Result<(), std::io::Error>
    where
        W: AsyncWrite + Unpin,
    {
        writer.write_u8(self.as_u8()).await?;
        writer.flush().await
    }
}

impl TryFrom<u8> for DataStatus {
    type Error = DataStreamError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Ready),
            1 => Ok(Self::NotFound),
            2 => Ok(Self::Stale),
            3 => Ok(Self::Busy),
            4 => Ok(Self::Forbidden),
            5 => Ok(Self::RuntimeUnavailable),
            6 => Ok(Self::DialFailed),
            7 => Ok(Self::Unsupported),
            8 => Ok(Self::Internal),
            other => Err(DataStreamError::UnknownStatus(other)),
        }
    }
}

#[derive(Debug, Error)]
pub enum DataStreamError {
    #[error("data stream I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid data stream magic")]
    InvalidMagic,
    #[error("unknown data stream kind {0}")]
    UnknownKind(u8),
    #[error("unknown data status {0}")]
    UnknownStatus(u8),
    #[error("data header is empty")]
    EmptyHeader,
    #[error("data header is {actual} bytes, exceeding the {maximum}-byte limit")]
    HeaderTooLarge { actual: usize, maximum: usize },
    #[error("invalid data header JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// Writes `RSD1`, kind, flags, a u16 JSON length, and the typed request header.
pub async fn write_data_request<W>(
    writer: &mut W,
    request: &DataStreamRequest,
) -> Result<(), DataStreamError>
where
    W: AsyncWrite + Unpin,
{
    let header = match request {
        DataStreamRequest::TcpProxy(value) => serde_json::to_vec(value)?,
        DataStreamRequest::InteractiveExec(value) => serde_json::to_vec(value)?,
    };
    if header.is_empty() {
        return Err(DataStreamError::EmptyHeader);
    }
    if header.len() > MAX_DATA_HEADER {
        return Err(DataStreamError::HeaderTooLarge {
            actual: header.len(),
            maximum: MAX_DATA_HEADER,
        });
    }

    writer.write_all(DATA_MAGIC).await?;
    writer.write_u8(request.kind() as u8).await?;
    writer.write_u8(0).await?;
    writer.write_u16(header.len() as u16).await?;
    writer.write_all(&header).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_data_request<R>(reader: &mut R) -> Result<DataStreamRequest, DataStreamError>
where
    R: AsyncRead + Unpin,
{
    let mut magic = [0_u8; 4];
    reader.read_exact(&mut magic).await?;
    if &magic != DATA_MAGIC {
        return Err(DataStreamError::InvalidMagic);
    }
    let kind = DataStreamKind::try_from(reader.read_u8().await?)?;
    let _flags = reader.read_u8().await?;
    let length = reader.read_u16().await? as usize;
    if length == 0 {
        return Err(DataStreamError::EmptyHeader);
    }
    if length > MAX_DATA_HEADER {
        return Err(DataStreamError::HeaderTooLarge {
            actual: length,
            maximum: MAX_DATA_HEADER,
        });
    }
    let mut header = vec![0_u8; length];
    reader.read_exact(&mut header).await?;
    match kind {
        DataStreamKind::TcpProxy => Ok(DataStreamRequest::TcpProxy(serde_json::from_slice(
            &header,
        )?)),
        DataStreamKind::InteractiveExec => Ok(DataStreamRequest::InteractiveExec(
            serde_json::from_slice(&header)?,
        )),
    }
}

pub async fn read_data_status<R>(reader: &mut R) -> Result<DataStatus, DataStreamError>
where
    R: AsyncRead + Unpin,
{
    DataStatus::try_from(reader.read_u8().await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn data_request_round_trip() {
        let expected = DataStreamRequest::TcpProxy(TcpProxyRequest {
            fence: WorkloadFence {
                workload_id: Uuid::new_v4(),
                assignment_id: Uuid::new_v4(),
                generation: 3,
            },
            service: "challenge".to_string(),
            port: "service".to_string(),
            replica: None,
        });
        let mut wire = Vec::new();
        write_data_request(&mut wire, &expected).await.unwrap();
        let actual = read_data_request(&mut wire.as_slice()).await.unwrap();
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn status_is_exactly_one_byte() {
        let mut wire = Vec::new();
        DataStatus::Unsupported.write(&mut wire).await.unwrap();
        assert_eq!(wire, vec![7]);
        assert_eq!(
            read_data_status(&mut wire.as_slice()).await.unwrap(),
            DataStatus::Unsupported
        );
    }
}
