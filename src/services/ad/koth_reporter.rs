//! Managed credentials injected into a Leaderboard KotH target.
//!
//! The credential is generated before a crash-recoverable container create and
//! remains stable for retries of the same reset attempt. It is accepted only
//! while that exact lifecycle generation is the active published target.

use axum::http::Uri;

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
    /// Non-secret fingerprint of the injected origin and callback ports. It
    /// fences Kubernetes crash-orphan adoption when routing changes.
    pub(crate) routing_revision: String,
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

fn routing_revision(base_url: &str, callback_ports: &[i32]) -> String {
    let ports = callback_ports
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    crate::utils::codec::sha256_str(&format!("{base_url}\0{ports}"))[..16].to_string()
}

fn runtime(
    base_url: &str,
    bind_addr: &str,
    game_id: i32,
    challenge_id: i32,
    secret: String,
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
        routing_revision: routing_revision(base_url, &callback_ports),
        callback_ports,
    })
}

/// Load or create the reporter secret for one lifecycle reset and routing
/// contract. A same-route retry keeps the first secret so an adopted container
/// and the database agree; changing the route rotates it before replacement
/// creation, immediately revoking any crash-orphaned target.
pub(crate) async fn ensure_for_cycle(
    pool: &sqlx::PgPool,
    base_url: Option<&str>,
    bind_addr: &str,
    cycle_id: i64,
    game_id: i32,
    challenge_id: i32,
    reset_attempt: i32,
) -> AppResult<Option<TargetReporterRuntime>> {
    let Some(base_url) = base_url else {
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
        return Ok(None);
    }

    let candidate = format!(
        "{REPORTER_SECRET_PREFIX}{}",
        crate::utils::codec::random_token(REPORTER_SECRET_BYTES)
    );
    let normalized_base_url = base_url.trim_end_matches('/');
    let effective_ports = callback_ports(normalized_base_url, bind_addr)?;
    let current_routing_revision = routing_revision(normalized_base_url, &effective_ports);
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
    runtime(base_url, bind_addr, game_id, challenge_id, secret).map(Some)
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

    #[test]
    fn reporter_runtime_contains_exact_scoped_endpoints_and_default_ports() {
        let http = runtime(
            "http://rsctf-control",
            "0.0.0.0:8080",
            7,
            9,
            "secret".to_string(),
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
        )
        .unwrap();
        assert_eq!(https.callback_ports, vec![443, 8080]);
        let custom = runtime(
            "http://rsctf-koth-reporter:8080",
            "0.0.0.0:8080",
            7,
            9,
            "secret".to_string(),
        )
        .unwrap();
        assert_eq!(custom.callback_ports, vec![8080]);
        assert_ne!(http.routing_revision, https.routing_revision);
        assert_ne!(http.routing_revision, custom.routing_revision);
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
            Some("http://rsctf-koth-reporter:8080"),
            "0.0.0.0:8080",
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
            Some("http://rsctf-koth-reporter:8080"),
            "0.0.0.0:8080",
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
            Some("https://rsctf-koth-reporter"),
            "0.0.0.0:8080",
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
            Some("https://rsctf-koth-reporter"),
            "0.0.0.0:8080",
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
            Some("http://rsctf-koth-reporter:8080"),
            "0.0.0.0:8080",
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

        sqlx::query(r#"UPDATE "KothCrownCycles" SET reset_attempt = 2 WHERE id = 41"#)
            .execute(&pool)
            .await
            .unwrap();
        let replacement = ensure_for_cycle(
            &pool,
            Some("http://rsctf-koth-reporter:8080"),
            "0.0.0.0:8080",
            41,
            7,
            9,
            2,
        )
        .await
        .unwrap()
        .unwrap();
        assert_ne!(
            reporter_secret(&restored_route),
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
