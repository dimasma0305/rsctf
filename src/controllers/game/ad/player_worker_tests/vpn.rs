use super::*;
use crate::{app_state::SharedState, utils::enums::Role};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tokio::io::AsyncWriteExt;
use tower::ServiceExt;

const PEER: &str = "rscpeer0";
pub(super) struct PlayerVpn;

async fn ip(args: &[&str]) -> Vec<u8> {
    let output = Command::new("ip").args(args).output().await.unwrap();
    assert!(
        output.status.success(),
        "ip {} failed: {}",
        args[0],
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

impl PlayerVpn {
    pub(super) async fn download(state: &SharedState, captain: Uuid, dir: &Path) -> Self {
        let token = state
            .token
            .issue(captain, Role::User, "fixture-one", "kind-test")
            .unwrap();
        let request = Request::get("/api/Game/900001/Ad/Vpn/Config")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let response = crate::controllers::game::ad::router()
            .with_state(state.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 16384)
            .await
            .unwrap();
        let config = std::str::from_utf8(&bytes).unwrap();
        let field = |name: &str| {
            config
                .lines()
                .find_map(|line| line.strip_prefix(name))
                .unwrap()
        };
        let address = field("Address = ");
        let routes = field("AllowedIPs = ");
        let redact = |stderr: &[u8]| {
            String::from_utf8_lossy(stderr)
                .replace(field("PrivateKey = "), "[redacted]")
                .replace(field("PublicKey = "), "[redacted]")
        };
        private_file(dir, "player.conf", &bytes);
        let stripped = Command::new("/usr/bin/wg-quick")
            .arg("strip")
            .arg(dir.join("player.conf"))
            .output()
            .await
            .unwrap();
        assert!(
            stripped.status.success(),
            "wg-quick strip failed ({}): {}",
            stripped.status,
            redact(&stripped.stderr)
        );
        ip(&["netns", "add", PEER]).await;
        let peer = Self;
        ip(&["link", "add", PEER, "type", "wireguard"]).await;
        let mut wg = Command::new("/usr/bin/wg")
            .args(["setconf", PEER, "/dev/stdin"])
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        wg.stdin
            .take()
            .unwrap()
            .write_all(&stripped.stdout)
            .await
            .unwrap();
        let configured = wg.wait_with_output().await.unwrap();
        assert!(
            configured.status.success(),
            "wg setconf failed ({}): {}",
            configured.status,
            redact(&configured.stderr)
        );
        ip(&["link", "set", PEER, "netns", PEER]).await;
        ip(&["-n", PEER, "addr", "add", address, "dev", PEER]).await;
        ip(&["-n", PEER, "link", "set", "lo", "up"]).await;
        ip(&["-n", PEER, "link", "set", PEER, "up"]).await;
        for route in routes.split(',').map(str::trim) {
            ip(&["-n", PEER, "route", "replace", route, "dev", PEER]).await;
        }
        peer
    }

    pub(super) async fn check_health(&self, pool: &PgPool) {
        let (host, port): (String, i32) =
            sqlx::query_as(r#"SELECT host,port FROM "AdTeamServices" WHERE id=900041"#)
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(host, "10.214.103.253");
        let body = ip(&[
            "netns",
            "exec",
            PEER,
            "curl",
            "--noproxy",
            "*",
            "-fsS",
            "--max-time",
            "5",
            &format!("http://{host}:{port}/health"),
        ])
        .await;
        assert_eq!(std::str::from_utf8(&body).unwrap().trim(), "ok");
    }
}
impl Drop for PlayerVpn {
    fn drop(&mut self) {
        let _ = std::process::Command::new("ip")
            .args(["netns", "del", PEER])
            .output();
    }
}
