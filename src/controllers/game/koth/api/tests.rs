use super::*;
use axum::http::HeaderValue;
use sqlx::{Connection, PgConnection};

fn signed_headers(
    secret: &str,
    timestamp: &str,
    game_id: i32,
    challenge_id: i32,
    body: &[u8],
) -> HeaderMap {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(game_id.to_string().as_bytes());
    mac.update(b".");
    mac.update(challenge_id.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    let mut headers = HeaderMap::new();
    headers.insert(TIMESTAMP_HEADER, HeaderValue::from_static("123"));
    headers.insert(
        SIGNATURE_HEADER,
        HeaderValue::from_str(&format!(
            "{SIGNATURE_PREFIX}{}",
            hex::encode(mac.finalize().into_bytes())
        ))
        .unwrap(),
    );
    headers
}

#[test]
fn signature_binds_timestamp_scope_and_exact_body() {
    let body = br#"{"context":"abc","teams":[]}"#;
    let headers = signed_headers("secret", "123", 7, 9, body);
    let signature = parse_signature(&headers).unwrap();
    assert!(verify_signature("secret", "123", 7, 9, body, &signature).is_ok());
    assert!(verify_signature("secret", "124", 7, 9, body, &signature).is_err());
    assert!(verify_signature("secret", "123", 8, 9, body, &signature).is_err());
    assert!(verify_signature("secret", "123", 7, 10, body, &signature).is_err());
    assert!(verify_signature("secret", "123", 7, 9, b"{}", &signature).is_err());
}

#[test]
fn context_changes_for_every_runtime_and_scoring_window() {
    let context = |game_id,
                   challenge_id,
                   target_id,
                   cycle_id,
                   reset_attempt,
                   reporting_revision,
                   container_id,
                   round_id,
                   objective_schema_hash,
                   eligible_tokens| {
        opaque_context(OpaqueContext {
            game_id,
            challenge_id,
            target_id,
            cycle_id,
            reset_attempt,
            reporting_revision,
            container_id,
            round_id,
            objective_schema_hash,
            eligible_tokens,
        })
    };
    let tokens = vec!["token-a".to_string(), "token-b".to_string()];
    let base = context(7, 9, 3, 41, 1, 5, "container-a", 51, None, &tokens);
    assert_eq!(base.len(), 64);
    assert_ne!(
        base,
        context(8, 9, 3, 41, 1, 5, "container-a", 51, None, &tokens)
    );
    assert_ne!(
        base,
        context(7, 9, 4, 41, 1, 5, "container-a", 51, None, &tokens)
    );
    assert_ne!(
        base,
        context(7, 9, 3, 42, 1, 5, "container-a", 51, None, &tokens)
    );
    assert_ne!(
        base,
        context(7, 9, 3, 41, 2, 5, "container-a", 51, None, &tokens)
    );
    assert_ne!(
        base,
        context(7, 9, 3, 41, 1, 6, "container-a", 51, None, &tokens)
    );
    assert_ne!(
        base,
        context(7, 9, 3, 41, 1, 5, "container-b", 51, None, &tokens)
    );
    assert_ne!(
        base,
        context(7, 9, 3, 41, 1, 5, "container-a", 52, None, &tokens)
    );
    assert_ne!(
        base,
        context(
            7,
            9,
            3,
            41,
            1,
            5,
            "container-a",
            51,
            Some(&[1; 32]),
            &tokens
        )
    );
    assert_ne!(
        base,
        context(
            7,
            9,
            3,
            41,
            1,
            5,
            "container-a",
            51,
            None,
            &["token-a".to_string(), "rotated-token".to_string()]
        )
    );
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn observation_rebase_removes_every_ineligible_identity_and_repairs_crowns() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let mut connection = PgConnection::connect(&database_url).await.unwrap();
    sqlx::raw_sql(
        r#"
            CREATE TEMP TABLE "Participations" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL, status SMALLINT NOT NULL
            );
            CREATE TEMP TABLE "Teams" (
              id INTEGER PRIMARY KEY, captain_id INTEGER NOT NULL,
              deletion_pending BOOLEAN NOT NULL
            );
            CREATE TEMP TABLE "TeamMembers" (team_id INTEGER, user_id INTEGER);
            CREATE TEMP TABLE "AspNetUsers" (id INTEGER PRIMARY KEY, role SMALLINT NOT NULL);
            CREATE TEMP TABLE "KothOfficialConfigs" (
              game_id INTEGER PRIMARY KEY, roster_snapshot JSONB NOT NULL
            );
            CREATE TEMP TABLE "KothApiTeamTokens" (
              game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL, token TEXT NOT NULL UNIQUE,
              generation INTEGER NOT NULL DEFAULT 1,
              rotated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
              last_used_at TIMESTAMPTZ,
              revocation_pending BOOLEAN NOT NULL DEFAULT FALSE,
              PRIMARY KEY (game_id, challenge_id, participation_id)
            );
            CREATE TEMP TABLE "KothApiSnapshots" (
              target_id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL, snapshot_hash BYTEA NOT NULL
            );
            CREATE TEMP TABLE "KothApiSnapshotScores" (
              target_id INTEGER NOT NULL, wave_id TEXT NOT NULL,
              participation_id INTEGER NOT NULL,
              activity_earned BIGINT NOT NULL,
              activity_possible BIGINT NOT NULL,
              objective_earned BIGINT NOT NULL,
              objective_possible BIGINT NOT NULL,
              objective_count SMALLINT NOT NULL,
              is_crown BOOLEAN NOT NULL,
              PRIMARY KEY (target_id, wave_id, participation_id)
            );
            CREATE UNIQUE INDEX uq_test_koth_api_crown
              ON "KothApiSnapshotScores" (target_id, wave_id)
              WHERE is_crown;
            INSERT INTO "KothOfficialConfigs" VALUES
              (7, '[11,12,13,14,15]');
            INSERT INTO "Participations" VALUES
              (11, 7, 21, 1), (12, 7, 22, 3), (13, 7, 23, 1),
              (14, 7, 24, 1), (15, 7, 25, 1);
            INSERT INTO "Teams" VALUES
              (21, 101, FALSE), (22, 102, FALSE), (23, 103, TRUE),
              (24, 104, FALSE), (25, 105, FALSE);
            INSERT INTO "AspNetUsers" VALUES
              (101, 1), (102, 1), (103, 1), (104, 1), (105, 1),
              (204, 0);
            INSERT INTO "TeamMembers" VALUES (24, 204), (25, 205);
            INSERT INTO "KothApiTeamTokens"
              (game_id, challenge_id, participation_id, token) VALUES
              (7, 9, 11, 'koth_eligible_team'),
              (7, 9, 12, 'koth_suspended_team'),
              (7, 9, 13, 'koth_deleting_team'),
              (7, 9, 14, 'koth_banned_team'),
              (7, 9, 15, 'koth_missing_account');
            INSERT INTO "KothApiSnapshots" VALUES
              (3, 7, 9, decode(repeat('11', 32), 'hex'));
            INSERT INTO "KothApiSnapshotScores" VALUES
              (3, 'status', 11, 1, 1, 1, 2, 1, FALSE),
              (3, 'status', 12, 1, 1, 3, 4, 1, TRUE),
              (3, 'deletion', 11, 1, 1, 1, 2, 1, FALSE),
              (3, 'deletion', 13, 1, 1, 3, 4, 1, TRUE),
              (3, 'banned', 11, 1, 1, 1, 2, 1, FALSE),
              (3, 'banned', 14, 1, 1, 3, 4, 1, TRUE),
              (3, 'missing-account', 11, 1, 1, 1, 2, 1, FALSE),
              (3, 'missing-account', 15, 1, 1, 3, 4, 1, TRUE);
            "#,
    )
    .execute(&mut connection)
    .await
    .unwrap();

    let before: Vec<u8> =
        sqlx::query_scalar(r#"SELECT snapshot_hash FROM "KothApiSnapshots" WHERE target_id = 3"#)
            .fetch_one(&mut connection)
            .await
            .unwrap();
    let mut transaction = connection.begin().await.unwrap();
    let eligible = load_eligible_capabilities(&mut *transaction, 7, 9)
        .await
        .unwrap();
    assert_eq!(
        eligible
            .iter()
            .map(|capability| capability.participation_id)
            .collect::<Vec<_>>(),
        [11]
    );
    assert_eq!(
        crate::services::ad::koth_api_capability::retain_eligible_unsettled_scores(
            &mut transaction,
            7,
            9,
            3,
            &[11],
        )
        .await
        .unwrap(),
        4
    );
    let rows: Vec<(String, i32, bool)> = sqlx::query_as(
        r#"SELECT wave_id, participation_id, is_crown
                 FROM "KothApiSnapshotScores"
                ORDER BY wave_id, participation_id"#,
    )
    .fetch_all(&mut *transaction)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            ("banned".to_string(), 11, true),
            ("deletion".to_string(), 11, true),
            ("missing-account".to_string(), 11, true),
            ("status".to_string(), 11, true),
        ]
    );
    let after: Vec<u8> =
        sqlx::query_scalar(r#"SELECT snapshot_hash FROM "KothApiSnapshots" WHERE target_id = 3"#)
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
    assert_ne!(after, before);

    let rotated_challenges =
        crate::services::ad::koth_api_capability::force_rotate_event_capabilities(
            &mut transaction,
            7,
            &[12, 14, 15],
        )
        .await
        .unwrap();
    assert_eq!(rotated_challenges.into_iter().collect::<Vec<_>>(), [9]);
    sqlx::raw_sql(
        r#"UPDATE "Participations" SET status = 1 WHERE id = 12;
               UPDATE "AspNetUsers" SET role = 1 WHERE id = 204;
               INSERT INTO "AspNetUsers" VALUES (205, 1);"#,
    )
    .execute(&mut *transaction)
    .await
    .unwrap();
    let restored = load_eligible_capabilities(&mut *transaction, 7, 9)
        .await
        .unwrap();
    assert_eq!(
        restored
            .iter()
            .map(|capability| capability.participation_id)
            .collect::<Vec<_>>(),
        [11, 12, 14, 15]
    );
    let restored_state: (i64, i64, bool) = sqlx::query_as(
        r#"SELECT COUNT(*),
                      COUNT(*) FILTER (WHERE generation = 2),
                      NOT EXISTS (
                        SELECT 1 FROM "KothApiTeamTokens"
                         WHERE token IN (
                           'koth_suspended_team',
                           'koth_banned_team',
                           'koth_missing_account'
                         )
                      )
                 FROM "KothApiTeamTokens"
                WHERE participation_id = ANY($1)"#,
    )
    .bind([11, 12, 14, 15].as_slice())
    .fetch_one(&mut *transaction)
    .await
    .unwrap();
    assert_eq!(restored_state, (4, 3, true));
    transaction.commit().await.unwrap();
}

#[tokio::test]
async fn observer_context_etag_is_stable_and_accepts_weak_lists_and_star() {
    let body = bytes::Bytes::from_static(br#"{"context":"stable"}"#);
    let context = fresh_observer_context(body);
    let first = context_response(context.clone(), &HeaderMap::new()).unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let etag = first.headers()[header::ETAG].clone();
    assert_eq!(
        first.headers()[header::CACHE_CONTROL],
        "public, max-age=0, must-revalidate"
    );

    let mut conditional = HeaderMap::new();
    conditional.insert(
        header::IF_NONE_MATCH,
        HeaderValue::from_str(&format!("\"unrelated\", W/{}", etag.to_str().unwrap())).unwrap(),
    );
    let unchanged = context_response(context.clone(), &conditional).unwrap();
    assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(unchanged.headers()[header::ETAG], etag);
    assert!(axum::body::to_bytes(unchanged.into_body(), 1)
        .await
        .unwrap()
        .is_empty());

    conditional.insert(header::IF_NONE_MATCH, HeaderValue::from_static("*"));
    assert_eq!(
        context_response(context, &conditional).unwrap().status(),
        StatusCode::NOT_MODIFIED
    );
}

#[test]
fn cached_context_validator_is_reused_without_rehashing_the_roster_body() {
    let expected_validator = [0x5a_u8; OBSERVER_CONTEXT_VALIDATOR_BYTES];
    let context = CachedObserverContext {
        body: bytes::Bytes::from(vec![b'x'; OBSERVER_CONTEXT_MAX_BYTES]),
        validator: expected_validator,
    };
    let encoded = encode_observer_context_cache(&context);
    assert_eq!(encoded.len(), OBSERVER_CONTEXT_CACHE_MAX_BYTES);
    let cached = decode_observer_context_cache(encoded).unwrap();
    let response = context_response(cached, &HeaderMap::new()).unwrap();
    assert_eq!(
        response.headers()[header::ETAG],
        format!("\"rsctf-koth-context-{}\"", hex::encode(expected_validator))
    );
}

#[test]
fn observer_context_cache_and_work_bounds_are_explicit() {
    assert_ne!(
        observer_context_cache_key(7, 9, 41, ContextVersion::V2),
        observer_context_cache_key(7, 9, 42, ContextVersion::V2)
    );
    assert_ne!(
        observer_context_cache_key(7, 9, 41, ContextVersion::V1),
        observer_context_cache_key(7, 9, 41, ContextVersion::V2)
    );
    assert!(OBSERVER_CONTEXT_TTL <= std::time::Duration::from_secs(5));
    assert!(OBSERVER_CONTEXT_DEADLINE <= std::time::Duration::from_secs(2));
    assert!(OBSERVER_CONTEXT_MAX_BYTES <= 512 * 1_024);
    assert_eq!(
        OBSERVER_CONTEXT_CACHE_MAX_BYTES,
        OBSERVER_CONTEXT_MAX_BYTES + OBSERVER_CONTEXT_VALIDATOR_BYTES
    );
    assert!(OBSERVER_CONTEXT_CHALLENGE_WEIGHT < OBSERVER_CONTEXT_GLOBAL_WEIGHT);
}

#[test]
fn transient_referee_database_failures_are_retryable_without_hiding_query_bugs() {
    assert_eq!(
        admission::referee_database_error(sqlx::Error::PoolTimedOut, "retry").status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        admission::referee_database_error(sqlx::Error::RowNotFound, "retry").status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[test]
fn timestamp_window_is_strict() {
    let now = 1_000_000_i64;
    let mut headers = HeaderMap::new();
    headers.insert(
        TIMESTAMP_HEADER,
        HeaderValue::from_str(&(now - MAX_CLOCK_SKEW_MS as i64).to_string()).unwrap(),
    );
    assert!(parse_timestamp(&headers, now).is_ok());
    headers.insert(
        TIMESTAMP_HEADER,
        HeaderValue::from_str(&(now - MAX_CLOCK_SKEW_MS as i64 - 1).to_string()).unwrap(),
    );
    assert!(parse_timestamp(&headers, now).is_err());
}
