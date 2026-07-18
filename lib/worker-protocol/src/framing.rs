use std::io;

use serde::de::DeserializeOwned;
use serde::Serialize;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum JSON payload carried in a control or connection-handshake frame.
pub const MAX_CONTROL_FRAME: usize = 256 * 1024;

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("frame payload is empty")]
    Empty,
    #[error("frame payload is {actual} bytes, exceeding the {maximum}-byte limit")]
    TooLarge { actual: usize, maximum: usize },
    #[error("frame I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("frame contains invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// Writes one big-endian u32 length followed by a UTF-8 JSON payload.
pub async fn write_json_frame<W, T>(writer: &mut W, value: &T) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize + ?Sized,
{
    let payload = serde_json::to_vec(value)?;
    if payload.is_empty() {
        return Err(FrameError::Empty);
    }
    if payload.len() > MAX_CONTROL_FRAME {
        return Err(FrameError::TooLarge {
            actual: payload.len(),
            maximum: MAX_CONTROL_FRAME,
        });
    }

    writer.write_u32(payload.len() as u32).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

/// Reads one length-prefixed JSON frame while enforcing the limit before allocation.
pub async fn read_json_frame<R, T>(reader: &mut R) -> Result<T, FrameError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let (value, _) = read_json_frame_counted(reader).await?;
    Ok(value)
}

/// Reads one bounded frame and returns its wire payload length. Servers can
/// enforce byte-rate quotas without serializing the decoded value a second
/// time.
pub async fn read_json_frame_counted<R, T>(reader: &mut R) -> Result<(T, usize), FrameError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let length = reader.read_u32().await? as usize;
    if length == 0 {
        return Err(FrameError::Empty);
    }
    if length > MAX_CONTROL_FRAME {
        return Err(FrameError::TooLarge {
            actual: length,
            maximum: MAX_CONTROL_FRAME,
        });
    }

    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).await?;
    Ok((serde_json::from_slice(&payload)?, length))
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Example {
        value: String,
    }

    #[tokio::test]
    async fn frame_round_trip() {
        let expected = Example {
            value: "worker".to_string(),
        };
        let mut wire = Vec::new();
        write_json_frame(&mut wire, &expected).await.unwrap();

        let actual: Example = read_json_frame(&mut wire.as_slice()).await.unwrap();
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn counted_read_reports_the_payload_without_a_second_serialization() {
        let expected = Example {
            value: "worker".to_string(),
        };
        let mut wire = Vec::new();
        write_json_frame(&mut wire, &expected).await.unwrap();
        let payload_length = u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize;

        let (actual, counted): (Example, usize) =
            read_json_frame_counted(&mut wire.as_slice()).await.unwrap();
        assert_eq!(actual, expected);
        assert_eq!(counted, payload_length);
    }

    #[tokio::test]
    async fn rejects_oversized_length_before_allocating() {
        let mut wire = ((MAX_CONTROL_FRAME + 1) as u32).to_be_bytes().to_vec();
        wire.extend_from_slice(b"{}");

        let error = read_json_frame::<_, serde_json::Value>(&mut wire.as_slice())
            .await
            .unwrap_err();
        assert!(matches!(error, FrameError::TooLarge { .. }));
    }
}
