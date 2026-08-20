use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

use super::authorization::{
    finalize_grant_for_test, finalize_monitor_grant_for_test, finalize_public_grant_for_test,
    participant_can_download_target, participant_grant_for_test, query_asset_gate, AssetGate,
    AssetTarget,
};
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::utils::enums::{GamePermission, Role};

const TEST_SECURITY_STAMP: &str = "asset-authorization-test-stamp";

fn current_user(id: Uuid) -> CurrentUser {
    CurrentUser {
        id,
        role: Role::User,
        name: "asset-test-user".to_string(),
        security_stamp: TEST_SECURITY_STAMP.to_string(),
    }
}

struct AssetAuthorizationHarness {
    admin: sqlx::PgPool,
    pool: sqlx::PgPool,
    schema: String,
}

impl AssetAuthorizationHarness {
    async fn new() -> Self {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_asset_auth_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY,
              hidden BOOLEAN NOT NULL,
              poster_hash TEXT,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE,
              start_time_utc TIMESTAMPTZ NOT NULL DEFAULT
                  (CURRENT_TIMESTAMP + interval '1 hour'),
              end_time_utc TIMESTAMPTZ NOT NULL DEFAULT
                  (CURRENT_TIMESTAMP + interval '2 hours')
            );
            CREATE TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              title TEXT NOT NULL DEFAULT '',
              is_enabled BOOLEAN NOT NULL,
              review_status SMALLINT NOT NULL,
              attachment_id INTEGER,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "Participations" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL,
              status SMALLINT NOT NULL,
              writeup_id INTEGER,
              division_id INTEGER
            );
            CREATE TABLE "UserParticipations" (
              user_id UUID NOT NULL,
              game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL,
              PRIMARY KEY (user_id, game_id)
            );
            CREATE TABLE "AspNetUsers" (
              id UUID PRIMARY KEY,
              avatar_hash TEXT,
              role SMALLINT NOT NULL DEFAULT 1,
              email_confirmed BOOLEAN NOT NULL DEFAULT TRUE,
              security_stamp TEXT NOT NULL DEFAULT 'asset-authorization-test-stamp'
            );
            CREATE TABLE "Teams" (
              id INTEGER PRIMARY KEY,
              avatar_hash TEXT,
              captain_id UUID,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "TeamMembers" (
              team_id INTEGER NOT NULL,
              user_id UUID NOT NULL,
              PRIMARY KEY (team_id, user_id)
            );
            CREATE TABLE "Divisions" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              default_permissions INTEGER NOT NULL
            );
            CREATE TABLE "DivisionChallengeConfigs" (
              division_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL,
              permissions INTEGER NOT NULL,
              PRIMARY KEY (division_id, challenge_id)
            );
            CREATE TABLE "Configs" (
              config_key TEXT PRIMARY KEY,
              value TEXT
            );
            CREATE TABLE "Files" (
              id INTEGER PRIMARY KEY,
              hash TEXT UNIQUE NOT NULL,
              file_size BIGINT NOT NULL,
              reference_count BIGINT NOT NULL
            );
            CREATE TABLE "Attachments" (
              id INTEGER PRIMARY KEY,
              local_file_id INTEGER
            );
            CREATE TABLE "FlagContexts" (
              id INTEGER PRIMARY KEY,
              attachment_id INTEGER
            );
            CREATE TABLE "GameInstances" (
              id INTEGER PRIMARY KEY,
              challenge_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL,
              flag_id INTEGER
            );
            CREATE TABLE "GameEvents" (
              id SERIAL PRIMARY KEY,
              game_id INTEGER NOT NULL,
              "Type" SMALLINT NOT NULL,
              "values" JSONB NOT NULL,
              publish_time_utc TIMESTAMPTZ NOT NULL,
              user_id UUID,
              team_id INTEGER NOT NULL
            );
            CREATE TABLE "IdentityObservations" (
              user_id UUID NOT NULL, game_id INTEGER,
              team_id INTEGER, participation_id INTEGER,
              observed_at_utc TIMESTAMPTZ NOT NULL
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        Self {
            admin,
            pool,
            schema,
        }
    }

    async fn cleanup(self) {
        self.pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn asset_gate_query_preserves_public_static_team_and_private_boundaries() {
    let harness = AssetAuthorizationHarness::new().await;
    let public_hash = "a".repeat(64);
    let static_hash = "b".repeat(64);
    let writeup_hash = "c".repeat(64);
    let orphan_hash = "d".repeat(64);

    sqlx::query(r#"INSERT INTO "AspNetUsers" (id, avatar_hash) VALUES ($1, $2)"#)
        .bind(Uuid::new_v4())
        .bind(&public_hash)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "Files" (id, hash, file_size, reference_count) VALUES
          (1, $1, 1048576, 1),
          (2, $2, 2048, 1),
          (3, $3, 4096, 1)"#,
    )
    .bind(&static_hash)
    .bind(&writeup_hash)
    .bind(&orphan_hash)
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::raw_sql(
        r#"
        INSERT INTO "Attachments" (id, local_file_id) VALUES (10, 1);
        INSERT INTO "Games" (id, hidden) VALUES (20, TRUE), (21, TRUE);
        INSERT INTO "GameChallenges"
          (id, game_id, is_enabled, review_status, attachment_id)
        VALUES (30, 20, TRUE, 0, 10);
        INSERT INTO "Participations"
          (id, game_id, team_id, status, writeup_id)
        VALUES (40, 21, 50, 1, 2);
        "#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();

    assert_eq!(
        query_asset_gate(&harness.pool, &public_hash).await.unwrap(),
        AssetGate::Public { file_size: None }
    );
    assert_eq!(
        query_asset_gate(&harness.pool, &static_hash).await.unwrap(),
        AssetGate::Protected {
            file_size: Some(1_048_576),
            targets: vec![AssetTarget {
                game_id: 20,
                source_team: None,
                challenge_id: Some(30),
            }],
        }
    );
    assert_eq!(
        query_asset_gate(&harness.pool, &writeup_hash)
            .await
            .unwrap(),
        AssetGate::Protected {
            file_size: Some(2048),
            targets: vec![AssetTarget {
                game_id: 21,
                source_team: Some(50),
                challenge_id: None,
            }],
        }
    );
    assert_eq!(
        query_asset_gate(&harness.pool, &orphan_hash).await.unwrap(),
        AssetGate::Private {
            file_size: Some(4096),
        }
    );

    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn delayed_public_and_monitor_grants_are_revalidated_before_response() {
    let harness = AssetAuthorizationHarness::new().await;
    let public_owner = Uuid::new_v4();
    let public_hash = "9".repeat(64);
    sqlx::query(
        r#"INSERT INTO "AspNetUsers" (id, avatar_hash, role)
           VALUES ($1, $2, 1)"#,
    )
    .bind(public_owner)
    .bind(&public_hash)
    .execute(&harness.pool)
    .await
    .unwrap();
    // The same content also backs a sensitive writeup. Once the sole public
    // field is removed, that protected relation must not inherit cached public
    // delivery from the request's early gate.
    sqlx::query(
        r#"INSERT INTO "Files" (id, hash, file_size, reference_count)
           VALUES (70, $1, 16, 1)"#,
    )
    .bind(&public_hash)
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::raw_sql(
        r#"
        INSERT INTO "Games" (id, hidden) VALUES (300, FALSE);
        INSERT INTO "Participations" (id, game_id, team_id, status, writeup_id)
        VALUES (30, 300, 30, 1, 70);
        "#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();
    assert_eq!(
        query_asset_gate(&harness.pool, &public_hash).await.unwrap(),
        AssetGate::Public {
            file_size: Some(16)
        }
    );

    let (storage_started_tx, storage_started_rx) = tokio::sync::oneshot::channel();
    let (storage_done_tx, storage_done_rx) = tokio::sync::oneshot::channel();
    let delayed_public = tokio::spawn({
        let pool = harness.pool.clone();
        let public_hash = public_hash.clone();
        async move {
            storage_started_tx.send(()).unwrap();
            storage_done_rx.await.unwrap();
            finalize_public_grant_for_test(&pool, &public_hash).await
        }
    });
    storage_started_rx.await.unwrap();
    sqlx::query(r#"UPDATE "AspNetUsers" SET avatar_hash = NULL WHERE id = $1"#)
        .bind(public_owner)
        .execute(&harness.pool)
        .await
        .unwrap();
    storage_done_tx.send(()).unwrap();
    let public_error = delayed_public
        .await
        .unwrap()
        .expect_err("detached public relation survived final authorization");
    assert_eq!(public_error.status(), axum::http::StatusCode::FORBIDDEN);

    sqlx::query(r#"UPDATE "AspNetUsers" SET avatar_hash = $2 WHERE id = $1"#)
        .bind(public_owner)
        .bind(&public_hash)
        .execute(&harness.pool)
        .await
        .unwrap();
    finalize_public_grant_for_test(&harness.pool, &public_hash)
        .await
        .unwrap();

    let monitor_id = Uuid::new_v4();
    let monitor = CurrentUser {
        id: monitor_id,
        role: Role::Monitor,
        name: "monitor".to_string(),
        security_stamp: TEST_SECURITY_STAMP.to_string(),
    };
    sqlx::query(r#"INSERT INTO "AspNetUsers" (id, role) VALUES ($1, $2)"#)
        .bind(monitor_id)
        .bind(Role::Monitor as i16)
        .execute(&harness.pool)
        .await
        .unwrap();
    finalize_monitor_grant_for_test(&harness.pool, &monitor)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "AspNetUsers" SET security_stamp = 'rotated' WHERE id = $1"#)
        .bind(monitor_id)
        .execute(&harness.pool)
        .await
        .unwrap();
    let monitor_error = finalize_monitor_grant_for_test(&harness.pool, &monitor)
        .await
        .expect_err("rotated monitor session survived final authorization");
    assert_eq!(monitor_error.status(), axum::http::StatusCode::FORBIDDEN);
    sqlx::query(r#"UPDATE "AspNetUsers" SET security_stamp = $2, role = $3 WHERE id = $1"#)
        .bind(monitor_id)
        .bind(TEST_SECURITY_STAMP)
        .bind(Role::User as i16)
        .execute(&harness.pool)
        .await
        .unwrap();
    assert_eq!(
        finalize_monitor_grant_for_test(&harness.pool, &monitor)
            .await
            .expect_err("demoted monitor survived final authorization")
            .status(),
        axum::http::StatusCode::FORBIDDEN
    );

    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn accepted_participant_can_download_hidden_game_attachment() {
    let harness = AssetAuthorizationHarness::new().await;
    let user_id = Uuid::new_v4();
    let user = current_user(user_id);
    let other_captain = Uuid::new_v4();
    sqlx::raw_sql(
        r#"
        INSERT INTO "Games" (id, hidden) VALUES (205, TRUE);
        INSERT INTO "GameChallenges" (id, game_id, title, is_enabled, review_status)
        VALUES (997, 205, 'Hidden challenge', TRUE, 0);
        INSERT INTO "Participations" (id, game_id, team_id, status)
        VALUES (17, 205, 9, 1);
        "#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "AspNetUsers" (id, role) VALUES ($1, 1), ($2, 1)"#)
        .bind(user_id)
        .bind(other_captain)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Teams" (id, captain_id) VALUES (9, $1)"#)
        .bind(other_captain)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES (9, $1)"#)
        .bind(user_id)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "UserParticipations"
             (user_id, game_id, team_id, participation_id)
           VALUES ($1, 205, 9, 17)"#,
    )
    .bind(user_id)
    .execute(&harness.pool)
    .await
    .unwrap();

    let target = AssetTarget {
        game_id: 205,
        source_team: None,
        challenge_id: Some(997),
    };
    assert!(
        participant_can_download_target(&harness.pool, &user, &target)
            .await
            .unwrap(),
        "hidden-game participation was incorrectly treated as public discovery"
    );

    // Kicks deliberately retain UserParticipations for evidence. That retained
    // row must not remain an attachment credential.
    sqlx::query(r#"DELETE FROM "TeamMembers" WHERE team_id = 9 AND user_id = $1"#)
        .bind(user_id)
        .execute(&harness.pool)
        .await
        .unwrap();
    assert!(
        !participant_can_download_target(&harness.pool, &user, &target)
            .await
            .unwrap(),
        "retained participation history authorized a kicked member"
    );

    // A captain is canonically part of the live roster even without a
    // duplicate TeamMembers row.
    sqlx::query(r#"UPDATE "Teams" SET captain_id = $1 WHERE id = 9"#)
        .bind(user_id)
        .execute(&harness.pool)
        .await
        .unwrap();
    assert!(
        participant_can_download_target(&harness.pool, &user, &target)
            .await
            .unwrap(),
        "current captain was not recognized as a live roster member"
    );

    sqlx::query(r#"UPDATE "AspNetUsers" SET role = 0 WHERE id = $1"#)
        .bind(user_id)
        .execute(&harness.pool)
        .await
        .unwrap();
    assert!(
        !participant_can_download_target(&harness.pool, &user, &target)
            .await
            .unwrap(),
        "banned captain retained protected attachment access"
    );
    sqlx::query(r#"UPDATE "AspNetUsers" SET role = 1 WHERE id = $1"#)
        .bind(user_id)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "Teams" SET deletion_pending = TRUE WHERE id = 9"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    assert!(
        !participant_can_download_target(&harness.pool, &user, &target)
            .await
            .unwrap(),
        "deleting team retained protected attachment access"
    );

    sqlx::query(r#"UPDATE "Teams" SET deletion_pending = FALSE WHERE id = 9"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "Participations" SET status = 2 WHERE id = 17"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    assert!(
        !participant_can_download_target(&harness.pool, &user, &target)
            .await
            .unwrap(),
        "rejected participation still authorized the private attachment"
    );

    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn effective_division_permission_hides_challenge_and_writeup_assets() {
    let harness = AssetAuthorizationHarness::new().await;
    let user_id = Uuid::new_v4();
    let user = current_user(user_id);
    let captain_id = Uuid::new_v4();

    sqlx::raw_sql(
        r#"
        INSERT INTO "Games" (id, hidden) VALUES (206, FALSE);
        INSERT INTO "GameChallenges" (id, game_id, title, is_enabled, review_status)
        VALUES (998, 206, 'Division challenge', TRUE, 0);
        INSERT INTO "Divisions" (id, game_id, default_permissions)
        VALUES (77, 206, 0);
        INSERT INTO "DivisionChallengeConfigs" (division_id, challenge_id, permissions)
        VALUES (77, 998, 0);
        INSERT INTO "Participations" (id, game_id, team_id, status, division_id)
        VALUES (18, 206, 10, 1, 77);
        "#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "AspNetUsers" (id, role) VALUES ($1, 1), ($2, 1)"#)
        .bind(user_id)
        .bind(captain_id)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Teams" (id, captain_id) VALUES (10, $1)"#)
        .bind(captain_id)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES (10, $1)"#)
        .bind(user_id)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "UserParticipations"
               (user_id, game_id, team_id, participation_id)
           VALUES ($1, 206, 10, 18)"#,
    )
    .bind(user_id)
    .execute(&harness.pool)
    .await
    .unwrap();

    let challenge = AssetTarget {
        game_id: 206,
        source_team: None,
        challenge_id: Some(998),
    };
    let writeup = AssetTarget {
        game_id: 206,
        source_team: Some(10),
        challenge_id: None,
    };
    assert!(
        !participant_can_download_target(&harness.pool, &user, &challenge)
            .await
            .unwrap(),
        "challenge-specific VIEW_CHALLENGE denial was ignored"
    );
    assert!(
        !participant_can_download_target(&harness.pool, &user, &writeup)
            .await
            .unwrap(),
        "division-default VIEW_CHALLENGE denial was ignored for a writeup"
    );

    sqlx::query(
        r#"UPDATE "DivisionChallengeConfigs" SET permissions = $1
            WHERE division_id = 77 AND challenge_id = 998"#,
    )
    .bind(GamePermission::VIEW_CHALLENGE)
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(r#"UPDATE "Divisions" SET default_permissions = $1 WHERE id = 77"#)
        .bind(GamePermission::VIEW_CHALLENGE)
        .execute(&harness.pool)
        .await
        .unwrap();
    assert!(
        participant_can_download_target(&harness.pool, &user, &challenge)
            .await
            .unwrap()
    );
    assert!(
        participant_can_download_target(&harness.pool, &user, &writeup)
            .await
            .unwrap()
    );

    // Match `game::effective_permission`: a dangling/cross-game division must
    // fail closed even if its stale challenge override still grants access.
    sqlx::query(r#"UPDATE "Divisions" SET game_id = 999 WHERE id = 77"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    assert!(
        !participant_can_download_target(&harness.pool, &user, &challenge)
            .await
            .unwrap(),
        "cross-game division override authorized a challenge attachment"
    );
    assert!(
        !participant_can_download_target(&harness.pool, &user, &writeup)
            .await
            .unwrap(),
        "cross-game division default authorized a writeup"
    );

    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn download_event_uses_only_the_exact_authorized_hash_target() {
    let harness = AssetAuthorizationHarness::new().await;
    let user_id = Uuid::new_v4();
    let user = current_user(user_id);
    let captain_id = Uuid::new_v4();
    let shared_hash = "e".repeat(64);
    sqlx::query(
        r#"INSERT INTO "Files" (id, hash, file_size, reference_count)
           VALUES (50, $1, 123, 2)"#,
    )
    .bind(&shared_hash)
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "AspNetUsers" (id, role) VALUES ($1, 1), ($2, 1)"#)
        .bind(user_id)
        .bind(captain_id)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Teams" (id, captain_id) VALUES (11, $1)"#)
        .bind(captain_id)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES (11, $1)"#)
        .bind(user_id)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        INSERT INTO "Attachments" (id, local_file_id) VALUES (51, 50);
        INSERT INTO "Games" (id, hidden) VALUES (207, FALSE);
        INSERT INTO "GameChallenges"
            (id, game_id, title, is_enabled, review_status, attachment_id)
        VALUES
            (999, 207, 'Authorized target', TRUE, 0, 51),
            (1000, 207, 'Same hash, different challenge', TRUE, 0, 51);
        INSERT INTO "Participations" (id, game_id, team_id, status)
        VALUES (19, 207, 11, 1);
        "#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "UserParticipations"
               (user_id, game_id, team_id, participation_id)
           VALUES ($1, 207, 11, 19)"#,
    )
    .bind(user_id)
    .execute(&harness.pool)
    .await
    .unwrap();

    let target = AssetTarget {
        game_id: 207,
        source_team: None,
        challenge_id: Some(999),
    };
    let grant = participant_grant_for_test(&harness.pool, &user, &target, &shared_hash)
        .await
        .unwrap()
        .expect("initial exact target should authorize");

    // Model an editor replacing/detaching the attachment while object storage
    // is preparing the old blob. The final exact relationship proof must win.
    let (storage_started_tx, storage_started_rx) = tokio::sync::oneshot::channel();
    let (storage_done_tx, storage_done_rx) = tokio::sync::oneshot::channel();
    let detached_finalize = tokio::spawn({
        let pool = harness.pool.clone();
        let grant = grant.clone();
        async move {
            storage_started_tx.send(()).unwrap();
            storage_done_rx.await.unwrap();
            finalize_grant_for_test(&pool, &grant, Some("detached-token"), true).await
        }
    });
    storage_started_rx.await.unwrap();
    sqlx::query(r#"UPDATE "GameChallenges" SET attachment_id = NULL WHERE id = 999"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    storage_done_tx.send(()).unwrap();
    let detached_error = detached_finalize
        .await
        .unwrap()
        .expect_err("detached blob survived final target reauthorization");
    assert_eq!(detached_error.status(), axum::http::StatusCode::FORBIDDEN);
    let detached_events: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "GameEvents""#)
        .fetch_one(&harness.pool)
        .await
        .unwrap();
    assert_eq!(detached_events, 0);

    sqlx::query(r#"UPDATE "GameChallenges" SET attachment_id = 51 WHERE id = 999"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "Files" SET reference_count = 0 WHERE id = 50"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    let unreferenced_error =
        finalize_grant_for_test(&harness.pool, &grant, Some("unreferenced-token"), true)
            .await
            .expect_err("zero-reference blob survived final target reauthorization");
    assert_eq!(
        unreferenced_error.status(),
        axum::http::StatusCode::FORBIDDEN
    );
    sqlx::query(r#"UPDATE "Files" SET reference_count = 2 WHERE id = 50"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    finalize_grant_for_test(&harness.pool, &grant, Some("secure-token"), true)
        .await
        .unwrap();
    // The exact event is idempotent and never expands to challenge 1000 merely
    // because both challenge rows share one content hash.
    finalize_grant_for_test(&harness.pool, &grant, Some("secure-token"), true)
        .await
        .unwrap();

    let events = sqlx::query_as::<_, (i32, i32, serde_json::Value, Option<Uuid>)>(
        r#"SELECT game_id, team_id, "values", user_id
             FROM "GameEvents"
            ORDER BY id"#,
    )
    .fetch_all(&harness.pool)
    .await
    .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, 207);
    assert_eq!(events[0].1, 11);
    assert_eq!(
        events[0].2,
        serde_json::json!(["999", "Authorized target", "secure-token"])
    );
    assert_eq!(events[0].3, Some(user_id));

    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn delayed_body_preparation_reauth_rejects_a_concurrent_kick() {
    let harness = AssetAuthorizationHarness::new().await;
    let user_id = Uuid::new_v4();
    let user = current_user(user_id);
    let captain_id = Uuid::new_v4();

    sqlx::raw_sql(
        r#"
        INSERT INTO "Games" (id, hidden) VALUES (208, FALSE);
        INSERT INTO "GameChallenges"
            (id, game_id, title, is_enabled, review_status)
        VALUES (1001, 208, 'Delayed body', TRUE, 0);
        INSERT INTO "Participations" (id, game_id, team_id, status)
        VALUES (20, 208, 12, 1);
        "#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "AspNetUsers" (id, role) VALUES ($1, 1), ($2, 1)"#)
        .bind(user_id)
        .bind(captain_id)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Teams" (id, captain_id) VALUES (12, $1)"#)
        .bind(captain_id)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES (12, $1)"#)
        .bind(user_id)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "UserParticipations"
               (user_id, game_id, team_id, participation_id)
           VALUES ($1, 208, 12, 20)"#,
    )
    .bind(user_id)
    .execute(&harness.pool)
    .await
    .unwrap();

    let target = AssetTarget {
        game_id: 208,
        source_team: None,
        challenge_id: Some(1001),
    };
    let delayed_hash = "f".repeat(64);
    let grant = participant_grant_for_test(&harness.pool, &user, &target, &delayed_hash)
        .await
        .unwrap()
        .expect("initial authorization should precede body preparation");

    // Model a slow object-store read between authorization phases. The kick
    // wins the canonical exclusive roster fence during that delay.
    let (storage_started_tx, storage_started_rx) = tokio::sync::oneshot::channel();
    let (storage_done_tx, storage_done_rx) = tokio::sync::oneshot::channel();
    let delayed_finalize = tokio::spawn({
        let pool = harness.pool.clone();
        let grant = grant.clone();
        async move {
            storage_started_tx.send(()).unwrap();
            storage_done_rx.await.unwrap();
            finalize_grant_for_test(&pool, &grant, Some("late-token"), true).await
        }
    });
    storage_started_rx.await.unwrap();
    let roster_key = crate::services::live_roster::lock_key(12);
    let mut kick = crate::utils::single_flight::PgAdvisoryLock::acquire(&harness.pool, &roster_key)
        .await
        .unwrap();
    sqlx::query(r#"DELETE FROM "TeamMembers" WHERE team_id = 12 AND user_id = $1"#)
        .bind(user_id)
        .execute(&mut **kick.transaction_mut())
        .await
        .unwrap();
    kick.release().await.unwrap();
    storage_done_tx.send(()).unwrap();

    let error = delayed_finalize
        .await
        .unwrap()
        .expect_err("final authorization ignored a kick during storage delay");
    assert_eq!(error.status(), axum::http::StatusCode::FORBIDDEN);
    let event_count: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "GameEvents""#)
        .fetch_one(&harness.pool)
        .await
        .unwrap();
    assert_eq!(event_count, 0, "revoked download produced cheat evidence");

    harness.cleanup().await;
}
