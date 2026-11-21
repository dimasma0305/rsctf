use super::*;

#[tokio::test]
async fn source_report_and_flag_ack_share_the_existing_v2_tunnel() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let url = format!("ws://{address}");
    let headers = tunnel_request(&url).unwrap();
    assert_eq!(headers.headers().get("x-rsctf-byoc-source").unwrap(), "v1");
    let payload =
        Bytes::from_static(br#"{"sampledAt":42,"status":"observed","files":{"app.rs":null}}"#);
    let (_sender, reports) =
        tokio::sync::watch::channel(crate::source::CachedReport::new(payload.clone()));
    let sink = Arc::new(FlagSink::default());
    let generation = sink.begin_connection().await.unwrap();
    let dir = std::env::temp_dir().join(format!("rsctf-source-wire-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let flag = dir.join("flag");
    let client = connect_once(
        &url,
        "127.0.0.1:1",
        flag.to_str().unwrap(),
        "",
        sink,
        generation,
        &reports,
    );
    let relay = async {
        let (socket, _) = listener.accept().await.unwrap();
        let ws = tokio_tungstenite::accept_async(MaybeTlsStream::Plain(socket))
            .await
            .unwrap();
        let mut connection = Connection::new(WsByteStream::new(ws), yamux_config(), Mode::Server);
        let mut source = future::poll_fn(|cx| connection.poll_new_outbound(cx))
            .await
            .unwrap();
        let mut delivery = future::poll_fn(|cx| connection.poll_new_outbound(cx))
            .await
            .unwrap();
        let traffic = async {
            source.write_all(b"P").await.unwrap();
            source.flush().await.unwrap();
            let mut reply = Vec::new();
            source.read_to_end(&mut reply).await.unwrap();
            assert_eq!(&reply[..], &payload[..]);
            delivery.write_all(b"F").await.unwrap();
            delivery.write_all(&1u64.to_be_bytes()).await.unwrap();
            delivery.write_all(b"local-test-flag").await.unwrap();
            delivery.close().await.unwrap();
            let mut ack = [0; 9];
            delivery.read_exact(&mut ack).await.unwrap();
            assert_eq!(ack[0], b'A');
            assert_eq!(&ack[1..], &1u64.to_be_bytes());
        };
        tokio::select! {
            _=traffic=>{},
            _=future::poll_fn(|cx| connection.poll_next_inbound(cx))=>panic!("unexpected incoming stream or disconnect"),
        }
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            _=relay=>{},
            _=client=>panic!("agent disconnected before the source and flag responses"),
        }
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read(&flag).unwrap(), b"local-test-flag");
    std::fs::remove_dir_all(dir).unwrap();
}
