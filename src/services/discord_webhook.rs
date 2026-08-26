//! Durable Discord delivery for Jeopardy blood announcements.
//!
//! A solve transaction stores only the notice id in PostgreSQL. The webhook
//! credential remains on `Games`, never in the outbox or logs. A bounded worker
//! claims rows with a recoverable lease and performs outbound I/O after the
//! grading transaction has committed.

use std::sync::LazyLock;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::{stream, StreamExt};
use reqwest::{StatusCode, Url};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::utils::enums::NoticeType;
use crate::utils::error::{AppError, AppResult};

const CLAIM_LIMIT: i64 = 16;
const MAX_CONCURRENT_DELIVERIES: usize = 4;
const MAX_ATTEMPTS: i32 = 8;
const LEASE_SECONDS: i64 = 30;
const MAX_RETRY_SECONDS: u64 = 300;
const FROZEN_DEFER_LIMIT: i64 = 256;
const TERMINAL_CLEANUP_LIMIT: i64 = 256;
const TERMINAL_RETENTION_SECONDS: i64 = 7 * 24 * 60 * 60;
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const CLEANUP_INTERVAL: Duration = Duration::from_secs(60);

// ASCII `DBLD`. This is intentionally distinct from submit's `(0,
// challenge_id)` lock. Holding it immediately before allocating a GameNotices
// id makes successful per-game blood ids visible in commit order.
const BLOOD_NOTICE_LOCK_NAMESPACE: i32 = 0x4442_4c44;

static HTTP_CLIENT: LazyLock<Result<reqwest::Client, String>> = LazyLock::new(|| {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .user_agent("rsctf-discord-webhook/1")
        .build()
        .map_err(|error| error.to_string())
});

#[derive(Debug, sqlx::FromRow)]
struct LeasedDelivery {
    notice_id: i32,
    game_id: i32,
    attempts: i32,
    notice_type: i16,
    values: Value,
    publish_time_utc: DateTime<Utc>,
    game_title: String,
    webhook_url: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum DeliveryDisposition {
    Delivered {
        status: i16,
    },
    Retry {
        status: Option<i16>,
        delay_seconds: u64,
        reason: &'static str,
    },
    Dead {
        status: Option<i16>,
        reason: &'static str,
    },
}

fn invalid_webhook() -> AppError {
    AppError::bad_request("Discord webhook must be an HTTPS discord.com API webhook URL")
}

/// Validate and normalize an organizer-provided webhook without ever including
/// its token in an error. Restricting the origin and path prevents the webhook
/// field from becoming a server-side request forgery primitive.
pub fn normalize_discord_webhook(raw: Option<&str>) -> AppResult<Option<String>> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let mut url = Url::parse(raw).map_err(|_| invalid_webhook())?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid_webhook());
    }

    match url.host_str() {
        Some("discord.com") => {}
        Some("discordapp.com") => {
            url.set_host(Some("discord.com"))
                .map_err(|_| invalid_webhook())?;
        }
        _ => return Err(invalid_webhook()),
    }

    let segments = url
        .path_segments()
        .ok_or_else(invalid_webhook)?
        .collect::<Vec<_>>();
    let (webhook_id, token) = match segments.as_slice() {
        ["api", "webhooks", webhook_id, token] => (*webhook_id, *token),
        ["api", version, "webhooks", webhook_id, token]
            if version.strip_prefix('v').is_some_and(|value| {
                !value.is_empty() && value.chars().all(|c| c.is_ascii_digit())
            }) =>
        {
            (*webhook_id, *token)
        }
        _ => return Err(invalid_webhook()),
    };
    if webhook_id.is_empty()
        || webhook_id.len() > 32
        || !webhook_id.chars().all(|c| c.is_ascii_digit())
        || !(16..=256).contains(&token.len())
        || !token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(invalid_webhook());
    }

    let mut thread_id: Option<String> = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "thread_id"
                if thread_id.is_none()
                    && !value.is_empty()
                    && value.len() <= 32
                    && value.chars().all(|c| c.is_ascii_digit()) =>
            {
                thread_id = Some(value.into_owned());
            }
            // `wait` is controlled by the sender so a successful response is
            // observable. Accept a valid configured value but do not persist it.
            "wait" if value == "true" || value == "false" => {}
            _ => return Err(invalid_webhook()),
        }
    }
    url.set_query(None);
    if let Some(thread_id) = thread_id {
        url.query_pairs_mut().append_pair("thread_id", &thread_id);
    }
    Ok(Some(url.to_string()))
}

fn delivery_endpoint(normalized: &str) -> Result<Url, &'static str> {
    let mut url = Url::parse(normalized).map_err(|_| "invalid_webhook_url")?;
    url.query_pairs_mut().append_pair("wait", "true");
    Ok(url)
}

fn escape_discord_markdown(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(
            character,
            '\\' | '*' | '_' | '~' | '`' | '>' | '|' | '[' | ']' | '(' | ')'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn safe_discord_text(value: &str, maximum: usize) -> String {
    let normalized = value.chars().map(|character| {
        if character.is_control() || matches!(character, '\u{2028}' | '\u{2029}') {
            ' '
        } else {
            character
        }
    });
    escape_discord_markdown(&normalized.take(maximum).collect::<String>())
}

fn delivery_payload(job: &LeasedDelivery) -> Result<Value, &'static str> {
    let (title, color) = match job.notice_type {
        value if value == NoticeType::FirstBlood as i16 => ("🩸 First Blood", 0xed_42_45),
        value if value == NoticeType::SecondBlood as i16 => ("🥈 Second Blood", 0x99_aab5),
        value if value == NoticeType::ThirdBlood as i16 => ("🥉 Third Blood", 0xcd_7f32),
        _ => return Err("unsupported_notice_type"),
    };
    let values = job.values.as_array().ok_or("invalid_notice_values")?;
    let team = values
        .first()
        .and_then(Value::as_str)
        .ok_or("invalid_notice_values")?;
    let challenge = values
        .get(1)
        .and_then(Value::as_str)
        .ok_or("invalid_notice_values")?;
    let team = safe_discord_text(team, 200);
    let challenge = safe_discord_text(challenge, 300);
    let mut game = safe_discord_text(&job.game_title, 300);
    if game.trim().is_empty() {
        game = "Untitled event".to_string();
    }

    Ok(json!({
        "username": "RSCTF",
        "allowed_mentions": { "parse": [] },
        "embeds": [{
            "title": title,
            "description": format!("**{team}** solved **{challenge}**."),
            "color": color,
            "fields": [{ "name": "Event", "value": game, "inline": false }],
            "timestamp": job.publish_time_utc.to_rfc3339(),
            "footer": { "text": format!("RSCTF notice #{}", job.notice_id) }
        }]
    }))
}

fn retry_delay(attempts: i32) -> u64 {
    let shift = u32::try_from(attempts.clamp(1, 8) - 1).unwrap_or(7);
    2_u64
        .checked_shl(shift)
        .unwrap_or(MAX_RETRY_SECONDS)
        .min(MAX_RETRY_SECONDS)
}

fn retry_after_seconds(response: &reqwest::Response) -> Option<u64> {
    ["retry-after", "x-ratelimit-reset-after"]
        .into_iter()
        .find_map(|header| {
            response
                .headers()
                .get(header)?
                .to_str()
                .ok()?
                .parse::<f64>()
                .ok()
        })
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .map(|seconds| (seconds.ceil() as u64).clamp(1, MAX_RETRY_SECONDS))
}

fn classify_status(
    status: StatusCode,
    retry_after: Option<u64>,
    attempts: i32,
) -> DeliveryDisposition {
    let status_code = i16::try_from(status.as_u16()).expect("HTTP status fits i16");
    if status.is_success() {
        return DeliveryDisposition::Delivered {
            status: status_code,
        };
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return DeliveryDisposition::Retry {
            status: Some(status_code),
            delay_seconds: retry_after.unwrap_or_else(|| retry_delay(attempts)),
            reason: "discord_rate_limited",
        };
    }
    if status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_EARLY
        || status.is_server_error()
    {
        return DeliveryDisposition::Retry {
            status: Some(status_code),
            delay_seconds: retry_delay(attempts),
            reason: "discord_temporary_failure",
        };
    }
    DeliveryDisposition::Dead {
        status: Some(status_code),
        reason: "discord_rejected_webhook",
    }
}

async fn send(job: &LeasedDelivery) -> DeliveryDisposition {
    let normalized = match normalize_discord_webhook(job.webhook_url.as_deref()) {
        Ok(Some(url)) => url,
        Ok(None) => {
            return DeliveryDisposition::Dead {
                status: None,
                reason: "discord_webhook_not_configured",
            };
        }
        Err(_) => {
            return DeliveryDisposition::Dead {
                status: None,
                reason: "invalid_discord_webhook",
            };
        }
    };
    let endpoint = match delivery_endpoint(&normalized) {
        Ok(endpoint) => endpoint,
        Err(reason) => {
            return DeliveryDisposition::Dead {
                status: None,
                reason,
            };
        }
    };
    let payload = match delivery_payload(job) {
        Ok(payload) => payload,
        Err(reason) => {
            return DeliveryDisposition::Dead {
                status: None,
                reason,
            };
        }
    };
    let client = match HTTP_CLIENT.as_ref() {
        Ok(client) => client,
        Err(_) => {
            return DeliveryDisposition::Retry {
                status: None,
                delay_seconds: retry_delay(job.attempts),
                reason: "discord_http_client_unavailable",
            };
        }
    };
    match client.post(endpoint).json(&payload).send().await {
        Ok(response) => {
            let retry_after = retry_after_seconds(&response);
            classify_status(response.status(), retry_after, job.attempts)
        }
        Err(error) => DeliveryDisposition::Retry {
            status: None,
            delay_seconds: retry_delay(job.attempts),
            reason: if error.is_timeout() {
                "discord_request_timeout"
            } else if error.is_connect() {
                "discord_connection_failed"
            } else {
                "discord_request_failed"
            },
        },
    }
}

/// Serialize canonical blood-notice id allocation for one event. Call this
/// immediately before inserting `GameNotices`, and keep the remaining
/// transaction tail short so a committed successor can never overtake an
/// invisible predecessor.
pub async fn lock_game_blood_notice_order(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
) -> AppResult<()> {
    sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
        .bind(BLOOD_NOTICE_LOCK_NAMESPACE)
        .bind(game_id)
        .execute(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

/// Enqueue delivery in the same transaction as the canonical game notice. A
/// blank webhook intentionally creates no row. `ON CONFLICT` makes replay safe.
pub async fn enqueue_blood_notice(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    notice_id: i32,
    game_id: i32,
    available_at_utc: DateTime<Utc>,
) -> AppResult<bool> {
    let inserted = sqlx::query(
        r#"INSERT INTO "DiscordWebhookOutbox"
               (notice_id, game_id, available_at_utc, freeze_deferred)
           SELECT $1, game.id,
                  CASE WHEN game.freeze_time_utc IS NOT NULL
                                  AND $3 >= game.freeze_time_utc
                                  AND $3 < game.end_time_utc
                       THEN game.end_time_utc
                       ELSE $3
                  END,
                  game.freeze_time_utc IS NOT NULL
                      AND $3 >= game.freeze_time_utc
                      AND $3 < game.end_time_utc
             FROM "Games" game
            WHERE game.id = $2
              AND game.discord_webhook IS NOT NULL
              AND btrim(game.discord_webhook) <> ''
           ON CONFLICT (notice_id) DO NOTHING"#,
    )
    .bind(notice_id)
    .bind(game_id)
    .bind(available_at_utc)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(inserted.rows_affected() == 1)
}

/// Recompute pending delivery times when an organizer edits the event freeze
/// window. Explicit deferral state distinguishes retries postponed by a freeze
/// from ordinary backoff that happens to share an event timestamp.
pub async fn reschedule_game_blood_notices(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    old_freeze_time_utc: Option<DateTime<Utc>>,
    old_end_time_utc: DateTime<Utc>,
    new_freeze_time_utc: Option<DateTime<Utc>>,
    new_end_time_utc: DateTime<Utc>,
) -> AppResult<u64> {
    let affected = sqlx::query(
        r#"WITH observed_clock AS MATERIALIZED (
               SELECT clock_timestamp() AS db_now
           )
           UPDATE "DiscordWebhookOutbox" job
              SET available_at_utc =
                  CASE WHEN $4 IS NOT NULL
                                  AND (
                                      (notice.publish_time_utc >= $4
                                       AND notice.publish_time_utc < $5)
                                      OR
                                      (job.freeze_deferred
                                       AND observed_clock.db_now >= $4
                                       AND observed_clock.db_now < $5)
                                  )
                       THEN $5
                       ELSE LEAST(job.available_at_utc, observed_clock.db_now)
                  END,
                  freeze_deferred =
                  CASE WHEN $4 IS NOT NULL
                                  AND (
                                      (notice.publish_time_utc >= $4
                                       AND notice.publish_time_utc < $5)
                                      OR
                                      (job.freeze_deferred
                                       AND observed_clock.db_now >= $4
                                       AND observed_clock.db_now < $5)
                                  )
                       THEN TRUE
                       ELSE FALSE
                  END
             FROM "GameNotices" notice, observed_clock
            WHERE job.notice_id = notice.id
              AND job.game_id = $1
              AND job.delivered_at_utc IS NULL
              AND job.dead_at_utc IS NULL
              AND (
                  ($2 IS NOT NULL
                   AND notice.publish_time_utc >= $2
                   AND notice.publish_time_utc < $3)
                  OR
                  ($4 IS NOT NULL
                   AND notice.publish_time_utc >= $4
                   AND notice.publish_time_utc < $5)
                  OR job.freeze_deferred
              )
              AND (
                  job.available_at_utc IS DISTINCT FROM
                      CASE WHEN $4 IS NOT NULL
                                      AND (
                                          (notice.publish_time_utc >= $4
                                           AND notice.publish_time_utc < $5)
                                          OR
                                          (job.freeze_deferred
                                           AND observed_clock.db_now >= $4
                                           AND observed_clock.db_now < $5)
                                      )
                           THEN $5
                           ELSE LEAST(job.available_at_utc, observed_clock.db_now)
                      END
                  OR job.freeze_deferred IS DISTINCT FROM
                      ($4 IS NOT NULL
                       AND (
                           (notice.publish_time_utc >= $4
                            AND notice.publish_time_utc < $5)
                           OR
                           (job.freeze_deferred
                            AND observed_clock.db_now >= $4
                            AND observed_clock.db_now < $5)
                       ))
              )"#,
    )
    .bind(game_id)
    .bind(old_freeze_time_utc)
    .bind(old_end_time_utc)
    .bind(new_freeze_time_utc)
    .bind(new_end_time_utc)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(affected.rows_affected())
}

/// Repair stale due rows without scanning or rewriting an unbounded batch.
/// This is a fail-safe for a solve racing a schedule edit or an interrupted
/// older deployment; normal enqueue and edit paths already schedule precisely.
async fn defer_frozen(pool: &sqlx::PgPool, limit: i64) -> AppResult<u64> {
    let affected = sqlx::query(
        r#"WITH observed_clock AS MATERIALIZED (
               SELECT clock_timestamp() AS db_now
           ), due AS MATERIALIZED (
               SELECT job.notice_id, game.end_time_utc
                 FROM "DiscordWebhookOutbox" job
                 JOIN "Games" game ON game.id = job.game_id
                 CROSS JOIN observed_clock
                WHERE job.delivered_at_utc IS NULL
                  AND job.dead_at_utc IS NULL
                  AND job.available_at_utc <= observed_clock.db_now
                  AND game.freeze_time_utc IS NOT NULL
                  AND observed_clock.db_now >= game.freeze_time_utc
                  AND observed_clock.db_now < game.end_time_utc
                ORDER BY job.available_at_utc, job.notice_id
                LIMIT $1
                FOR UPDATE OF job SKIP LOCKED
           )
           UPDATE "DiscordWebhookOutbox" job
              SET available_at_utc = due.end_time_utc,
                  freeze_deferred = TRUE
             FROM due
            WHERE job.notice_id = due.notice_id"#,
    )
    .bind(limit.clamp(1, 1_024))
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(affected.rows_affected())
}

/// Retain a short diagnostic window, then delete terminal rows in small,
/// indexed batches so the outbox cannot grow with the lifetime of the server.
async fn purge_terminal(pool: &sqlx::PgPool, limit: i64) -> AppResult<u64> {
    let affected = sqlx::query(
        r#"WITH expired AS MATERIALIZED (
               SELECT notice_id
                 FROM "DiscordWebhookOutbox"
                WHERE COALESCE(delivered_at_utc, dead_at_utc)
                      < clock_timestamp() - ($2::bigint * INTERVAL '1 second')
                ORDER BY COALESCE(delivered_at_utc, dead_at_utc), notice_id
                LIMIT $1
                FOR UPDATE SKIP LOCKED
           )
           DELETE FROM "DiscordWebhookOutbox" job
            USING expired
            WHERE job.notice_id = expired.notice_id"#,
    )
    .bind(limit.clamp(1, 1_024))
    .bind(TERMINAL_RETENTION_SECONDS)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(affected.rows_affected())
}

async fn expire_exhausted(pool: &sqlx::PgPool) -> AppResult<u64> {
    let affected = sqlx::query(
        r#"UPDATE "DiscordWebhookOutbox"
              SET dead_at_utc = clock_timestamp(),
                  lease_token = NULL,
                  lease_expires_at_utc = NULL,
                  last_error = 'retry_budget_exhausted'
            WHERE delivered_at_utc IS NULL
              AND dead_at_utc IS NULL
              AND attempts >= $1
              AND (lease_expires_at_utc IS NULL
                   OR lease_expires_at_utc <= clock_timestamp())"#,
    )
    .bind(MAX_ATTEMPTS)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(affected.rows_affected())
}

async fn claim_pending(pool: &sqlx::PgPool, limit: i64) -> AppResult<(Uuid, Vec<LeasedDelivery>)> {
    let lease_token = Uuid::new_v4();
    let rows = sqlx::query_as::<_, LeasedDelivery>(
        r#"WITH observed_clock AS MATERIALIZED (
               SELECT clock_timestamp() AS db_now
           ), candidates AS MATERIALIZED (
               SELECT job.notice_id
                 FROM "DiscordWebhookOutbox" job
                 JOIN "Games" game ON game.id = job.game_id
                 CROSS JOIN observed_clock
                WHERE job.delivered_at_utc IS NULL
                  AND job.dead_at_utc IS NULL
                  AND job.attempts < $4
                  AND job.available_at_utc <= observed_clock.db_now
                  AND (job.lease_expires_at_utc IS NULL
                       OR job.lease_expires_at_utc <= observed_clock.db_now)
                  AND NOT EXISTS (
                      SELECT 1
                        FROM "DiscordWebhookOutbox" earlier
                       WHERE earlier.game_id = job.game_id
                         AND earlier.notice_id < job.notice_id
                         AND earlier.delivered_at_utc IS NULL
                         AND earlier.dead_at_utc IS NULL
                  )
                  AND (game.freeze_time_utc IS NULL
                       OR observed_clock.db_now < game.freeze_time_utc
                       OR observed_clock.db_now >= game.end_time_utc)
                ORDER BY job.available_at_utc, job.notice_id
                LIMIT $1
                FOR UPDATE OF job SKIP LOCKED
           ), leased AS (
               UPDATE "DiscordWebhookOutbox" job
                  SET lease_token = $2,
                      lease_expires_at_utc = clock_timestamp()
                          + ($3::bigint * INTERVAL '1 second'),
                      attempts = attempts + 1
                 FROM candidates
                WHERE job.notice_id = candidates.notice_id
            RETURNING job.notice_id, job.game_id, job.attempts
           )
           SELECT leased.notice_id, leased.game_id, leased.attempts,
                  notice."Type" AS notice_type, notice.values,
                  notice.publish_time_utc, game.title AS game_title,
                  game.discord_webhook AS webhook_url
             FROM leased
             JOIN "GameNotices" notice ON notice.id = leased.notice_id
             JOIN "Games" game ON game.id = leased.game_id
            ORDER BY leased.notice_id"#,
    )
    .bind(limit.clamp(1, 64))
    .bind(lease_token)
    .bind(LEASE_SECONDS)
    .bind(MAX_ATTEMPTS)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok((lease_token, rows))
}

async fn finish_job(
    pool: &sqlx::PgPool,
    job: &LeasedDelivery,
    lease_token: Uuid,
    disposition: DeliveryDisposition,
) -> AppResult<()> {
    let affected = match disposition {
        DeliveryDisposition::Delivered { status } => {
            sqlx::query(
                r#"UPDATE "DiscordWebhookOutbox"
                  SET delivered_at_utc = clock_timestamp(),
                      lease_token = NULL,
                      lease_expires_at_utc = NULL,
                      last_http_status = $3,
                      last_error = NULL
                WHERE notice_id = $1 AND lease_token = $2"#,
            )
            .bind(job.notice_id)
            .bind(lease_token)
            .bind(status)
            .execute(pool)
            .await
        }
        DeliveryDisposition::Retry {
            status,
            delay_seconds,
            reason,
        } if job.attempts < MAX_ATTEMPTS => {
            sqlx::query(
                r#"UPDATE "DiscordWebhookOutbox"
                  SET available_at_utc = clock_timestamp()
                        + ($3::bigint * INTERVAL '1 second'),
                      freeze_deferred = FALSE,
                      lease_token = NULL,
                      lease_expires_at_utc = NULL,
                      last_http_status = $4,
                      last_error = $5
                WHERE notice_id = $1 AND lease_token = $2"#,
            )
            .bind(job.notice_id)
            .bind(lease_token)
            .bind(i64::try_from(delay_seconds).unwrap_or(i64::from(i32::MAX)))
            .bind(status)
            .bind(reason)
            .execute(pool)
            .await
        }
        DeliveryDisposition::Retry { status, reason, .. }
        | DeliveryDisposition::Dead { status, reason } => {
            sqlx::query(
                r#"UPDATE "DiscordWebhookOutbox"
                  SET dead_at_utc = clock_timestamp(),
                      lease_token = NULL,
                      lease_expires_at_utc = NULL,
                      last_http_status = $3,
                      last_error = $4
                WHERE notice_id = $1 AND lease_token = $2"#,
            )
            .bind(job.notice_id)
            .bind(lease_token)
            .bind(status)
            .bind(reason)
            .execute(pool)
            .await
        }
    }
    .map_err(|error| AppError::internal(error.to_string()))?;
    if affected.rows_affected() != 1 {
        return Err(AppError::internal(
            "Discord webhook delivery lease was lost",
        ));
    }
    Ok(())
}

async fn deliver_one(pool: &sqlx::PgPool, lease_token: Uuid, job: LeasedDelivery) -> AppResult<()> {
    let disposition = send(&job).await;
    let delivered = matches!(disposition, DeliveryDisposition::Delivered { .. });
    let reason = match &disposition {
        DeliveryDisposition::Delivered { .. } => None,
        DeliveryDisposition::Retry { reason, .. } | DeliveryDisposition::Dead { reason, .. } => {
            Some(*reason)
        }
    };
    finish_job(pool, &job, lease_token, disposition).await?;
    if delivered {
        tracing::info!(
            notice_id = job.notice_id,
            game_id = job.game_id,
            attempts = job.attempts,
            "delivered Discord blood announcement"
        );
    } else {
        tracing::warn!(
            notice_id = job.notice_id,
            game_id = job.game_id,
            attempts = job.attempts,
            reason = reason.unwrap_or("unknown"),
            "Discord blood announcement was not delivered"
        );
    }
    Ok(())
}

/// Process a bounded batch. At most one notice per game is claimable, preserving
/// first/second/third blood order across retries and freeze release. Independent
/// games remain concurrent and capped; no transaction or row lock is held during
/// outbound I/O.
pub async fn reconcile(pool: &sqlx::PgPool, limit: i64) -> AppResult<usize> {
    expire_exhausted(pool).await?;
    defer_frozen(pool, FROZEN_DEFER_LIMIT).await?;
    let (lease_token, jobs) = claim_pending(pool, limit).await?;
    let claimed = jobs.len();
    let results = stream::iter(
        jobs.into_iter()
            .map(|job| async move { deliver_one(pool, lease_token, job).await }),
    )
    .buffer_unordered(MAX_CONCURRENT_DELIVERIES)
    .collect::<Vec<_>>()
    .await;
    for result in results {
        result?;
    }
    Ok(claimed)
}

/// Start an active-active durable delivery worker. PostgreSQL leases arbitrate
/// multiple control/engine replicas, while API-only replicas merely enqueue.
pub fn start_reconciler(
    state: SharedState,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut next_cleanup = tokio::time::Instant::now();
        loop {
            if *shutdown.borrow() {
                break;
            }
            let now = tokio::time::Instant::now();
            if now >= next_cleanup {
                if let Err(error) = purge_terminal(state.pg(), TERMINAL_CLEANUP_LIMIT).await {
                    tracing::error!(%error, "Discord webhook retention cleanup failed");
                }
                next_cleanup = now + CLEANUP_INTERVAL;
            }
            if let Err(error) = reconcile(state.pg(), CLAIM_LIMIT).await {
                tracing::error!(%error, "Discord webhook reconciler pass failed");
            }
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                () = tokio::time::sleep(POLL_INTERVAL) => {}
            }
        }
    })
}

#[cfg(test)]
#[path = "discord_webhook_tests.rs"]
mod tests;
