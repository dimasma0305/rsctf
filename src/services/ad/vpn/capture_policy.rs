//! Kernel-expiring admission gate for capture-required service routes.
//!
//! PostgreSQL proves which exact endpoint generations have a healthy capture
//! owner. A stable ipset mirrors that proof with per-entry timeouts, so routes
//! close even if every userspace task in the network owner stops refreshing it.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::net::Ipv4Addr;
use std::process::Stdio;
use std::time::Duration;

use sea_orm::DatabaseConnection;
use tokio::io::AsyncWriteExt;

use crate::services::capture_safety::KERNEL_LIVE_TIMEOUT_SECONDS;
use crate::utils::error::{AppError, AppResult};

pub(super) const REQUIRED_SET: &str = "rsv_capture_required";
pub(super) const LIVE_SET: &str = "rsv_capture_live";
const REQUIRED_STAGE_SET: &str = "rsv_capture_req_stage";
const LIVE_STAGE_SET: &str = "rsv_capture_live_stage";
pub(super) const REFRESH_INTERVAL: Duration = Duration::from_secs(3);
const APPLY_TIMEOUT: Duration = Duration::from_secs(5);
static APPLY_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
struct EndpointRow {
    host: String,
    port: i32,
    live: bool,
}

const LOAD_ENDPOINTS_SQL: &str = r#"SELECT BTRIM(service.host) AS host,
          service.port,
          EXISTS (
              SELECT 1
                FROM "TrafficCaptureLiveEndpoints" live
                JOIN "TrafficCaptureOwnerState" owner ON owner.id = 1
               WHERE live.service_id = service.id
                 AND live.container_id = BTRIM(service.container_id)
                 AND live.host = BTRIM(service.host)
                 AND live.port = service.port
                 AND live.owner_id = owner.owner_id
                 AND live.owner_epoch = owner.owner_epoch
                 AND owner.draining = FALSE
                 AND owner.lease_expires_at > clock_timestamp()
          ) AS live
     FROM "AdTeamServices" service
     JOIN "GameChallenges" challenge
       ON challenge.id = service.challenge_id
      AND challenge.game_id = service.game_id
     JOIN "Games" game ON game.id = service.game_id
     JOIN "Participations" participation
       ON participation.id = service.participation_id
      AND participation.game_id = service.game_id
    WHERE challenge.enable_traffic_capture = TRUE
      AND challenge.ad_self_hosted = FALSE
      AND challenge.is_enabled = TRUE
      AND challenge.review_status = 0
      AND challenge."Type" = 4
      AND participation.status = 1
      AND game.start_time_utc <= clock_timestamp()
      AND clock_timestamp() <= game.end_time_utc
      AND service.container_id IS NOT NULL
      AND NULLIF(BTRIM(service.container_id), '') IS NOT NULL
      AND NULLIF(BTRIM(service.host), '') IS NOT NULL
      AND service.port BETWEEN 1 AND 65535"#;

fn capture_set_script(rows: Vec<EndpointRow>) -> String {
    let mut endpoints = BTreeMap::<(Ipv4Addr, u16), bool>::new();
    for row in rows {
        let Ok(host) = row.host.trim().parse::<Ipv4Addr>() else {
            continue;
        };
        let Some(port) = u16::try_from(row.port).ok().filter(|port| *port > 0) else {
            continue;
        };
        endpoints
            .entry((host, port))
            .and_modify(|live| *live |= row.live)
            .or_insert(row.live);
    }

    let mut script = format!(
        "create {REQUIRED_SET} hash:ip,port family inet maxelem 131072\n\
         create {REQUIRED_STAGE_SET} hash:ip,port family inet maxelem 131072\n\
         create {LIVE_SET} hash:ip,port family inet timeout {KERNEL_LIVE_TIMEOUT_SECONDS} maxelem 131072\n\
         create {LIVE_STAGE_SET} hash:ip,port family inet timeout {KERNEL_LIVE_TIMEOUT_SECONDS} maxelem 131072\n\
         flush {REQUIRED_STAGE_SET}\nflush {LIVE_STAGE_SET}\n"
    );
    for ((host, port), live) in endpoints {
        writeln!(script, "add {REQUIRED_STAGE_SET} {host},tcp:{port}")
            .expect("writing to String cannot fail");
        if live {
            writeln!(
                script,
                "add {LIVE_STAGE_SET} {host},tcp:{port} timeout {KERNEL_LIVE_TIMEOUT_SECONDS}"
            )
            .expect("writing to String cannot fail");
        }
    }
    writeln!(script, "swap {LIVE_STAGE_SET} {LIVE_SET}").expect("writing to String cannot fail");
    writeln!(script, "swap {REQUIRED_STAGE_SET} {REQUIRED_SET}")
        .expect("writing to String cannot fail");
    writeln!(script, "flush {REQUIRED_STAGE_SET}").expect("writing to String cannot fail");
    writeln!(script, "flush {LIVE_STAGE_SET}").expect("writing to String cannot fail");
    script
}

fn empty_live_set_script() -> String {
    format!(
        "create {LIVE_SET} hash:ip,port family inet timeout {KERNEL_LIVE_TIMEOUT_SECONDS} maxelem 131072\n\
         create {LIVE_STAGE_SET} hash:ip,port family inet timeout {KERNEL_LIVE_TIMEOUT_SECONDS} maxelem 131072\n\
         flush {LIVE_STAGE_SET}\nswap {LIVE_STAGE_SET} {LIVE_SET}\nflush {LIVE_STAGE_SET}\n"
    )
}

async fn load_endpoints(db: &DatabaseConnection) -> AppResult<Vec<EndpointRow>> {
    tokio::time::timeout(
        APPLY_TIMEOUT,
        sqlx::query_as::<_, EndpointRow>(LOAD_ENDPOINTS_SQL)
            .fetch_all(db.get_postgres_connection_pool()),
    )
    .await
    .map_err(|_| AppError::internal("capture policy database load timed out"))?
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Atomically replace both the durable required set and the short-lived live
/// set. On any database or task failure the old live entries are not refreshed
/// and therefore expire in the kernel without another userspace decision.
pub(super) async fn refresh(db: &DatabaseConnection) -> AppResult<()> {
    let _guard = APPLY_LOCK.lock().await;
    let script = capture_set_script(load_endpoints(db).await?);
    apply_script(script).await
}

/// Immediately revoke every capture-required route in this network namespace.
/// This does not depend on PostgreSQL, so owner cleanup can fence the kernel
/// even when its durable drain update failed.
pub(super) async fn fence_live() -> AppResult<()> {
    let _guard = tokio::time::timeout(APPLY_TIMEOUT, APPLY_LOCK.lock())
        .await
        .map_err(|_| AppError::internal("capture policy fence lock timed out"))?;
    apply_script(empty_live_set_script()).await
}

async fn apply_script(script: String) -> AppResult<()> {
    tokio::time::timeout(APPLY_TIMEOUT, run_ipset_restore(&script))
        .await
        .map_err(|_| AppError::internal("capture policy apply timed out"))?
        .map_err(AppError::internal)
}

async fn run_ipset_restore(script: &str) -> Result<(), String> {
    let mut child = tokio::process::Command::new("ipset")
        .args(["restore", "-exist"])
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("execute ipset restore -exist: {error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "open ipset restore stdin".to_string())?;
    stdin
        .write_all(script.as_bytes())
        .await
        .map_err(|error| format!("write ipset restore commands: {error}"))?;
    drop(stdin);
    let output = child
        .wait_with_output()
        .await
        .map_err(|error| format!("wait for ipset restore: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(format!(
            "ipset restore -exist failed with {}{}",
            output.status,
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_live_members_are_timed_and_required_members_persist() {
        let script = capture_set_script(vec![
            EndpointRow {
                host: "10.13.40.8".into(),
                port: 8080,
                live: true,
            },
            EndpointRow {
                host: "10.13.40.9".into(),
                port: 8081,
                live: false,
            },
        ]);
        assert!(script.contains("add rsv_capture_req_stage 10.13.40.8,tcp:8080"));
        assert!(script.contains("add rsv_capture_live_stage 10.13.40.8,tcp:8080 timeout 15"));
        assert!(script.contains("add rsv_capture_req_stage 10.13.40.9,tcp:8081"));
        assert!(!script.contains("add rsv_capture_live_stage 10.13.40.9,tcp:8081"));
        assert!(script.contains("swap rsv_capture_live_stage rsv_capture_live"));
        assert!(
            script.find("swap rsv_capture_live_stage").unwrap()
                < script.find("swap rsv_capture_req_stage").unwrap(),
            "revocations must become live before required admission changes"
        );
    }

    #[test]
    fn kernel_timeout_outlives_three_refresh_attempts() {
        assert!(REFRESH_INTERVAL * 3 < Duration::from_secs(KERNEL_LIVE_TIMEOUT_SECONDS.into()));
    }

    #[test]
    fn emergency_fence_atomically_empties_the_live_set() {
        let script = empty_live_set_script();
        assert!(script.contains("flush rsv_capture_live_stage"));
        assert!(script.contains("swap rsv_capture_live_stage rsv_capture_live"));
        assert!(!script.contains("add rsv_capture_live_stage"));
    }

    #[test]
    fn live_query_fences_every_exact_identity_and_owner_dimension() {
        for predicate in [
            "live.service_id = service.id",
            "live.container_id = BTRIM(service.container_id)",
            "live.host = BTRIM(service.host)",
            "live.port = service.port",
            "live.owner_id = owner.owner_id",
            "live.owner_epoch = owner.owner_epoch",
            "owner.draining = FALSE",
            "owner.lease_expires_at > clock_timestamp()",
        ] {
            assert!(
                LOAD_ENDPOINTS_SQL.contains(predicate),
                "missing {predicate}"
            );
        }
    }
}
