use std::str::FromStr;

use chrono::Utc;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::*;

const TOKEN: &str = "abcdefghijklmnopqrstuvwxyz_1234567890";

fn webhook(host: &str) -> String {
    format!("https://{host}/api/webhooks/123456789012345678/{TOKEN}")
}

fn leased(notice_type: NoticeType) -> LeasedDelivery {
    LeasedDelivery {
        notice_id: 41,
        game_id: 7,
        attempts: 1,
        notice_type: notice_type as i16,
        values: json!(["team_*@everyone", "challenge_[one]"]),
        publish_time_utc: Utc::now(),
        game_title: "event_*".to_string(),
        webhook_url: Some(webhook("discord.com")),
    }
}

#[test]
fn webhook_validation_allowlists_only_discord_api_targets() {
    let canonical = normalize_discord_webhook(Some(&webhook("discord.com")))
        .unwrap()
        .unwrap();
    assert!(canonical.starts_with("https://discord.com/api/webhooks/"));

    let legacy = format!(
        "{}?wait=false&thread_id=987654321",
        webhook("discordapp.com")
    );
    let normalized = normalize_discord_webhook(Some(&legacy)).unwrap().unwrap();
    assert!(normalized.starts_with("https://discord.com/api/webhooks/"));
    assert!(normalized.ends_with("?thread_id=987654321"));
    assert!(!normalized.contains("wait="));

    assert_eq!(normalize_discord_webhook(None).unwrap(), None);
    assert_eq!(normalize_discord_webhook(Some("  ")).unwrap(), None);
    for invalid in [
        format!("http://discord.com/api/webhooks/123456789012345678/{TOKEN}"),
        format!("https://example.com/api/webhooks/123456789012345678/{TOKEN}"),
        format!("https://discord.com.example/api/webhooks/123456789012345678/{TOKEN}"),
        format!("https://user@discord.com/api/webhooks/123456789012345678/{TOKEN}"),
        format!("https://discord.com:444/api/webhooks/123456789012345678/{TOKEN}"),
        format!("{}/extra", webhook("discord.com")),
        format!("{}?redirect=https://example.com", webhook("discord.com")),
    ] {
        let error = normalize_discord_webhook(Some(&invalid)).unwrap_err();
        assert!(!error.to_string().contains(TOKEN));
    }
}

#[test]
fn sender_forces_observable_discord_responses() {
    let configured = format!("{}?thread_id=987654321", webhook("discord.com"));
    let endpoint = delivery_endpoint(&configured).unwrap();
    let query = endpoint.query().unwrap();
    assert!(query.contains("thread_id=987654321"));
    assert!(query.contains("wait=true"));
}

#[test]
fn payload_blocks_mentions_and_escapes_untrusted_markdown() {
    let payload = delivery_payload(&leased(NoticeType::FirstBlood)).unwrap();
    assert_eq!(payload["allowed_mentions"]["parse"], json!([]));
    assert_eq!(payload["embeds"][0]["title"], "🩸 First Blood");
    let description = payload["embeds"][0]["description"].as_str().unwrap();
    assert!(description.contains("team\\_\\*"));
    assert!(description.contains("challenge\\_\\[one\\]"));
    assert!(description.contains("@everyone"));
    assert_eq!(payload["embeds"][0]["fields"][0]["value"], "event\\_\\*");
    assert!(delivery_payload(&leased(NoticeType::Normal)).is_err());
}

#[test]
fn retry_policy_is_bounded_and_distinguishes_permanent_rejection() {
    for attempts in 1..10_000 {
        assert!((2..=MAX_RETRY_SECONDS).contains(&retry_delay(attempts)));
    }
    assert_eq!(
        classify_status(StatusCode::NO_CONTENT, None, 1),
        DeliveryDisposition::Delivered { status: 204 }
    );
    assert_eq!(
        classify_status(StatusCode::TOO_MANY_REQUESTS, Some(17), 1),
        DeliveryDisposition::Retry {
            status: Some(429),
            delay_seconds: 17,
            reason: "discord_rate_limited",
        }
    );
    assert!(matches!(
        classify_status(StatusCode::BAD_GATEWAY, None, 2),
        DeliveryDisposition::Retry { .. }
    ));
    assert!(matches!(
        classify_status(StatusCode::NOT_FOUND, None, 2),
        DeliveryDisposition::Dead { .. }
    ));
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn outbox_enqueue_claim_freeze_and_completion_are_durable() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("discord_outbox_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TABLE "Games" (
          id INTEGER PRIMARY KEY,
          title TEXT NOT NULL,
          discord_webhook TEXT,
          freeze_time_utc TIMESTAMPTZ,
          end_time_utc TIMESTAMPTZ NOT NULL
        );
        CREATE TABLE "GameNotices" (
          id INTEGER PRIMARY KEY,
          game_id INTEGER NOT NULL,
          "Type" SMALLINT NOT NULL,
          values JSON NOT NULL,
          publish_time_utc TIMESTAMPTZ NOT NULL
        );
        CREATE TABLE "DiscordWebhookOutbox" (
          notice_id INTEGER PRIMARY KEY REFERENCES "GameNotices"(id) ON DELETE CASCADE,
          game_id INTEGER NOT NULL REFERENCES "Games"(id) ON DELETE CASCADE,
          attempts INTEGER NOT NULL DEFAULT 0,
          available_at_utc TIMESTAMPTZ NOT NULL,
          lease_token UUID,
          lease_expires_at_utc TIMESTAMPTZ,
          delivered_at_utc TIMESTAMPTZ,
          dead_at_utc TIMESTAMPTZ,
          last_http_status SMALLINT,
          last_error VARCHAR(256),
          created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
        );
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    for (id, frozen) in [(1_i32, false), (2_i32, true), (3_i32, false)] {
        let configured = (id != 3).then(|| webhook("discord.com"));
        sqlx::query(
            r#"INSERT INTO "Games"
                 (id, title, discord_webhook, freeze_time_utc, end_time_utc)
               VALUES ($1, $2, $3,
                       CASE WHEN $4 THEN clock_timestamp() - INTERVAL '1 minute'
                            ELSE NULL END,
                       clock_timestamp() + INTERVAL '1 hour')"#,
        )
        .bind(id)
        .bind(format!("Game {id}"))
        .bind(configured)
        .bind(frozen)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "GameNotices"
                 (id, game_id, "Type", values, publish_time_utc)
               VALUES ($1, $2, $3, $4, clock_timestamp())"#,
        )
        .bind(id)
        .bind(id)
        .bind(NoticeType::FirstBlood as i16)
        .bind(sqlx::types::Json(json!(["Team", "Challenge"])))
        .execute(&pool)
        .await
        .unwrap();
    }

    let mut transaction = pool.begin().await.unwrap();
    assert!(enqueue_blood_notice(&mut transaction, 1, 1, Utc::now())
        .await
        .unwrap());
    assert!(!enqueue_blood_notice(&mut transaction, 1, 1, Utc::now())
        .await
        .unwrap());
    assert!(enqueue_blood_notice(&mut transaction, 2, 2, Utc::now())
        .await
        .unwrap());
    assert!(!enqueue_blood_notice(&mut transaction, 3, 3, Utc::now())
        .await
        .unwrap());
    transaction.commit().await.unwrap();

    let (lease_token, jobs) = claim_pending(&pool, 16).await.unwrap();
    assert_eq!(jobs.len(), 1, "the frozen event must remain queued");
    assert_eq!(jobs[0].notice_id, 1);
    finish_job(
        &pool,
        &jobs[0],
        lease_token,
        DeliveryDisposition::Delivered { status: 200 },
    )
    .await
    .unwrap();
    let delivered: bool = sqlx::query_scalar(
        r#"SELECT delivered_at_utc IS NOT NULL
             FROM "DiscordWebhookOutbox" WHERE notice_id = 1"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(delivered);
    let frozen_pending: bool = sqlx::query_scalar(
        r#"SELECT delivered_at_utc IS NULL AND dead_at_utc IS NULL
             FROM "DiscordWebhookOutbox" WHERE notice_id = 2"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(frozen_pending);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
