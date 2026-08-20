//! Regression tests for the final private play-response authorization boundary.
use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

use super::*;

fn challenge_model() -> ChallengeDetailModel {
    ChallengeDetailModel {
        id: 4,
        title: "challenge".into(),
        content: "content".into(),
        category: ChallengeCategory::Misc,
        challenge_type: ChallengeType::StaticContainer,
        hints: None,
        score: 100,
        context: ClientFlagContext {
            instance_entry: Some("tcp://private.example:31337".into()),
            close_time: Some(Utc::now()),
            is_shared_instance: true,
            url: Some("/assets/hash/file".into()),
            file_size: Some(7),
            sha256: Some("hash".into()),
        },
        limit: 0,
        attempts: 0,
        deadline: None,
        user_rating: ReviewRating::None,
        user_comment: None,
    }
}

fn shared_container(container_id: Uuid, expect_stop_at: DateTime<Utc>) -> container::Model {
    container::Model {
        id: container_id,
        image: "image@sha256:test".into(),
        container_id: "runtime-container".into(),
        status: ContainerStatus::Running,
        started_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        expect_stop_at,
        is_proxy: false,
        ip: "10.0.0.4".into(),
        port: 31337,
        public_ip: Some("203.0.113.4".into()),
        public_port: Some(41337),
        game_instance_id: None,
        exercise_instance_id: None,
        ad_team_service_id: None,
    }
}

fn response_grant(container: container::Model) -> PreparedChallengeGrant {
    PreparedChallengeGrant {
        challenge: PreparedChallenge {
            id: 4,
            title: "challenge".into(),
            content: "content".into(),
            category: ChallengeCategory::Misc as i16,
            challenge_type: ChallengeType::StaticContainer as i16,
            hints: None,
            attachment_id: Some(8),
            submission_limit: 0,
            deadline_utc: None,
            enable_shared_container: true,
            workload_spec: None,
            container_image: Some("image@sha256:test".into()),
            expose_port: Some(31337),
            shared_container_id: Some(container.id),
        },
        attachment: PreparedAttachment::Observed {
            attachment: Some(attachment::Model {
                id: 8,
                file_type: FileType::Remote,
                remote_url: Some("https://old.example/secret".into()),
                local_file_id: None,
            }),
            local_file: None,
        },
        runtime: PreparedRuntime::Shared { container },
    }
}

fn prepared_response(container: &container::Model) -> ChallengeDetailModel {
    let mut model = challenge_model();
    model.context.instance_entry = Some(container.entry());
    model.context.close_time = Some(container.expect_stop_at);
    model.context.url = Some("https://old.example/secret".into());
    model.context.file_size = None;
    model.context.sha256 = None;
    model
}

async fn insert_shared_container(pool: &sqlx::PgPool, runtime: &container::Model) {
    sqlx::query(
        r#"INSERT INTO "Containers"
             (id, container_id, status, expect_stop_at, is_proxy,
              ip, port, public_ip, public_port, game_instance_id)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NULL)"#,
    )
    .bind(runtime.id)
    .bind(&runtime.container_id)
    .bind(runtime.status as i16)
    .bind(runtime.expect_stop_at)
    .bind(runtime.is_proxy)
    .bind(&runtime.ip)
    .bind(runtime.port)
    .bind(&runtime.public_ip)
    .bind(runtime.public_port)
    .execute(pool)
    .await
    .unwrap();
}

#[test]
fn archive_keeps_attachment_metadata_but_removes_runtime_coordinates() {
    let mut model = challenge_model();

    strip_live_runtime_context(&mut model);

    assert_eq!(model.context.instance_entry, None);
    assert_eq!(model.context.close_time, None);
    assert!(!model.context.is_shared_instance);
    assert_eq!(model.context.url.as_deref(), Some("/assets/hash/file"));
    assert_eq!(model.context.file_size, Some(7));
    assert_eq!(model.context.sha256.as_deref(), Some("hash"));
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn committed_policy_end_and_kick_win_the_final_response_boundary() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("play_final_policy_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    // A retained finalizer must never perform a nested pool checkout.
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TABLE "Games" (
          id INTEGER PRIMARY KEY, deletion_pending BOOLEAN NOT NULL,
          start_time_utc TIMESTAMPTZ NOT NULL, end_time_utc TIMESTAMPTZ NOT NULL,
          practice_mode BOOLEAN NOT NULL
        );
        CREATE TABLE "Teams" (
          id INTEGER PRIMARY KEY, captain_id UUID NOT NULL,
          deletion_pending BOOLEAN NOT NULL
        );
        CREATE TABLE "TeamMembers" (team_id INTEGER NOT NULL, user_id UUID NOT NULL);
        CREATE TABLE "AspNetUsers" (
          id UUID PRIMARY KEY, role SMALLINT NOT NULL,
          email_confirmed BOOLEAN NOT NULL, security_stamp TEXT
        );
        CREATE TABLE "Participations" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
          team_id INTEGER NOT NULL, status SMALLINT NOT NULL,
          division_id INTEGER
        );
        CREATE TABLE "UserParticipations" (
          user_id UUID NOT NULL, game_id INTEGER NOT NULL,
          team_id INTEGER NOT NULL, participation_id INTEGER NOT NULL
        );
        CREATE TABLE "IdentityObservations" (
          user_id UUID NOT NULL, game_id INTEGER,
          team_id INTEGER, participation_id INTEGER,
          observed_at_utc TIMESTAMPTZ NOT NULL
        );
        CREATE TABLE "Divisions" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
          default_permissions INTEGER NOT NULL
        );
        CREATE TABLE "GameChallenges" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
          title TEXT NOT NULL, content TEXT NOT NULL,
          category SMALLINT NOT NULL, "Type" SMALLINT NOT NULL,
          hints JSONB,
          is_enabled BOOLEAN NOT NULL, review_status SMALLINT NOT NULL,
          deletion_pending BOOLEAN NOT NULL,
          attachment_id INTEGER, submission_limit INTEGER NOT NULL,
          deadline_utc TIMESTAMPTZ, enable_shared_container BOOLEAN NOT NULL,
          workload_spec JSONB, container_image TEXT, expose_port INTEGER,
          shared_container_id UUID
        );
        CREATE TABLE "DivisionChallengeConfigs" (
          division_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
          permissions INTEGER NOT NULL,
          PRIMARY KEY (division_id, challenge_id)
        );
        CREATE TABLE "Attachments" (
          id INTEGER PRIMARY KEY, "Type" SMALLINT NOT NULL,
          remote_url TEXT, local_file_id INTEGER
        );
        CREATE TABLE "Files" (
          id INTEGER PRIMARY KEY, hash TEXT NOT NULL,
          file_size BIGINT NOT NULL, name TEXT NOT NULL
        );
        CREATE TABLE "Containers" (
          id UUID PRIMARY KEY, container_id TEXT NOT NULL,
          status SMALLINT NOT NULL, expect_stop_at TIMESTAMPTZ NOT NULL,
          is_proxy BOOLEAN NOT NULL, ip TEXT NOT NULL, port INTEGER NOT NULL,
          public_ip TEXT, public_port INTEGER, game_instance_id INTEGER
        );
        CREATE TABLE "GameInstances" (
          id INTEGER PRIMARY KEY, challenge_id INTEGER NOT NULL,
          participation_id INTEGER NOT NULL, is_loaded BOOLEAN NOT NULL,
          last_container_operation TIMESTAMPTZ NOT NULL,
          flag_id INTEGER, container_id UUID
        );
        CREATE TABLE "GameEvents" (
          id BIGSERIAL PRIMARY KEY, game_id INTEGER NOT NULL,
          "Type" SMALLINT NOT NULL, "values" JSONB NOT NULL,
          publish_time_utc TIMESTAMPTZ NOT NULL, user_id UUID,
          team_id INTEGER NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let user_id = Uuid::new_v4();
    let captain_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "Games" VALUES
           (1, FALSE, clock_timestamp() - interval '1 hour',
            clock_timestamp() + interval '1 hour', FALSE)"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "Teams" VALUES (2, $1, FALSE)"#)
        .bind(captain_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "TeamMembers" VALUES (2, $1)"#)
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "AspNetUsers" VALUES ($1, 1, TRUE, 'stamp')"#)
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Participations" VALUES (3, 1, 2, 1, 7)"#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "UserParticipations" VALUES ($1, 1, 2, 3)"#)
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "IdentityObservations"
             (user_id, game_id, team_id, participation_id, observed_at_utc)
           VALUES ($1, 1, 2, 3, clock_timestamp())"#,
    )
    .bind(user_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "Divisions" VALUES (7, 1, $1)"#)
        .bind(GamePermission::VIEW_CHALLENGE)
        .execute(&pool)
        .await
        .unwrap();
    let runtime_id = Uuid::new_v4();
    let runtime_stop = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
    let runtime = shared_container(runtime_id, runtime_stop);
    sqlx::query(
        r#"INSERT INTO "GameChallenges"
             (id, game_id, title, content, category, "Type", hints,
              is_enabled, review_status, deletion_pending, attachment_id,
              submission_limit, deadline_utc, enable_shared_container,
              workload_spec, container_image, expose_port, shared_container_id)
           VALUES (4, 1, 'challenge', 'content', 0, 1, NULL,
                   TRUE, 0, FALSE, 8, 0, NULL, TRUE,
                   NULL, 'image@sha256:test', 31337, $1)"#,
    )
    .bind(runtime_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "DivisionChallengeConfigs" VALUES (7, 4, $1)"#)
        .bind(GamePermission::VIEW_CHALLENGE)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "Attachments" VALUES
           (8, $1, 'https://old.example/secret', NULL)"#,
    )
    .bind(FileType::Remote as i16)
    .execute(&pool)
    .await
    .unwrap();
    insert_shared_container(&pool, &runtime).await;

    let user = CurrentUser {
        id: user_id,
        role: crate::utils::enums::Role::User,
        name: "player".into(),
        security_stamp: "stamp".into(),
    };

    let response = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        finish_challenge_response(
            &pool,
            &user,
            ChallengeResponseScope::new(1, 2, 3, 4),
            response_grant(runtime.clone()),
            prepared_response(&runtime),
        ),
    )
    .await
    .expect("one-connection finalization deadlocked")
    .unwrap();
    let live_body = axum::body::to_bytes(response.into_body(), 16_384)
        .await
        .unwrap();
    let live_json: serde_json::Value = serde_json::from_slice(&live_body).unwrap();
    assert_eq!(live_json["context"]["instanceEntry"], "203.0.113.4:41337");
    let event_count: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "GameEvents""#)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(event_count, 1);

    // A permission edit that commits while the expensive early reads are
    // being prepared must beat those stale bytes at the final boundary.
    sqlx::query(
        r#"UPDATE "DivisionChallengeConfigs" SET permissions = 0
            WHERE division_id = 7 AND challenge_id = 4"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(matches!(
        finish_challenge_response(
            &pool,
            &user,
            ChallengeResponseScope::new(1, 2, 3, 4),
            response_grant(runtime.clone()),
            prepared_response(&runtime),
        )
        .await,
        Err(AppError::NotFound(_))
    ));
    sqlx::query(
        r#"UPDATE "DivisionChallengeConfigs" SET permissions = $1
            WHERE division_id = 7 AND challenge_id = 4"#,
    )
    .bind(GamePermission::VIEW_CHALLENGE)
    .execute(&pool)
    .await
    .unwrap();

    // Private text and a raw remote attachment URL are part of the exact
    // prepared grant, not merely the challenge id. An editor replacement
    // that commits during early preparation wins and discards the stale
    // response.
    sqlx::query(r#"UPDATE "GameChallenges" SET content = 'rotated' WHERE id = 4"#)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        finish_challenge_response(
            &pool,
            &user,
            ChallengeResponseScope::new(1, 2, 3, 4),
            response_grant(runtime.clone()),
            prepared_response(&runtime),
        )
        .await,
        Err(AppError::NotFound(_))
    ));
    sqlx::query(r#"UPDATE "GameChallenges" SET content = 'content' WHERE id = 4"#)
        .execute(&pool)
        .await
        .unwrap();

    sqlx::query(
        r#"UPDATE "Attachments" SET remote_url = 'https://new.example/rotated'
            WHERE id = 8"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(matches!(
        finish_challenge_response(
            &pool,
            &user,
            ChallengeResponseScope::new(1, 2, 3, 4),
            response_grant(runtime.clone()),
            prepared_response(&runtime),
        )
        .await,
        Err(AppError::NotFound(_))
    ));
    sqlx::query(
        r#"UPDATE "Attachments" SET remote_url = 'https://old.example/secret'
            WHERE id = 8"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    // Likewise, deleting/rotating the exact shared runtime while the
    // response is prepared must never return its stale network endpoint.
    sqlx::query(r#"DELETE FROM "Containers" WHERE id = $1"#)
        .bind(runtime.id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        finish_challenge_response(
            &pool,
            &user,
            ChallengeResponseScope::new(1, 2, 3, 4),
            response_grant(runtime.clone()),
            prepared_response(&runtime),
        )
        .await,
        Err(AppError::NotFound(_))
    ));
    insert_shared_container(&pool, &runtime).await;

    // Challenge visibility is final too; a disabled/deleting/non-Active
    // challenge cannot leak through a cached player model.
    sqlx::query(r#"UPDATE "GameChallenges" SET is_enabled = FALSE WHERE id = 4"#)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        finish_challenge_response(
            &pool,
            &user,
            ChallengeResponseScope::new(1, 2, 3, 4),
            response_grant(runtime.clone()),
            prepared_response(&runtime),
        )
        .await,
        Err(AppError::NotFound(_))
    ));
    sqlx::query(r#"UPDATE "GameChallenges" SET is_enabled = TRUE WHERE id = 4"#)
        .execute(&pool)
        .await
        .unwrap();

    // A request queued on the first-open advisory must take its DB-clock
    // snapshot only after that wait. Hold the exact key across the scheduled
    // end, then release it: the response must be archival and the event absent.
    sqlx::query(r#"DELETE FROM "GameEvents""#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"UPDATE "Games" SET end_time_utc = clock_timestamp() + interval '3 seconds'
            WHERE id = 1"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let contender_options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let contender_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(contender_options)
        .await
        .unwrap();
    let event_key = "challenge-opened:1:2:4";
    let mut holder =
        crate::utils::single_flight::PgAdvisoryLock::acquire(&contender_pool, event_key)
            .await
            .unwrap();
    let holder_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut **holder.transaction_mut())
        .await
        .unwrap();
    let pending_pool = pool.clone();
    let pending_user = user.clone();
    let pending_runtime = runtime.clone();
    let pending = tokio::spawn(async move {
        finish_challenge_response(
            &pending_pool,
            &pending_user,
            ChallengeResponseScope::new(1, 2, 3, 4),
            response_grant(pending_runtime.clone()),
            prepared_response(&pending_runtime),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                r#"WITH target AS (
                       SELECT classid, objid, objsubid
                         FROM pg_locks
                        WHERE pid = $1 AND locktype = 'advisory' AND granted
                   )
                   SELECT EXISTS (
                       SELECT 1
                         FROM pg_locks waiting
                         JOIN target USING (classid, objid, objsubid)
                        WHERE waiting.locktype = 'advisory'
                          AND waiting.pid <> $1
                          AND NOT waiting.granted
                   )"#,
            )
            .bind(holder_pid)
            .fetch_one(&admin)
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("challenge response never waited on the held event advisory");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let ended: bool = sqlx::query_scalar(
                r#"SELECT clock_timestamp() >= end_time_utc FROM "Games" WHERE id = 1"#,
            )
            .fetch_one(&mut **holder.transaction_mut())
            .await
            .unwrap();
            if ended {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("database clock did not reach the configured game end");
    holder.release().await.unwrap();
    let archived = tokio::time::timeout(std::time::Duration::from_secs(2), pending)
        .await
        .expect("challenge response stayed blocked after advisory release")
        .unwrap()
        .unwrap();
    let archived_body = axum::body::to_bytes(archived.into_body(), 16_384)
        .await
        .unwrap();
    let archived_json: serde_json::Value = serde_json::from_slice(&archived_body).unwrap();
    assert!(archived_json["context"]["instanceEntry"].is_null());
    assert!(archived_json["context"]["closeTime"].is_null());
    assert_eq!(archived_json["context"]["isSharedInstance"], false);
    assert_eq!(
        archived_json["context"]["url"],
        "https://old.example/secret"
    );
    let event_count: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "GameEvents""#)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(event_count, 0);
    contender_pool.close().await;

    // Retained UserParticipations is historical evidence, never current
    // authority: a kick that commits before finalization discards the
    // prepared private model even after the game is reopened.
    sqlx::query(
        r#"UPDATE "Games" SET end_time_utc = clock_timestamp() + interval '1 hour'
            WHERE id = 1"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"DELETE FROM "TeamMembers" WHERE team_id = 2 AND user_id = $1"#)
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        finish_challenge_response(
            &pool,
            &user,
            ChallengeResponseScope::new(1, 2, 3, 4),
            response_grant(runtime.clone()),
            prepared_response(&runtime),
        )
        .await,
        Err(AppError::Forbidden)
    ));
    let historical_count: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "UserParticipations""#)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(historical_count, 1);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
