//! Cut only this fixture's TCP connections to real Redis. Never stop a shared
//! Redis service or flush its database while unrelated tests may be running.
use super::*;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    task::{JoinHandle, JoinSet},
};

struct Proxy {
    url: String,
    control: mpsc::Sender<(bool, oneshot::Sender<()>)>,
    task: JoinHandle<()>,
}

impl Proxy {
    async fn new(upstream: &str) -> Self {
        let mut url = url::Url::parse(upstream).unwrap();
        assert_eq!(url.scheme(), "redis");
        assert_eq!(
            url.host_str(),
            Some("127.0.0.1"),
            "loopback fixture required"
        );
        let target = std::net::SocketAddr::from(([127, 0, 0, 1], url.port().unwrap_or(6379)));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        url.set_port(Some(listener.local_addr().unwrap().port()))
            .unwrap();
        let (control, mut commands) = mpsc::channel::<(bool, oneshot::Sender<()>)>(1);
        let task = tokio::spawn(async move {
            let mut online = true;
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    command = commands.recv() => {
                        let Some((enabled, acknowledged)) = command else { break };
                        online = enabled;
                        if !online {
                            connections.abort_all();
                            while connections.join_next().await.is_some() {}
                        }
                        let _ = acknowledged.send(());
                    }
                    accepted = listener.accept() => {
                        let (mut client, _) = accepted.unwrap();
                        if online && connections.len() < 64 {
                            connections.spawn(async move {
                                if let Ok(Ok(mut upstream)) = tokio::time::timeout(
                                    Duration::from_millis(500), TcpStream::connect(target),
                                ).await {
                                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                                }
                            });
                        }
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        Self {
            url: url.to_string(),
            control,
            task,
        }
    }

    async fn online(&self, enabled: bool) {
        let (send, received) = oneshot::channel();
        self.control.send((enabled, send)).await.unwrap();
        received.await.unwrap();
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable loopback Redis via RSCTF_TEST_REDIS_URL"]
async fn real_redis_interruption_bounds_staleness_and_fails_closed_for_credentials_and_fences() {
    let url = std::env::var("RSCTF_TEST_REDIS_URL").expect("disposable Redis required");
    let proxy = Proxy::new(&url).await;
    let direct = RedisCache::connect(&url).await.unwrap();
    let first = Arc::new(TieredCache::new(
        Arc::new(RedisCache::connect(&proxy.url).await.unwrap()),
        Duration::from_secs(1),
    ));
    let second = Arc::new(TieredCache::new(
        Arc::new(RedisCache::connect(&proxy.url).await.unwrap()),
        Duration::from_secs(1),
    ));
    let prefix = format!("cache-interruption:{}", uuid::Uuid::new_v4());
    let response = format!("{prefix}:response");
    let credential = format!("{prefix}:credential");
    let fence = format!("{prefix}:fence");
    let secret = format!("{prefix}:secret");
    let offline_write = format!("{prefix}:offline-write");
    let ttl = Some(Duration::from_secs(60));

    tokio::time::timeout(Duration::from_secs(25), async {
        first.set(&response, b"old", ttl).await;
        first.set(&credential, b"single-use", ttl).await;
        first.set(&fence, b"v1", ttl).await;
        first.set_local(&secret, b"local-only", ttl).await;
        assert!(direct.get(&secret).await.is_none());
        assert!(second.get_local(&secret).await.is_none());
        // Cross-replica invalidation is bounded by the response L1 TTL. Fence
        // reads must observe the authoritative replacement immediately.
        second.set(&response, b"new", ttl).await;
        second.set_authoritative(&fence, b"v2", ttl).await;
        assert_eq!(first.get(&response).await.unwrap().as_ref(), b"old");
        assert_eq!(
            first.get_authoritative(&fence).await.unwrap().as_ref(),
            b"v2"
        );
        proxy.online(false).await;
        let cut_at = Instant::now();
        assert_eq!(first.get(&response).await.unwrap().as_ref(), b"old");
        assert_eq!(
            first.get_local(&secret).await.unwrap().as_ref(),
            b"local-only"
        );
        assert_eq!(
            first.backend_health().await,
            CacheBackendHealth::Unavailable
        );
        assert!(first.get_authoritative(&fence).await.is_none());
        assert!(first.get_and_remove(&credential).await.is_none());
        assert!(first.l1.get(&credential).await.is_none());
        assert!(
            !first
                .compare_and_set_authoritative(&fence, b"v2", b"lost", ttl)
                .await
        );
        assert!(
            !first
                .set_if_absent_authoritative(&offline_write, b"lost", ttl)
                .await
        );
        assert!(
            !first
                .set_authoritative_checked(&offline_write, b"lost", ttl)
                .await
        );
        assert!(first.l1.get(&fence).await.is_none());
        // The underlying authoritative value still exists; offline consume
        // must never return a warmed L1 credential or silently use a local CAS.
        assert_eq!(
            direct.get(&credential).await.unwrap().as_ref(),
            b"single-use"
        );
        assert_eq!(direct.get(&fence).await.unwrap().as_ref(), b"v2");
        let remaining = Duration::from_millis(1100).saturating_sub(cut_at.elapsed());
        tokio::time::sleep(remaining).await;
        assert!(
            first.get(&response).await.is_none(),
            "expired L1 cannot mask an outage"
        );

        proxy.online(true).await;
        tokio::time::timeout(Duration::from_secs(10), async {
            while first.backend_health().await != CacheBackendHealth::Ready
                || second.backend_health().await != CacheBackendHealth::Ready
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("Redis reconnect must be bounded");
        assert_eq!(first.get(&response).await.unwrap().as_ref(), b"new");
        assert_eq!(
            first.get_authoritative(&fence).await.unwrap().as_ref(),
            b"v2"
        );
        assert!(direct.get(&offline_write).await.is_none());
        let mut consumers = JoinSet::new();
        for index in 0..32 {
            let cache = if index % 2 == 0 {
                first.clone()
            } else {
                second.clone()
            };
            let key = credential.clone();
            consumers.spawn(async move { cache.get_and_remove(&key).await });
        }
        let mut consumed = 0;
        while let Some(value) = consumers.join_next().await {
            if let Some(value) = value.unwrap() {
                assert_eq!(value.as_ref(), b"single-use");
                consumed += 1;
            }
        }
        assert_eq!(consumed, 1);
        assert!(
            !first
                .compare_and_set_authoritative(&fence, b"v1", b"wrong", ttl)
                .await
        );
        assert!(
            second
                .compare_and_set_authoritative(&fence, b"v2", b"v3", ttl)
                .await
        );
        assert_eq!(
            first.get_authoritative(&fence).await.unwrap().as_ref(),
            b"v3"
        );
        second.remove(&response).await;
        assert_eq!(first.get(&response).await.unwrap().as_ref(), b"new");
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(first.get(&response).await.is_none());
    })
    .await
    .expect("interruption regression must finish within its deadline");
    for key in [&response, &credential, &fence, &secret, &offline_write] {
        direct.remove(key).await;
    }
}
