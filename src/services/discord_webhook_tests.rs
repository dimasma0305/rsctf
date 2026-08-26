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
    let mut job = leased(NoticeType::FirstBlood);
    job.values = json!([
        "team_*@everyone\n# forged\r\nline\u{2028}break",
        "challenge_[one]"
    ]);
    job.game_title = "event_*\n# forged".to_string();
    let payload = delivery_payload(&job).unwrap();
    assert_eq!(payload["allowed_mentions"]["parse"], json!([]));
    assert_eq!(payload["embeds"][0]["title"], "🩸 First Blood");
    let description = payload["embeds"][0]["description"].as_str().unwrap();
    assert!(description.contains("team\\_\\*@everyone # forged  line break"));
    assert!(description.contains("challenge\\_\\[one\\]"));
    assert!(description.contains("@everyone"));
    assert!(!description
        .chars()
        .any(|character| matches!(character, '\r' | '\n' | '\u{2028}' | '\u{2029}')));
    assert_eq!(
        payload["embeds"][0]["fields"][0]["value"],
        "event\\_\\* # forged"
    );

    job.game_title = "\n\u{0000}\u{2028}".to_string();
    let blank_event_payload = delivery_payload(&job).unwrap();
    assert_eq!(
        blank_event_payload["embeds"][0]["fields"][0]["value"],
        "Untitled event"
    );
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
        CREATE INDEX ix_discord_webhook_outbox_game_notice
          ON "DiscordWebhookOutbox" (game_id, notice_id);
        CREATE INDEX ix_discord_webhook_outbox_pending
          ON "DiscordWebhookOutbox" (available_at_utc, notice_id)
          WHERE delivered_at_utc IS NULL AND dead_at_utc IS NULL;
        CREATE INDEX ix_discord_webhook_outbox_terminal
          ON "DiscordWebhookOutbox"
             ((COALESCE(delivered_at_utc, dead_at_utc)), notice_id)
          WHERE delivered_at_utc IS NOT NULL OR dead_at_utc IS NOT NULL;
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    for (id, frozen) in [
        (1_i32, false),
        (2_i32, true),
        (3_i32, false),
        (4_i32, false),
    ] {
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
        if id <= 3 {
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
    }
    for (notice_id, game_id, notice_type) in [
        (4_i32, 1_i32, NoticeType::SecondBlood),
        (5_i32, 1_i32, NoticeType::ThirdBlood),
        (6_i32, 4_i32, NoticeType::FirstBlood),
    ] {
        sqlx::query(
            r#"INSERT INTO "GameNotices"
                 (id, game_id, "Type", values, publish_time_utc)
               VALUES ($1, $2, $3, $4, clock_timestamp())"#,
        )
        .bind(notice_id)
        .bind(game_id)
        .bind(notice_type as i16)
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
    for (notice_id, game_id) in [(4, 1), (5, 1), (6, 4)] {
        assert!(
            enqueue_blood_notice(&mut transaction, notice_id, game_id, Utc::now())
                .await
                .unwrap()
        );
    }
    transaction.commit().await.unwrap();

    let frozen_was_scheduled_once: bool = sqlx::query_scalar(
        r#"SELECT job.available_at_utc = game.end_time_utc
             FROM "DiscordWebhookOutbox" job
             JOIN "Games" game ON game.id = job.game_id
            WHERE job.notice_id = 2"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(frozen_was_scheduled_once);

    let (first_freeze, first_end): (Option<chrono::DateTime<Utc>>, chrono::DateTime<Utc>) =
        sqlx::query_as(r#"SELECT freeze_time_utc, end_time_utc FROM "Games" WHERE id = 2"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    let extended_end = first_end + chrono::Duration::minutes(30);
    let mut end_extension = pool.begin().await.unwrap();
    sqlx::query(r#"UPDATE "Games" SET end_time_utc = $2 WHERE id = $1"#)
        .bind(2_i32)
        .bind(extended_end)
        .execute(&mut *end_extension)
        .await
        .unwrap();
    assert_eq!(
        reschedule_game_blood_notices(
            &mut end_extension,
            2,
            first_freeze,
            first_end,
            first_freeze,
            extended_end,
        )
        .await
        .unwrap(),
        1
    );
    end_extension.commit().await.unwrap();
    let extension_rescheduled_to_new_end: bool = sqlx::query_scalar(
        r#"SELECT job.available_at_utc = game.end_time_utc
             FROM "DiscordWebhookOutbox" job
             JOIN "Games" game ON game.id = job.game_id
            WHERE job.notice_id = 2"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(extension_rescheduled_to_new_end);

    sqlx::query(
        r#"UPDATE "DiscordWebhookOutbox"
              SET available_at_utc = clock_timestamp() - INTERVAL '1 second'
            WHERE notice_id = 2"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(defer_frozen(&pool, 256).await.unwrap(), 1);
    let repaired_to_current_end: bool = sqlx::query_scalar(
        r#"SELECT job.available_at_utc = game.end_time_utc
             FROM "DiscordWebhookOutbox" job
             JOIN "Games" game ON game.id = job.game_id
            WHERE job.notice_id = 2"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(repaired_to_current_end);

    let (lease_token, jobs) = claim_pending(&pool, 16).await.unwrap();
    assert_eq!(
        jobs.iter().map(|job| job.notice_id).collect::<Vec<_>>(),
        vec![1, 6],
        "the frozen event must remain queued and each active game may lease only its oldest notice"
    );
    let (_, overtaking) = claim_pending(&pool, 16).await.unwrap();
    assert!(
        overtaking.is_empty(),
        "later bloods must not overtake a delayed leased delivery"
    );
    let first = jobs.iter().find(|job| job.notice_id == 1).unwrap();
    let independent = jobs.iter().find(|job| job.notice_id == 6).unwrap();
    finish_job(
        &pool,
        independent,
        lease_token,
        DeliveryDisposition::Delivered { status: 200 },
    )
    .await
    .unwrap();
    finish_job(
        &pool,
        first,
        lease_token,
        DeliveryDisposition::Delivered { status: 200 },
    )
    .await
    .unwrap();
    let (second_lease, second_jobs) = claim_pending(&pool, 16).await.unwrap();
    assert_eq!(
        second_jobs
            .iter()
            .map(|job| job.notice_id)
            .collect::<Vec<_>>(),
        vec![4]
    );
    finish_job(
        &pool,
        &second_jobs[0],
        second_lease,
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

    let (old_freeze, old_end): (Option<chrono::DateTime<Utc>>, chrono::DateTime<Utc>) =
        sqlx::query_as(r#"SELECT freeze_time_utc, end_time_utc FROM "Games" WHERE id = 2"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    let mut schedule_edit = pool.begin().await.unwrap();
    sqlx::query(r#"UPDATE "Games" SET freeze_time_utc = NULL WHERE id = 2"#)
        .execute(&mut *schedule_edit)
        .await
        .unwrap();
    assert_eq!(
        reschedule_game_blood_notices(&mut schedule_edit, 2, old_freeze, old_end, None, old_end,)
            .await
            .unwrap(),
        1
    );
    schedule_edit.commit().await.unwrap();
    let (released_lease, released_jobs) = claim_pending(&pool, 16).await.unwrap();
    assert_eq!(
        released_jobs
            .iter()
            .map(|job| job.notice_id)
            .collect::<Vec<_>>(),
        vec![2, 5],
        "the newly unfrozen event and an independent ordered successor may run together"
    );
    let released_frozen = released_jobs.iter().find(|job| job.notice_id == 2).unwrap();
    let ordered_third = released_jobs.iter().find(|job| job.notice_id == 5).unwrap();
    finish_job(
        &pool,
        released_frozen,
        released_lease,
        DeliveryDisposition::Dead {
            status: Some(404),
            reason: "test_terminal_dead_letter",
        },
    )
    .await
    .unwrap();
    finish_job(
        &pool,
        ordered_third,
        released_lease,
        DeliveryDisposition::Delivered { status: 200 },
    )
    .await
    .unwrap();

    sqlx::query(
        r#"UPDATE "DiscordWebhookOutbox"
              SET delivered_at_utc = clock_timestamp() - INTERVAL '8 days'
            WHERE notice_id IN (1, 6)"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"UPDATE "DiscordWebhookOutbox"
              SET dead_at_utc = clock_timestamp() - INTERVAL '8 days'
            WHERE notice_id = 2"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(purge_terminal(&pool, 2).await.unwrap(), 2);
    let expired_remaining: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)
             FROM "DiscordWebhookOutbox"
            WHERE COALESCE(delivered_at_utc, dead_at_utc)
                  < clock_timestamp() - INTERVAL '7 days'"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(expired_remaining, 1, "cleanup must obey its batch limit");
    assert_eq!(purge_terminal(&pool, 2).await.unwrap(), 1);
    let recent_terminal_retained: bool = sqlx::query_scalar(
        r#"SELECT delivered_at_utc IS NOT NULL
             FROM "DiscordWebhookOutbox" WHERE notice_id = 4"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(recent_terminal_retained);

    sqlx::query(r#"DELETE FROM "Games" WHERE id = 1"#)
        .execute(&pool)
        .await
        .unwrap();
    let remaining_for_deleted_game: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "DiscordWebhookOutbox" WHERE game_id = 1"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(remaining_for_deleted_game, 0);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
