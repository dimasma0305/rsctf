use axum::extract::ws::{close_code, CloseFrame, Message, WebSocket};
/// Accept the upgraded socket and close it cleanly when there is no valid proxy
/// target. Sending the frame before dropping the socket keeps rejection graceful.
pub(super) async fn close_cleanly(mut socket: WebSocket) {
    let _ = socket.send(normal_close()).await;
}

pub(super) fn normal_close() -> Message {
    close_message(close_code::NORMAL, "")
}

pub(super) fn endpoint_unavailable_close() -> Message {
    close_message(close_code::AGAIN, "proxy endpoint unavailable")
}

pub(super) fn transport_failure_close() -> Message {
    close_message(close_code::ERROR, "proxy transport failed")
}

fn close_message(code: u16, reason: &'static str) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: reason.into(),
    }))
}
