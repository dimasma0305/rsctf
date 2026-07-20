//! WebSocket-message framing for the yamux byte stream.

use axum::extract::ws::Message;
use bytes::Bytes;

/// Only binary WebSocket messages carry yamux bytes. Control-frame payloads
/// are WebSocket metadata, not application data, and must never be injected
/// into the multiplexed connection.
pub(super) fn message_payload(message: Message) -> std::io::Result<Option<Bytes>> {
    match message {
        Message::Binary(payload) => Ok(Some(payload)),
        Message::Text(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "BYOC yamux transport requires binary WebSocket messages",
        )),
        Message::Ping(_) | Message::Pong(_) | Message::Close(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_binary_messages_enter_the_yamux_stream() {
        assert_eq!(
            message_payload(Message::Binary(Bytes::from_static(b"yamux"))).unwrap(),
            Some(Bytes::from_static(b"yamux"))
        );
        assert_eq!(
            message_payload(Message::Ping(Bytes::from_static(b"not-yamux"))).unwrap(),
            None
        );
        assert_eq!(
            message_payload(Message::Pong(Bytes::from_static(b"not-yamux"))).unwrap(),
            None
        );
        assert_eq!(message_payload(Message::Close(None)).unwrap(), None);
        assert_eq!(
            message_payload(Message::Text("not-yamux".into()))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );
    }
}
