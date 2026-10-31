//! Real TCP/WebSocket close handshakes, including a close before SignalR setup.
use axum::extract::ws::WebSocketUpgrade;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{admission, attack, signalr};

async fn write_frame(stream: &mut tokio::net::TcpStream, opcode: u8, payload: &[u8]) {
    assert!(payload.len() < 126);
    let mask = [1, 2, 3, 4];
    let mut bytes = vec![0x80 | opcode, 0x80 | payload.len() as u8];
    bytes.extend(mask);
    bytes.extend(
        payload
            .iter()
            .enumerate()
            .map(|(i, byte)| byte ^ mask[i % 4]),
    );
    stream.write_all(&bytes).await.unwrap();
}

async fn read_frame(stream: &mut tokio::net::TcpStream) -> (u8, Vec<u8>) {
    let mut header = [0; 2];
    stream
        .read_exact(&mut header)
        .await
        .expect("peer dropped TCP before completing the WebSocket close");
    assert_eq!(header[1] & 0x80, 0, "server frames must be unmasked");
    let length = header[1] & 0x7f;
    assert!(
        length < 126,
        "fixture expects bounded control/handshake frames"
    );
    let mut payload = vec![0; usize::from(length)];
    stream.read_exact(&mut payload).await.unwrap();
    (header[0] & 0xf, payload)
}

#[tokio::test]
async fn broadcast_hubs_complete_peer_close_before_and_after_handshake() {
    for mode in ["signalr-before", "signalr-after", "raw"] {
        let app = axum::Router::new().route(
            "/socket",
            axum::routing::get(move |ws: WebSocketUpgrade| async move {
                let bus = crate::services::event_bus::EventBus::local();
                let rx = bus.subscribe_game(1);
                let permit = admission::try_connection_permit(
                    format!("close-test-{mode}"),
                    admission::Scope::Game(1),
                )
                .unwrap();
                signalr::bounded_upgrade(ws).on_upgrade(move |socket| async move {
                    // Keep the publisher alive until the handler finishes.
                    let _bus = bus;
                    if mode == "raw" {
                        attack::serve_raw(socket, rx, 1, None, permit).await;
                    } else {
                        signalr::serve(socket, rx, &["ReceivedGameNotice"], Some(1), None, permit)
                            .await;
                    }
                })
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            stream.write_all(format!("GET /socket HTTP/1.1\r\nHost: {address}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n").as_bytes()).await.unwrap();
            let mut response = Vec::new();
            while !response.ends_with(b"\r\n\r\n") {
                response.push(stream.read_u8().await.unwrap());
                assert!(response.len() < 8192);
            }
            assert!(response.starts_with(b"HTTP/1.1 101"));
            if mode == "signalr-after" {
                write_frame(&mut stream, 1, b"{\"protocol\":\"json\",\"version\":1}\x1e").await;
                assert_eq!(read_frame(&mut stream).await, (1, b"{}\x1e".to_vec()));
            } else if mode == "raw" {
                let (opcode, hello) = read_frame(&mut stream).await;
                assert_eq!(opcode, 1);
                assert_eq!(serde_json::from_slice::<serde_json::Value>(&hello).unwrap()["kind"], "hello");
            }
            let close = b"\x03\xe8normal close";
            write_frame(&mut stream, 8, close).await;
            // A SignalR keepalive can race the client's close.
            loop {
                let (opcode, payload) = read_frame(&mut stream).await;
                if opcode == 8 {
                    assert_eq!(payload, close, "{mode}: close code/reason changed");
                    break;
                }
                assert_eq!(opcode, 1);
            }
        }).await;
        server.abort();
        result.expect("WebSocket close handshake timed out");
    }
}
