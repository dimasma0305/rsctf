//! Reliable rotating-flag delivery over a BYOC yamux control stream.

use std::time::Duration;

use bytes::Bytes;
use futures::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::{TunnelHandle, STREAM_FLAG};

/// The agent applies the same bound while reading a flag stream. Keeping one
/// retained value per live/authorized relay endpoint therefore has a hard and
/// small memory ceiling.
pub(super) const MAX_FLAG_BYTES: usize = 4096;
const FLAG_ACK: u8 = b'A';
const FLAG_ACK_BYTES: usize = 9;
/// Absolute fail-closed ceiling. Round publication applies its configured
/// (normally shorter) attempt timeout around `push_flag`.
const FLAG_ACK_TIMEOUT: Duration = Duration::from_secs(10);
const RECONNECT_REPLAY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RetainedFlag {
    sequence: u64,
    value: Bytes,
}

impl RetainedFlag {
    pub(super) fn sequence(&self) -> u64 {
        self.sequence
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum FlagRetention {
    Accepted(RetainedFlag),
    Stale(RetainedFlag),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FlagRetentionError {
    InvalidSequence,
    InvalidValue,
    SequenceConflict,
    Retired,
}

#[derive(Default)]
pub(super) struct FlagState {
    current: Option<RetainedFlag>,
}

impl FlagState {
    /// Retain the newest durable round flag for reconnect replay. A stale
    /// hydration read returns the already-newer value without replacing it;
    /// equal sequences are idempotent only for the exact same bytes.
    pub(super) fn retain(
        &mut self,
        sequence: u64,
        value: &str,
    ) -> Result<FlagRetention, FlagRetentionError> {
        let value = value.as_bytes();
        if value.is_empty() || value.len() > MAX_FLAG_BYTES {
            return Err(FlagRetentionError::InvalidValue);
        }
        if sequence == 0 {
            return Err(FlagRetentionError::InvalidSequence);
        }
        if let Some(current) = &self.current {
            if sequence < current.sequence {
                return Ok(FlagRetention::Stale(current.clone()));
            }
            if sequence == current.sequence {
                return if current.value.as_ref() == value {
                    Ok(FlagRetention::Accepted(current.clone()))
                } else {
                    Err(FlagRetentionError::SequenceConflict)
                };
            }
        }
        let retained = RetainedFlag {
            sequence,
            value: Bytes::copy_from_slice(value),
        };
        self.current = Some(retained.clone());
        Ok(FlagRetention::Accepted(retained))
    }

    pub(super) fn current(&self) -> Option<RetainedFlag> {
        self.current.clone()
    }

    pub(super) fn clear(&mut self) {
        self.current = None;
    }
}

/// Deliver one retained flag and require the exact sequence-bound install ACK.
/// Merely writing the stream is not success: the official agent sends this ACK
/// only after its temporary-file rename has completed.
pub(super) async fn deliver(handle: &TunnelHandle, flag: &RetainedFlag) -> bool {
    deliver_with_timeout(handle, flag, FLAG_ACK_TIMEOUT).await
}

pub(super) async fn replay(handle: &TunnelHandle, flag: &RetainedFlag) -> bool {
    deliver_with_timeout(handle, flag, RECONNECT_REPLAY_TIMEOUT).await
}

async fn deliver_with_timeout(
    handle: &TunnelHandle,
    flag: &RetainedFlag,
    timeout: Duration,
) -> bool {
    tokio::time::timeout(timeout, async {
        let Some(stream) = handle.open_stream().await else {
            return false;
        };
        deliver_over_stream(stream, flag, timeout).await
    })
    .await
    .unwrap_or(false)
}

async fn deliver_over_stream<S>(mut stream: S, flag: &RetainedFlag, ack_timeout: Duration) -> bool
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut header = [0_u8; 9];
    header[0] = STREAM_FLAG;
    header[1..].copy_from_slice(&flag.sequence.to_be_bytes());
    if stream.write_all(&header).await.is_err()
        || stream.write_all(&flag.value).await.is_err()
        || stream.close().await.is_err()
    {
        return false;
    }

    let mut ack = [0_u8; FLAG_ACK_BYTES];
    matches!(
        tokio::time::timeout(ack_timeout, stream.read_exact(&mut ack)).await,
        Ok(Ok(_))
    ) && ack[0] == FLAG_ACK
        && ack[1..] == flag.sequence.to_be_bytes()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use tokio::sync::{mpsc, watch};
    use tokio_util::compat::TokioAsyncReadCompatExt;

    fn retained(sequence: u64, value: &'static [u8]) -> RetainedFlag {
        RetainedFlag {
            sequence,
            value: Bytes::from_static(value),
        }
    }

    #[test]
    fn durable_retention_is_monotonic_bounded_and_idempotent() {
        let mut state = FlagState::default();
        assert_eq!(state.retain(1, ""), Err(FlagRetentionError::InvalidValue));
        assert_eq!(
            state.retain(1, &"x".repeat(MAX_FLAG_BYTES + 1)),
            Err(FlagRetentionError::InvalidValue)
        );
        assert_eq!(
            state.retain(0, "flag"),
            Err(FlagRetentionError::InvalidSequence)
        );

        let FlagRetention::Accepted(first) = state.retain(10, "flag-one").unwrap() else {
            panic!("first durable flag was not accepted")
        };
        assert_eq!(
            state.retain(10, "flag-one"),
            Ok(FlagRetention::Accepted(first.clone()))
        );
        assert_eq!(
            state.retain(10, "forged"),
            Err(FlagRetentionError::SequenceConflict)
        );
        assert_eq!(
            state.retain(9, "stale"),
            Ok(FlagRetention::Stale(first.clone()))
        );
        let FlagRetention::Accepted(second) = state.retain(42, "flag-two").unwrap() else {
            panic!("newer durable flag was not accepted")
        };
        assert_eq!(second.sequence, 42);
        assert_eq!(state.current(), Some(second));
    }

    #[tokio::test]
    async fn delivery_succeeds_only_for_the_matching_install_ack() {
        let flag = retained(23, b"flag-value");
        let (server, agent) = tokio::io::duplex(128);
        let peer = tokio::spawn(async move {
            let mut agent = agent.compat();
            let mut request = Vec::new();
            agent.read_to_end(&mut request).await.unwrap();
            assert_eq!(request[0], STREAM_FLAG);
            assert_eq!(u64::from_be_bytes(request[1..9].try_into().unwrap()), 23);
            assert_eq!(&request[9..], b"flag-value");
            let mut ack = [0_u8; FLAG_ACK_BYTES];
            ack[0] = FLAG_ACK;
            ack[1..].copy_from_slice(&23_u64.to_be_bytes());
            agent.write_all(&ack).await.unwrap();
            agent.close().await.unwrap();
        });
        assert!(deliver_over_stream(server.compat(), &flag, Duration::from_millis(100)).await);
        peer.await.unwrap();

        let (server, agent) = tokio::io::duplex(128);
        let peer = tokio::spawn(async move {
            let mut agent = agent.compat();
            let mut request = Vec::new();
            agent.read_to_end(&mut request).await.unwrap();
            let mut ack = [0_u8; FLAG_ACK_BYTES];
            ack[0] = FLAG_ACK;
            ack[1..].copy_from_slice(&22_u64.to_be_bytes());
            agent.write_all(&ack).await.unwrap();
            agent.close().await.unwrap();
        });
        assert!(!deliver_over_stream(server.compat(), &flag, Duration::from_millis(100)).await);
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn delivery_without_an_ack_is_not_reported_as_success() {
        let flag = retained(7, b"flag-value");
        let (server, agent) = tokio::io::duplex(128);
        let peer = tokio::spawn(async move {
            let mut agent = agent.compat();
            let mut request = Vec::new();
            agent.read_to_end(&mut request).await.unwrap();
            agent.close().await.unwrap();
        });
        assert!(!deliver_over_stream(server.compat(), &flag, Duration::from_millis(100)).await);
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn total_delivery_timeout_also_bounds_a_stalled_stream_open() {
        let (open, mut requests) = mpsc::channel(1);
        let (_closed_tx, closed) = watch::channel(false);
        let handle = TunnelHandle {
            id: 1,
            open,
            shutdown: Arc::new(tokio::sync::Notify::new()),
            closed,
            active: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };
        let stalled = tokio::spawn(async move {
            let _request = requests.recv().await.unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
        });
        let flag = retained(8, b"flag-value");
        assert!(!deliver_with_timeout(&handle, &flag, Duration::from_millis(20)).await);
        stalled.abort();
    }
}
