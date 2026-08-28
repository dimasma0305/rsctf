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

fn runtime(
    base_url: &str,
    bind_addr: &str,
    game_id: i32,
    challenge_id: i32,
    secret: String,
) -> AppResult<TargetReporterRuntime> {
    let base_url = base_url.trim_end_matches('/');
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
        callback_ports: callback_ports(base_url, bind_addr)?,
    })
}

/// Load or create the one reporter secret for an exact lifecycle reset. A
/// concurrent retry keeps the first secret so an adopted container and the
/// database can never disagree about the injected credential.
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
    let secret: Option<String> = sqlx::query_scalar(
        r#"INSERT INTO "KothTargetReporters"
             (cycle_id, game_id, challenge_id, reset_attempt,
              hmac_secret, issued_at, expires_at, last_used_at)
           SELECT cycle.id, cycle.game_id, cycle.challenge_id,
                  cycle.reset_attempt, $5, clock_timestamp(),
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
             hmac_secret = CASE
               WHEN "KothTargetReporters".reset_attempt = EXCLUDED.reset_attempt
                 THEN "KothTargetReporters".hmac_secret
               ELSE EXCLUDED.hmac_secret
             END,
             issued_at = CASE
               WHEN "KothTargetReporters".reset_attempt = EXCLUDED.reset_attempt
                 THEN "KothTargetReporters".issued_at
               ELSE clock_timestamp()
             END,
             expires_at = EXCLUDED.expires_at,
             last_used_at = CASE
               WHEN "KothTargetReporters".reset_attempt = EXCLUDED.reset_attempt
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
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn retry_keeps_one_secret_and_a_new_reset_rotates_it() {
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
        assert_ne!(reporter_secret(&first), reporter_secret(&replacement));
        let stored_attempt: i32 = sqlx::query_scalar(
            r#"SELECT reset_attempt FROM "KothTargetReporters" WHERE cycle_id = 41"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored_attempt, 2);
    }
}
