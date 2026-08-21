use std::str::FromStr;
use std::sync::Arc;

use chrono::{Duration, Utc};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::sync::Barrier;
use uuid::Uuid;

use super::*;

pub(super) struct Harness {
    pub(super) pool: sqlx::PgPool,
}

impl Harness {
    pub(super) async fn new() -> Self {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("identity_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "AspNetUsers" (
                id UUID PRIMARY KEY,
                user_name TEXT,
                normalized_email TEXT,
                last_signed_in_utc TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                register_time_utc TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                security_stamp TEXT,
                ip TEXT NOT NULL DEFAULT '0.0.0.0',
                browser_fingerprint TEXT,
                email_confirmed BOOLEAN NOT NULL DEFAULT TRUE,
                role SMALLINT NOT NULL DEFAULT 1
            );
            CREATE TABLE "Configs" (
                config_key TEXT PRIMARY KEY,
                value TEXT,
                cache_keys TEXT
            );
            CREATE TABLE "Games" (
                id INTEGER PRIMARY KEY,
                practice_mode BOOLEAN NOT NULL DEFAULT FALSE,
                deletion_pending BOOLEAN NOT NULL DEFAULT FALSE,
                start_time_utc TIMESTAMPTZ NOT NULL,
                end_time_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "SuspicionReconciliationState" (
                game_id INTEGER PRIMARY KEY,
                evidence_closed_at_utc TIMESTAMPTZ
            );
            CREATE TABLE "Participations" (
                id INTEGER PRIMARY KEY,
                game_id INTEGER NOT NULL,
                team_id INTEGER NOT NULL,
                status SMALLINT NOT NULL
            );
            CREATE TABLE "Teams" (
                id INTEGER PRIMARY KEY,
                captain_id UUID NOT NULL,
                deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "UserParticipations" (
                user_id UUID NOT NULL,
                game_id INTEGER NOT NULL,
                team_id INTEGER NOT NULL,
                participation_id INTEGER NOT NULL,
                PRIMARY KEY (user_id, game_id)
            );
            CREATE TABLE "TeamMembers" (
                team_id INTEGER NOT NULL,
                user_id UUID NOT NULL,
                PRIMARY KEY (team_id, user_id)
            );
            CREATE TABLE "IdentityObservations" (
                id BIGSERIAL PRIMARY KEY,
                user_id UUID NOT NULL,
                team_id INTEGER,
                game_id INTEGER,
                participation_id INTEGER,
                kind TEXT NOT NULL,
                value_hash BYTEA NOT NULL,
                subnet_group_hash BYTEA,
                broad_network_hash BYTEA,
                value_hint TEXT NOT NULL,
                source TEXT NOT NULL,
                observed_at_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "AntiCheatBlocks" (
                id SERIAL PRIMARY KEY,
                user_id UUID NOT NULL,
                user_name TEXT,
                conflict_user_id UUID,
                conflict_user_name TEXT,
                kind TEXT NOT NULL,
                conflicting_value TEXT,
                conflicting_value_hash BYTEA,
                occurred_at_utc TIMESTAMPTZ NOT NULL,
                adjudicated_at_utc TIMESTAMPTZ,
                adjudicated_by_user_id UUID,
                exemption_expires_at_utc TIMESTAMPTZ
            );
            CREATE TABLE "AntiCheatExemptions" (
                id BIGSERIAL PRIMARY KEY,
                user_a UUID NOT NULL,
                user_b UUID NOT NULL,
                kind TEXT NOT NULL,
                value_hash BYTEA NOT NULL,
                created_from_block_id INTEGER NOT NULL,
                created_by_user_id UUID NOT NULL,
                created_at_utc TIMESTAMPTZ NOT NULL,
                expires_at_utc TIMESTAMPTZ NOT NULL,
                revoked_at_utc TIMESTAMPTZ
            );
            CREATE TABLE "FingerprintChallenges" (
                nonce_hash BYTEA PRIMARY KEY,
                required_signals TEXT[] NOT NULL,
                created_at_utc TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                expires_at_utc TIMESTAMPTZ NOT NULL,
                consumed_at_utc TIMESTAMPTZ
            );
            CREATE TABLE "IdentityObservationBootstrapState" (
                version SMALLINT PRIMARY KEY,
                key_identifier BYTEA NOT NULL,
                completed_at_utc TIMESTAMPTZ NOT NULL,
                observations_inserted BIGINT NOT NULL
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        Self { pool }
    }
}

pub(super) async fn set_policy(pool: &sqlx::PgPool, policy: PolicyFlags) {
    for (key, enabled) in [
        (
            "AccountPolicy:EnableBrowserFingerprint",
            policy.enable_browser_fingerprint,
        ),
        (
            "AccountPolicy:RequireUniqueIpPerTeamUser",
            policy.require_unique_ip_per_team_user,
        ),
        (
            "AccountPolicy:RequireUniqueFingerprintPerTeamUser",
            policy.require_unique_fingerprint_per_team_user,
        ),
        (
            "AccountPolicy:RequireUniqueIpGlobal",
            policy.require_unique_ip_global,
        ),
        (
            "AccountPolicy:RequireUniqueFingerprintGlobal",
            policy.require_unique_fingerprint_global,
        ),
    ] {
        sqlx::query(
            r#"INSERT INTO "Configs" (config_key, value)
               VALUES ($1,$2)
               ON CONFLICT (config_key) DO UPDATE SET value = EXCLUDED.value"#,
        )
        .bind(key)
        .bind(enabled.to_string())
        .execute(pool)
        .await
        .unwrap();
    }
}

pub(super) fn test_config() -> AppConfig {
    let mut config = AppConfig::from_env();
    config.identity_hash_key = "identity-test-key-0123456789abcdef".to_string();
    config
}

pub(super) async fn insert_user(pool: &sqlx::PgPool, id: Uuid, name: &str, ip: &str) {
    sqlx::query(
        r#"INSERT INTO "AspNetUsers" (id, user_name, security_stamp, ip)
           VALUES ($1, $2, 'stamp', $3)"#,
    )
    .bind(id)
    .bind(name)
    .bind(ip)
    .execute(pool)
    .await
    .unwrap();
}

pub(super) async fn insert_observation(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    team_id: Option<i32>,
    game_id: Option<i32>,
    participation_id: Option<i32>,
    value: &IdentityValue,
    observed_at: chrono::DateTime<Utc>,
) {
    sqlx::query(
        r#"INSERT INTO "IdentityObservations"
             (user_id, team_id, game_id, participation_id, kind, value_hash,
              subnet_group_hash, broad_network_hash, value_hint, source,
              observed_at_utc)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,'Password',$10)"#,
    )
    .bind(user_id)
    .bind(team_id)
    .bind(game_id)
    .bind(participation_id)
    .bind(value.kind)
    .bind(&value.hash)
    .bind(&value.subnet_group_hash)
    .bind(&value.broad_network_hash)
    .bind(&value.hint)
    .bind(observed_at)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn policy_matrix_and_exact_24_hour_boundary() {
    let harness = Harness::new().await;
    let key = test_config().identity_hash_key;
    let first = Uuid::new_v4();
    let teammate = Uuid::new_v4();
    let outsider = Uuid::new_v4();
    for (id, name) in [(first, "first"), (teammate, "mate"), (outsider, "out")] {
        insert_user(&harness.pool, id, name, "0.0.0.0").await;
    }
    let now = Utc::now();
    sqlx::query(
        r#"INSERT INTO "Games" (id, start_time_utc, end_time_utc)
           VALUES (1, $1, $2)"#,
    )
    .bind(now - Duration::hours(1))
    .bind(now + Duration::hours(1))
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "Teams" (id, captain_id) VALUES (10,$1), (20,$2)"#)
        .bind(first)
        .bind(outsider)
        .execute(&harness.pool)
        .await
        .unwrap();
    for (user_id, team_id, participation_id) in
        [(first, 10, 1), (teammate, 10, 2), (outsider, 20, 3)]
    {
        sqlx::query(
            r#"INSERT INTO "Participations" (id, game_id, team_id, status)
               VALUES ($1,1,$2,$3)"#,
        )
        .bind(participation_id)
        .bind(team_id)
        .bind(crate::utils::enums::ParticipationStatus::Accepted as i16)
        .execute(&harness.pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "UserParticipations"
                 (user_id, game_id, team_id, participation_id)
               VALUES ($1,1,$2,$3)"#,
        )
        .bind(user_id)
        .bind(team_id)
        .bind(participation_id)
        .execute(&harness.pool)
        .await
        .unwrap();
        if user_id == teammate {
            // The captain intentionally has no TeamMembers projection: the
            // policy's canonical roster union must still connect the pair.
            sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES ($1,$2)"#)
                .bind(team_id)
                .bind(user_id)
                .execute(&harness.pool)
                .await
                .unwrap();
        }
    }
    let identity = prepare_identity(key.as_bytes(), Some("192.0.2.44"), Some(&"a".repeat(64)));
    for value in &identity.values {
        insert_observation(&harness.pool, first, Some(10), Some(1), Some(1), value, now).await;
    }

    let mut transaction = harness.pool.begin().await.unwrap();
    assert!(find_conflict(
        &mut transaction,
        PolicyFlags::default(),
        teammate,
        &identity,
        now - Duration::hours(24)
    )
    .await
    .unwrap()
    .is_none());
    assert_eq!(
        find_conflict(
            &mut transaction,
            PolicyFlags {
                require_unique_ip_per_team_user: true,
                ..Default::default()
            },
            teammate,
            &identity,
            now - Duration::hours(24)
        )
        .await
        .unwrap()
        .unwrap()
        .value
        .kind,
        "Ip"
    );
    assert!(find_conflict(
        &mut transaction,
        PolicyFlags {
            require_unique_ip_per_team_user: true,
            ..Default::default()
        },
        outsider,
        &identity,
        now - Duration::hours(24)
    )
    .await
    .unwrap()
    .is_none());
    assert_eq!(
        find_conflict(
            &mut transaction,
            PolicyFlags {
                require_unique_fingerprint_global: true,
                enable_browser_fingerprint: true,
                ..Default::default()
            },
            outsider,
            &PreparedIdentity {
                ip: None,
                values: vec![identity.values[1].clone()],
            },
            now - Duration::hours(24)
        )
        .await
        .unwrap()
        .unwrap()
        .value
        .kind,
        "Fingerprint"
    );

    let boundary_identity = prepare_identity(key.as_bytes(), Some("198.51.100.8"), None);
    insert_observation(
        &harness.pool,
        first,
        None,
        None,
        None,
        &boundary_identity.values[0],
        now - Duration::hours(24),
    )
    .await;
    assert!(find_conflict(
        &mut transaction,
        PolicyFlags {
            require_unique_ip_global: true,
            ..Default::default()
        },
        outsider,
        &boundary_identity,
        now - Duration::hours(24)
    )
    .await
    .unwrap()
    .is_none());
    transaction.rollback().await.unwrap();

    let newer_identity = prepare_identity(key.as_bytes(), Some("198.51.100.9"), None);
    insert_observation(
        &harness.pool,
        first,
        None,
        None,
        None,
        &newer_identity.values[0],
        now - Duration::hours(24) + Duration::milliseconds(1),
    )
    .await;
    let mut transaction = harness.pool.begin().await.unwrap();
    assert!(find_conflict(
        &mut transaction,
        PolicyFlags {
            require_unique_ip_global: true,
            ..Default::default()
        },
        outsider,
        &newer_identity,
        now - Duration::hours(24)
    )
    .await
    .unwrap()
    .is_some());

    sqlx::query(
        r#"INSERT INTO "Games" (id, practice_mode, start_time_utc, end_time_utc)
           VALUES (2, TRUE, $1, $2)"#,
    )
    .bind(now - Duration::hours(2))
    .bind(now - Duration::hours(1))
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "Participations" (id, game_id, team_id, status)
           VALUES (4,2,20,$1)"#,
    )
    .bind(crate::utils::enums::ParticipationStatus::Suspended as i16)
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "UserParticipations"
             (user_id, game_id, team_id, participation_id)
           VALUES ($1,2,20,4)"#,
    )
    .bind(outsider)
    .execute(&harness.pool)
    .await
    .unwrap();
    let practice_identity = prepare_identity(key.as_bytes(), Some("198.18.0.1"), None);
    let mut practice_transaction = harness.pool.begin().await.unwrap();
    record_observations(
        &mut practice_transaction,
        outsider,
        &practice_identity,
        IdentitySource::OAuth,
        now,
        None,
    )
    .await
    .unwrap();
    practice_transaction.commit().await.unwrap();
    let post_end_practice_rows: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM "IdentityObservations"
            WHERE user_id = $1 AND game_id = 2 AND source = 'OAuth'"#,
    )
    .bind(outsider)
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!(post_end_practice_rows, 0);
    let global_rows: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM "IdentityObservations"
            WHERE user_id = $1 AND game_id IS NULL AND source = 'OAuth'"#,
    )
    .bind(outsider)
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!(global_rows, 1);
    let active_competition_rows: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM "IdentityObservations"
            WHERE user_id = $1 AND game_id = 1 AND source = 'OAuth'"#,
    )
    .bind(outsider)
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!(active_competition_rows, 1);
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn register_then_join_cannot_bypass_per_team_identity_policy() {
    let harness = Harness::new().await;
    let config = test_config();
    let key = config.identity_hash_key.clone();
    set_policy(
        &harness.pool,
        PolicyFlags {
            require_unique_ip_per_team_user: true,
            ..Default::default()
        },
    )
    .await;
    let member = Uuid::new_v4();
    let joining = Uuid::new_v4();
    insert_user(&harness.pool, member, "member", "192.0.2.40").await;
    insert_user(&harness.pool, joining, "joining", "198.51.100.20").await;
    sqlx::query(r#"INSERT INTO "Teams" (id, captain_id) VALUES (10,$1)"#)
        .bind(member)
        .execute(&harness.pool)
        .await
        .unwrap();
    let identity = prepare_identity(key.as_bytes(), Some("192.0.2.40"), None);
    insert_observation(
        &harness.pool,
        member,
        None,
        None,
        None,
        &identity.values[0],
        Utc::now(),
    )
    .await;
    // The joining account previously logged in from the target captain's IP,
    // then moved to a clean IP for the join request. Target-roster admission
    // must check the complete accepted 24-hour history, not only this request.
    let old_identity = prepare_identity(key.as_bytes(), Some("192.0.2.40"), None);
    insert_observation(
        &harness.pool,
        joining,
        None,
        None,
        None,
        &old_identity.values[0],
        Utc::now(),
    )
    .await;

    let mut transaction = harness.pool.begin().await.unwrap();
    let outcome = super::roster::admit_team_member_in_transaction(
        &mut transaction,
        &config,
        joining,
        Some("joining"),
        10,
        Some("198.51.100.20"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(outcome, AdmissionOutcome::Blocked);
    transaction.commit().await.unwrap();
    let joined: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(SELECT 1 FROM "TeamMembers"
                          WHERE team_id = 10 AND user_id = $1)"#,
    )
    .bind(joining)
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert!(!joined);
    let blocks: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "AntiCheatBlocks" WHERE user_id = $1"#)
            .bind(joining)
            .fetch_one(&harness.pool)
            .await
            .unwrap();
    assert_eq!(blocks, 1);
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn concurrent_new_identity_and_roster_join_cannot_both_be_accepted() {
    let harness = Harness::new().await;
    let config = test_config();
    let key = config.identity_hash_key.clone();
    let member = Uuid::new_v4();
    let joining = Uuid::new_v4();
    insert_user(&harness.pool, member, "member", "198.51.100.8").await;
    insert_user(&harness.pool, joining, "joining", "192.0.2.8").await;
    sqlx::query(r#"INSERT INTO "Teams" (id, captain_id) VALUES (10,$1)"#)
        .bind(member)
        .execute(&harness.pool)
        .await
        .unwrap();
    let member_identity = prepare_identity(key.as_bytes(), Some("198.51.100.8"), None);
    let original_joiner_identity = prepare_identity(key.as_bytes(), Some("192.0.2.8"), None);
    insert_observation(
        &harness.pool,
        member,
        None,
        None,
        None,
        &member_identity.values[0],
        Utc::now(),
    )
    .await;
    insert_observation(
        &harness.pool,
        joining,
        None,
        None,
        None,
        &original_joiner_identity.values[0],
        Utc::now(),
    )
    .await;

    set_policy(
        &harness.pool,
        PolicyFlags {
            require_unique_ip_per_team_user: true,
            ..Default::default()
        },
    )
    .await;
    let barrier = Arc::new(Barrier::new(2));
    let roster_pool = harness.pool.clone();
    let roster_config = config.clone();
    let roster_barrier = barrier.clone();
    let roster = tokio::spawn(async move {
        let mut transaction = roster_pool.begin().await.unwrap();
        roster_barrier.wait().await;
        let outcome = super::roster::admit_team_member_in_transaction(
            &mut transaction,
            &roster_config,
            joining,
            Some("joining"),
            10,
            Some("192.0.2.8"),
            None,
        )
        .await
        .unwrap();
        if outcome == AdmissionOutcome::Accepted {
            sqlx::query(r#"SELECT id FROM "AspNetUsers" WHERE id = $1 FOR SHARE"#)
                .bind(joining)
                .execute(&mut *transaction)
                .await
                .unwrap();
            sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES (10,$1)"#)
                .bind(joining)
                .execute(&mut *transaction)
                .await
                .unwrap();
        }
        transaction.commit().await.unwrap();
        outcome
    });
    let login_pool = harness.pool.clone();
    let login_config = config.clone();
    let login_barrier = barrier.clone();
    let login = tokio::spawn(async move {
        login_barrier.wait().await;
        admit_existing_user(
            &login_pool,
            &login_config,
            joining,
            Some("joining"),
            Some("198.51.100.8"),
            None,
            IdentitySource::Password,
            "stamp",
            None,
            crate::services::captcha::CaptchaAdmission::Local(None),
        )
        .await
        .unwrap()
    });
    let (roster_outcome, login_outcome) =
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::try_join!(roster, login)
        })
        .await
        .expect("identity/account lock ordering timed out")
        .expect("identity operations join");
    assert!(
        (roster_outcome == AdmissionOutcome::Accepted
            && login_outcome == AdmissionOutcome::Blocked)
            || (roster_outcome == AdmissionOutcome::Blocked
                && login_outcome == AdmissionOutcome::Accepted)
    );
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn concurrent_global_admission_allows_exactly_one_identity_owner() {
    let harness = Harness::new().await;
    let key = test_config().identity_hash_key;
    let users = [Uuid::new_v4(), Uuid::new_v4()];
    for (index, user_id) in users.iter().copied().enumerate() {
        insert_user(&harness.pool, user_id, &format!("user{index}"), "0.0.0.0").await;
    }
    let identity = prepare_identity(key.as_bytes(), Some("203.0.113.7"), None);
    let barrier = Arc::new(Barrier::new(2));
    let mut tasks = Vec::new();
    for (index, user_id) in users.iter().copied().enumerate() {
        let pool = harness.pool.clone();
        let identity = identity.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            let mut transaction = pool.begin().await.unwrap();
            barrier.wait().await;
            let outcome = evaluate_admission(
                &mut transaction,
                PolicyFlags {
                    require_unique_ip_global: true,
                    ..Default::default()
                },
                user_id,
                Some(if index == 0 { "user0" } else { "user1" }),
                &identity,
                IdentitySource::Password,
                Utc::now(),
            )
            .await
            .unwrap();
            transaction.commit().await.unwrap();
            outcome
        }));
    }
    let mut outcomes = Vec::new();
    for task in tasks {
        outcomes.push(task.await.unwrap());
    }
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == AdmissionOutcome::Accepted)
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == AdmissionOutcome::Blocked)
            .count(),
        1
    );
    let observation_count: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "IdentityObservations""#)
            .fetch_one(&harness.pool)
            .await
            .unwrap();
    let block_count: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "AntiCheatBlocks""#)
        .fetch_one(&harness.pool)
        .await
        .unwrap();
    assert_eq!((observation_count, block_count), (1, 1));
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn rejected_registration_is_not_poisoned_and_exemption_reuses_stable_account() {
    let harness = Harness::new().await;
    let config = test_config();
    let first = Uuid::new_v4();
    let pending = Uuid::new_v4();
    insert_user(&harness.pool, first, "first", "192.0.2.1").await;
    let identity = prepare_identity(config.identity_hash_key.as_bytes(), Some("192.0.2.1"), None);
    insert_observation(
        &harness.pool,
        first,
        None,
        None,
        None,
        &identity.values[0],
        Utc::now(),
    )
    .await;

    let mut transaction = harness.pool.begin().await.unwrap();
    let outcome = evaluate_admission(
        &mut transaction,
        PolicyFlags {
            require_unique_ip_global: true,
            ..Default::default()
        },
        pending,
        Some("pending"),
        &identity,
        IdentitySource::Registration,
        Utc::now(),
    )
    .await
    .unwrap();
    assert_eq!(outcome, AdmissionOutcome::Blocked);
    sqlx::query(
        r#"INSERT INTO "AspNetUsers" (id, user_name, security_stamp, ip)
           VALUES ($1, 'pending', 'stamp', '0.0.0.0')"#,
    )
    .bind(pending)
    .execute(&mut *transaction)
    .await
    .unwrap();
    transaction.commit().await.unwrap();

    let pending_ip: String = sqlx::query_scalar(r#"SELECT ip FROM "AspNetUsers" WHERE id = $1"#)
        .bind(pending)
        .fetch_one(&harness.pool)
        .await
        .unwrap();
    assert_eq!(pending_ip, "0.0.0.0");
    let poisoned_observations: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "IdentityObservations" WHERE user_id = $1"#)
            .bind(pending)
            .fetch_one(&harness.pool)
            .await
            .unwrap();
    assert_eq!(poisoned_observations, 0);
    let block_id: i32 = sqlx::query_scalar(
        r#"SELECT id FROM "AntiCheatBlocks" WHERE user_id = $1 ORDER BY id DESC LIMIT 1"#,
    )
    .bind(pending)
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    exempt_block(&harness.pool, &config, block_id, Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(
        admit_existing_user(
            &harness.pool,
            &config,
            pending,
            Some("pending"),
            Some("192.0.2.1"),
            None,
            IdentitySource::Password,
            "stamp",
            None,
            crate::services::captcha::CaptchaAdmission::Local(None),
        )
        .await
        .unwrap(),
        AdmissionOutcome::Accepted
    );
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn rejected_existing_login_keeps_last_accepted_account_identity() {
    let harness = Harness::new().await;
    let config = test_config();
    let first = Uuid::new_v4();
    let rejected = Uuid::new_v4();
    insert_user(&harness.pool, first, "first", "203.0.113.50").await;
    insert_user(&harness.pool, rejected, "rejected", "192.0.2.9").await;
    set_policy(
        &harness.pool,
        PolicyFlags {
            require_unique_ip_global: true,
            ..Default::default()
        },
    )
    .await;
    let identity = prepare_identity(
        config.identity_hash_key.as_bytes(),
        Some("203.0.113.50"),
        None,
    );
    insert_observation(
        &harness.pool,
        first,
        None,
        None,
        None,
        &identity.values[0],
        Utc::now(),
    )
    .await;
    let signed_in_before: chrono::DateTime<Utc> =
        sqlx::query_scalar(r#"SELECT last_signed_in_utc FROM "AspNetUsers" WHERE id = $1"#)
            .bind(rejected)
            .fetch_one(&harness.pool)
            .await
            .unwrap();

    assert_eq!(
        admit_existing_user(
            &harness.pool,
            &config,
            rejected,
            Some("rejected"),
            Some("203.0.113.50"),
            None,
            IdentitySource::Password,
            "stamp",
            None,
            crate::services::captcha::CaptchaAdmission::Local(None),
        )
        .await
        .unwrap(),
        AdmissionOutcome::Blocked
    );
    let (ip_after, signed_in_after): (String, chrono::DateTime<Utc>) =
        sqlx::query_as(r#"SELECT ip, last_signed_in_utc FROM "AspNetUsers" WHERE id = $1"#)
            .bind(rejected)
            .fetch_one(&harness.pool)
            .await
            .unwrap();
    assert_eq!(ip_after, "192.0.2.9");
    assert_eq!(signed_in_after, signed_in_before);
    let observations: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "IdentityObservations" WHERE user_id = $1"#)
            .bind(rejected)
            .fetch_one(&harness.pool)
            .await
            .unwrap();
    assert_eq!(observations, 0);
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn fingerprint_challenge_consumption_is_single_use_and_expiry_bound() {
    let harness = Harness::new().await;
    let live = vec![7_u8; 32];
    let expired = vec![8_u8; 32];
    for (hash, expires_at) in [
        (&live, Utc::now() + Duration::minutes(1)),
        (&expired, Utc::now() - Duration::milliseconds(1)),
    ] {
        sqlx::query(
            r#"INSERT INTO "FingerprintChallenges"
                 (nonce_hash, required_signals, expires_at_utc)
               VALUES ($1, ARRAY['lie_count'], $2)"#,
        )
        .bind(hash)
        .bind(expires_at)
        .execute(&harness.pool)
        .await
        .unwrap();
    }
    assert_eq!(
        super::fingerprint::consume_challenge(&harness.pool, &live)
            .await
            .unwrap(),
        vec!["lie_count"]
    );
    assert!(super::fingerprint::consume_challenge(&harness.pool, &live)
        .await
        .is_err());
    assert!(
        super::fingerprint::consume_challenge(&harness.pool, &expired)
            .await
            .is_err()
    );
}
