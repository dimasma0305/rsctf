//! Managed credentials injected into a Leaderboard KotH target.
//!
//! The credential is generated before a crash-recoverable container create and
//! remains stable for retries of the same reset attempt. It is accepted only
//! while that exact lifecycle generation is the active published target.

use axum::http::Uri;

use crate::services::container::ContainerBackendKind;
use crate::utils::error::{AppError, AppResult};

const REPORTER_SECRET_BYTES: usize = 32;
const REPORTER_SECRET_PREFIX: &str = "koth_target_";

pub(crate) const GAME_ID_ENV: &str = "RSCTF_KOTH_GAME_ID";
pub(crate) const CHALLENGE_ID_ENV: &str = "RSCTF_KOTH_CHALLENGE_ID";
pub(crate) const PLATFORM_URL_ENV: &str = "RSCTF_KOTH_PLATFORM_URL";
pub(crate) const CONTEXT_URL_ENV: &str = "RSCTF_KOTH_CONTEXT_URL";
pub(crate) const OBSERVATION_URL_ENV: &str = "RSCTF_KOTH_OBSERVATION_URL";
pub(crate) const REPORTER_SECRET_ENV: &str = "RSCTF_KOTH_REPORTER_SECRET";

pub(crate) struct TargetReporterRuntime {
    pub(crate) env: Vec<(String, String)>,
    pub(crate) callback_ports: Vec<i32>,
    /// Non-secret fingerprint of the callback origin, ports, backend, and
    /// backend route identity. It fences crash-orphan adoption when routing
    /// changes.
    pub(crate) routing_revision: String,
}

pub(crate) struct TargetReporterRoute<'a> {
    pub(crate) base_url: Option<&'a str>,
    pub(crate) bind_addr: &'a str,
    pub(crate) backend_kind: ContainerBackendKind,
    pub(crate) backend_identity: Option<&'a str>,
}

fn callback_origin_port(base_url: &str) -> AppResult<i32> {
    let uri = base_url
        .parse::<Uri>()
        .map_err(|_| AppError::internal("invalid managed KotH reporter base URL"))?;
    let port = uri
        .authority()
        .and_then(|authority| authority.port_u16())
        .unwrap_or_else(|| {
            if uri.scheme_str() == Some("https") {
                443
            } else {
                80
            }
        });
    Ok(i32::from(port))
}

fn callback_ports(base_url: &str, bind_addr: &str) -> AppResult<Vec<i32>> {
    let origin_port = callback_origin_port(base_url)?;
    let target_port = bind_addr
        .parse::<std::net::SocketAddr>()
        .map(|address| i32::from(address.port()))
        .map_err(|_| AppError::internal("invalid rsctf bind address for managed KotH reporting"))?;
    let mut ports = vec![origin_port];
    if target_port != origin_port {
        ports.push(target_port);
    }
    Ok(ports)
}

pub(crate) fn routing_revision(
    base_url: &str,
    callback_ports: &[i32],
    backend_kind: ContainerBackendKind,
    backend_route_identity: Option<&str>,
) -> AppResult<String> {
    if backend_kind == ContainerBackendKind::Kubernetes && backend_route_identity.is_none() {
        return Err(AppError::internal(
            "managed KotH reporting on Kubernetes requires an exact callback routing identity",
        ));
    }
    let ports = callback_ports
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let backend = match backend_kind {
        ContainerBackendKind::None => "none",
        ContainerBackendKind::Docker => "docker",
        ContainerBackendKind::Kubernetes => "kubernetes",
        ContainerBackendKind::Worker => "worker",
    };
    Ok(crate::utils::codec::sha256_str(&format!(
        "{base_url}\0{ports}\0{backend}\0{}",
        backend_route_identity.unwrap_or_default()
    ))[..16]
        .to_string())
}

fn runtime(
    base_url: &str,
    bind_addr: &str,
    game_id: i32,
    challenge_id: i32,
    secret: String,
    backend_kind: ContainerBackendKind,
    backend_route_identity: Option<&str>,
) -> AppResult<TargetReporterRuntime> {
    let base_url = base_url.trim_end_matches('/');
    let callback_ports = callback_ports(base_url, bind_addr)?;
    let challenge_api = format!("{base_url}/api/v1/koth/games/{game_id}/challenges/{challenge_id}");
    Ok(TargetReporterRuntime {
        env: vec![
            (GAME_ID_ENV.to_string(), game_id.to_string()),
            (CHALLENGE_ID_ENV.to_string(), challenge_id.to_string()),
            (PLATFORM_URL_ENV.to_string(), base_url.to_string()),
            (
                CONTEXT_URL_ENV.to_string(),
                format!("{challenge_api}/context"),
            ),
            (
                OBSERVATION_URL_ENV.to_string(),
                format!("{challenge_api}/observations"),
            ),
            (REPORTER_SECRET_ENV.to_string(), secret),
        ],
        routing_revision: routing_revision(
            base_url,
            &callback_ports,
            backend_kind,
            backend_route_identity,
        )?,
        callback_ports,
    })
}

async fn revoke_for_cycle(
    pool: &sqlx::PgPool,
    cycle_id: i64,
    game_id: i32,
    challenge_id: i32,
    reset_attempt: i32,
) -> AppResult<()> {
    sqlx::query(
        r#"DELETE FROM "KothTargetReporters" reporter
             USING "KothCrownCycles" cycle
             WHERE reporter.cycle_id = cycle.id
               AND cycle.id = $1
               AND cycle.game_id = $2
               AND cycle.challenge_id = $3
               AND cycle.reset_attempt = $4
               AND cycle.phase = 'CreatePending'"#,
    )
    .bind(cycle_id)
    .bind(game_id)
    .bind(challenge_id)
    .bind(reset_attempt)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

/// Load or create the reporter secret for one lifecycle reset and routing
/// contract. A same-route retry keeps the first secret so an adopted container
/// and the database agree; changing the route rotates it before replacement
/// creation, immediately revoking any crash-orphaned target.
pub(crate) async fn ensure_for_cycle(
    pool: &sqlx::PgPool,
    route: TargetReporterRoute<'_>,
    cycle_id: i64,
    game_id: i32,
    challenge_id: i32,
    reset_attempt: i32,
) -> AppResult<Option<TargetReporterRuntime>> {
    let TargetReporterRoute {
        base_url,
        bind_addr,
        backend_kind,
        backend_identity,
    } = route;
    let Some(base_url) = base_url else {
        revoke_for_cycle(pool, cycle_id, game_id, challenge_id, reset_attempt).await?;
        return Ok(None);
    };
    let configured: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
             SELECT 1 FROM "KothApiObservers"
              WHERE game_id = $1 AND challenge_id = $2
           )"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !configured {
        revoke_for_cycle(pool, cycle_id, game_id, challenge_id, reset_attempt).await?;
        return Ok(None);
    }

    let candidate = format!(
        "{REPORTER_SECRET_PREFIX}{}",
        crate::utils::codec::random_token(REPORTER_SECRET_BYTES)
    );
    let normalized_base_url = base_url.trim_end_matches('/');
    let effective_ports = callback_ports(normalized_base_url, bind_addr)?;
    let current_routing_revision = routing_revision(
        normalized_base_url,
        &effective_ports,
        backend_kind,
        backend_identity,
    )?;
    let secret: Option<String> = sqlx::query_scalar(
        r#"INSERT INTO "KothTargetReporters"
             (cycle_id, game_id, challenge_id, reset_attempt,
              routing_revision, hmac_secret, issued_at, expires_at, last_used_at)
           SELECT cycle.id, cycle.game_id, cycle.challenge_id,
                  cycle.reset_attempt, $6, $5, clock_timestamp(),
                  game.end_time_utc, NULL
             FROM "KothCrownCycles" cycle
             JOIN "Games" game ON game.id = cycle.game_id
             JOIN "KothApiObservers" observer
               ON observer.game_id = cycle.game_id
              AND observer.challenge_id = cycle.challenge_id
            WHERE cycle.id = $1
              AND cycle.game_id = $2
              AND cycle.challenge_id = $3
              AND cycle.reset_attempt = $4
              AND cycle.phase = 'CreatePending'
              AND clock_timestamp() < game.end_time_utc
           ON CONFLICT (cycle_id) DO UPDATE SET
             reset_attempt = EXCLUDED.reset_attempt,
             routing_revision = EXCLUDED.routing_revision,
             hmac_secret = CASE
               WHEN "KothTargetReporters".reset_attempt = EXCLUDED.reset_attempt
                AND "KothTargetReporters".routing_revision = EXCLUDED.routing_revision
                 THEN "KothTargetReporters".hmac_secret
               ELSE EXCLUDED.hmac_secret
             END,
             issued_at = CASE
               WHEN "KothTargetReporters".reset_attempt = EXCLUDED.reset_attempt
                AND "KothTargetReporters".routing_revision = EXCLUDED.routing_revision
                 THEN "KothTargetReporters".issued_at
               ELSE clock_timestamp()
             END,
             expires_at = EXCLUDED.expires_at,
             last_used_at = CASE
               WHEN "KothTargetReporters".reset_attempt = EXCLUDED.reset_attempt
                AND "KothTargetReporters".routing_revision = EXCLUDED.routing_revision
                 THEN "KothTargetReporters".last_used_at
               ELSE NULL
             END
           WHERE "KothTargetReporters".reset_attempt <= EXCLUDED.reset_attempt
        RETURNING hmac_secret"#,
    )
    .bind(cycle_id)
    .bind(game_id)
    .bind(challenge_id)
    .bind(reset_attempt)
    .bind(candidate)
    .bind(&current_routing_revision)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let secret = secret.ok_or_else(|| {
        AppError::conflict("KotH reporter lifecycle changed during target creation")
    })?;
    runtime(
        base_url,
        bind_addr,
        game_id,
        challenge_id,
        secret,
        backend_kind,
        backend_identity,
    )
    .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    fn reporter_secret(runtime: &TargetReporterRuntime) -> &str {
        runtime
            .env
            .iter()
            .find_map(|(key, value)| (key == REPORTER_SECRET_ENV).then_some(value.as_str()))
            .unwrap()
    }

    fn route<'a>(
        base_url: Option<&'a str>,
        backend_kind: ContainerBackendKind,
        backend_identity: Option<&'a str>,
    ) -> TargetReporterRoute<'a> {
        TargetReporterRoute {
            base_url,
            bind_addr: "0.0.0.0:8080",
            backend_kind,
            backend_identity,
        }
    }

    #[test]
    fn reporter_runtime_contains_exact_scoped_endpoints_and_default_ports() {
        let http = runtime(
            "http://rsctf-control",
            "0.0.0.0:8080",
            7,
            9,
            "secret".to_string(),
            ContainerBackendKind::Docker,
            None,
        )
        .unwrap();
        assert_eq!(http.callback_ports, vec![80, 8080]);
        assert_eq!(http.routing_revision.len(), 16);
        assert!(http
            .routing_revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit()));
        assert!(http.env.contains(&(
            CONTEXT_URL_ENV.to_string(),
            "http://rsctf-control/api/v1/koth/games/7/challenges/9/context".to_string()
        )));
        assert!(http.env.contains(&(
            OBSERVATION_URL_ENV.to_string(),
            "http://rsctf-control/api/v1/koth/games/7/challenges/9/observations".to_string()
        )));

        let https = runtime(
            "https://rsctf-control/",
            "0.0.0.0:8080",
            7,
            9,
            "secret".to_string(),
            ContainerBackendKind::Docker,
            None,
        )
        .unwrap();
        assert_eq!(https.callback_ports, vec![443, 8080]);
        let custom = runtime(
            "http://rsctf-koth-reporter:8080",
            "0.0.0.0:8080",
            7,
            9,
            "secret".to_string(),
            ContainerBackendKind::Docker,
            None,
        )
        .unwrap();
        assert_eq!(custom.callback_ports, vec![8080]);
        assert_ne!(http.routing_revision, https.routing_revision);
        assert_ne!(http.routing_revision, custom.routing_revision);

        let route_a = runtime(
            "http://rsctf-control",
            "0.0.0.0:8080",
            7,
            9,
            "secret".to_string(),
            ContainerBackendKind::Kubernetes,
            Some("namespace=rsctf-system\0selector=app=network"),
        )
        .unwrap();
        let route_b = runtime(
            "http://rsctf-control",
            "0.0.0.0:8080",
            7,
            9,
            "secret".to_string(),
            ContainerBackendKind::Kubernetes,
            Some("namespace=rsctf-control\0selector=app=network"),
        )
        .unwrap();
        assert_ne!(route_a.routing_revision, route_b.routing_revision);
        assert!(runtime(
            "http://rsctf-control",
            "0.0.0.0:8080",
            7,
            9,
            "secret".to_string(),
            ContainerBackendKind::Kubernetes,
            None,
        )
        .is_err());
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn retry_keeps_one_secret_while_routing_and_reset_changes_rotate_it() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "Games" (
              id INTEGER PRIMARY KEY, end_time_utc TIMESTAMPTZ
            );
            CREATE TEMP TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY, game_id INTEGER
            );
            CREATE TEMP TABLE "KothApiObservers" (
              challenge_id INTEGER PRIMARY KEY, game_id INTEGER
            );
            CREATE TEMP TABLE "KothCrownCycles" (
              id BIGINT PRIMARY KEY, game_id INTEGER, challenge_id INTEGER,
              reset_attempt INTEGER, phase TEXT
            );
            CREATE TEMP TABLE "KothTargetReporters" (
              cycle_id BIGINT PRIMARY KEY, game_id INTEGER,
              challenge_id INTEGER, reset_attempt INTEGER,
              routing_revision VARCHAR(16),
              hmac_secret TEXT, issued_at TIMESTAMPTZ,
              expires_at TIMESTAMPTZ, last_used_at TIMESTAMPTZ
            );
            INSERT INTO "Games" VALUES
              (7, clock_timestamp() + interval '1 hour');
            INSERT INTO "GameChallenges" VALUES (9, 7);
            INSERT INTO "KothApiObservers" VALUES (9, 7);
            INSERT INTO "KothCrownCycles" VALUES
              (41, 7, 9, 1, 'CreatePending');
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let first = ensure_for_cycle(
            &pool,
            route(
                Some("http://rsctf-koth-reporter:8080"),
                ContainerBackendKind::Docker,
                None,
            ),
            41,
            7,
            9,
            1,
        )
        .await
        .unwrap()
        .unwrap();
        let retry = ensure_for_cycle(
            &pool,
            route(
                Some("http://rsctf-koth-reporter:8080"),
                ContainerBackendKind::Docker,
                None,
            ),
            41,
            7,
            9,
            1,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(reporter_secret(&first), reporter_secret(&retry));

        let rerouted = ensure_for_cycle(
            &pool,
            route(
                Some("https://rsctf-koth-reporter"),
                ContainerBackendKind::Docker,
                None,
            ),
            41,
            7,
            9,
            1,
        )
        .await
        .unwrap()
        .unwrap();
        assert_ne!(reporter_secret(&first), reporter_secret(&rerouted));
        let rerouted_retry = ensure_for_cycle(
            &pool,
            route(
                Some("https://rsctf-koth-reporter"),
                ContainerBackendKind::Docker,
                None,
            ),
            41,
            7,
            9,
            1,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(reporter_secret(&rerouted), reporter_secret(&rerouted_retry));

        let restored_route = ensure_for_cycle(
            &pool,
            route(
                Some("http://rsctf-koth-reporter:8080"),
                ContainerBackendKind::Docker,
                None,
            ),
            41,
            7,
            9,
            1,
        )
        .await
        .unwrap()
        .unwrap();
        assert_ne!(reporter_secret(&rerouted), reporter_secret(&restored_route));
        assert_ne!(
            reporter_secret(&first),
            reporter_secret(&restored_route),
            "returning to an old route must not reactivate its orphan credential"
        );

        let route_a =
            "namespace=rsctf-system\0selector=app.kubernetes.io/component=network,app.kubernetes.io/instance=rsctf-network,app.kubernetes.io/name=rsctf";
        let kubernetes = ensure_for_cycle(
            &pool,
            route(
                Some("http://rsctf-koth-reporter:8080"),
                ContainerBackendKind::Kubernetes,
                Some(route_a),
            ),
            41,
            7,
            9,
            1,
        )
        .await
        .unwrap()
        .unwrap();
        assert_ne!(
            reporter_secret(&restored_route),
            reporter_secret(&kubernetes),
            "changing the container backend must rotate the credential"
        );

        let route_b =
            "namespace=rsctf-control\0selector=app.kubernetes.io/component=network,app.kubernetes.io/instance=rsctf-network,app.kubernetes.io/name=rsctf";
        let rerouted_kubernetes = ensure_for_cycle(
            &pool,
            route(
                Some("http://rsctf-koth-reporter:8080"),
                ContainerBackendKind::Kubernetes,
                Some(route_b),
            ),
            41,
            7,
            9,
            1,
        )
        .await
        .unwrap()
        .unwrap();
        assert_ne!(
            reporter_secret(&kubernetes),
            reporter_secret(&rerouted_kubernetes),
            "changing the callback namespace must rotate the credential"
        );

        let disabled = ensure_for_cycle(
            &pool,
            route(None, ContainerBackendKind::Kubernetes, None),
            41,
            7,
            9,
            1,
        )
        .await
        .unwrap();
        assert!(disabled.is_none());
        let remaining_reporters: i64 =
            sqlx::query_scalar(r#"SELECT count(*) FROM "KothTargetReporters" WHERE cycle_id = 41"#)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            remaining_reporters, 0,
            "disabling managed reporting must revoke an orphan credential before replacement"
        );

        let reenabled = ensure_for_cycle(
            &pool,
            route(
                Some("http://rsctf-koth-reporter:8080"),
                ContainerBackendKind::Kubernetes,
                Some(route_b),
            ),
            41,
            7,
            9,
            1,
        )
        .await
        .unwrap()
        .unwrap();
        assert_ne!(
            reporter_secret(&rerouted_kubernetes),
            reporter_secret(&reenabled),
            "re-enabling reporting must not revive the deleted credential"
        );

        sqlx::query(r#"DELETE FROM "KothApiObservers" WHERE challenge_id = 9"#)
            .execute(&pool)
            .await
            .unwrap();
        let no_longer_configured = ensure_for_cycle(
            &pool,
            route(
                Some("http://rsctf-koth-reporter:8080"),
                ContainerBackendKind::Kubernetes,
                Some(route_b),
            ),
            41,
            7,
            9,
            1,
        )
        .await
        .unwrap();
        assert!(no_longer_configured.is_none());
        let remaining_reporters: i64 =
            sqlx::query_scalar(r#"SELECT count(*) FROM "KothTargetReporters" WHERE cycle_id = 41"#)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            remaining_reporters, 0,
            "removing API-hill configuration must revoke an orphan credential"
        );

        sqlx::query(r#"INSERT INTO "KothApiObservers" VALUES (9, 7)"#)
            .execute(&pool)
            .await
            .unwrap();
        let reconfigured = ensure_for_cycle(
            &pool,
            route(
                Some("http://rsctf-koth-reporter:8080"),
                ContainerBackendKind::Kubernetes,
                Some(route_b),
            ),
            41,
            7,
            9,
            1,
        )
        .await
        .unwrap()
        .unwrap();
        assert_ne!(
            reporter_secret(&reenabled),
            reporter_secret(&reconfigured),
            "restoring API-hill configuration must issue a new credential"
        );

        sqlx::query(r#"UPDATE "KothCrownCycles" SET reset_attempt = 2 WHERE id = 41"#)
            .execute(&pool)
            .await
            .unwrap();
        let replacement = ensure_for_cycle(
            &pool,
            route(
                Some("http://rsctf-koth-reporter:8080"),
                ContainerBackendKind::Kubernetes,
                Some(route_b),
            ),
            41,
            7,
            9,
            2,
        )
        .await
        .unwrap()
        .unwrap();
        assert_ne!(
            reporter_secret(&reconfigured),
            reporter_secret(&replacement)
        );
        let (stored_attempt, stored_routing_revision): (i32, String) = sqlx::query_as(
            r#"SELECT reset_attempt, routing_revision
                 FROM "KothTargetReporters" WHERE cycle_id = 41"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored_attempt, 2);
        assert_eq!(stored_routing_revision, replacement.routing_revision);
    }
}
