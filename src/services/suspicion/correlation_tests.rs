use std::str::FromStr;

use sea_orm::SqlxPostgresConnector;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::*;
use crate::services::suspicion::detectors::ReconciliationSnapshot;

fn observation(
    id: i64,
    user_id: Uuid,
    team_id: i32,
    participation_id: i32,
    kind: &str,
    value_hash: &[u8],
    observed_at_utc: DateTime<Utc>,
) -> Observation {
    Observation {
        id,
        user_id,
        team_id,
        participation_id,
        kind: kind.to_string(),
        value_hash: value_hash.to_vec(),
        subnet_group_hash: None,
        broad_network_hash: None,
        observed_at_utc,
    }
}

fn exemption(
    user_a: Uuid,
    user_b: Uuid,
    kind: &str,
    value_hash: &[u8],
    created_at_utc: DateTime<Utc>,
    expires_at_utc: DateTime<Utc>,
) -> IdentityExemption {
    IdentityExemption {
        user_a,
        user_b,
        kind: kind.to_string(),
        value_hash: value_hash.to_vec(),
        created_at_utc,
        expires_at_utc,
        revoked_at_utc: None,
    }
}

#[test]
fn exemption_query_is_game_windowed_and_never_uses_reconciliation_time() {
    assert!(LOAD_IDENTITY_EXEMPTIONS_SQL.contains("observation.game_id = $1"));
    assert!(LOAD_IDENTITY_EXEMPTIONS_SQL.contains("observed_at_utc >= $2"));
    assert!(LOAD_IDENTITY_EXEMPTIONS_SQL.contains("observed_at_utc < $3"));
    assert!(LOAD_IDENTITY_EXEMPTIONS_SQL.contains("created_at_utc < $3"));
    assert!(LOAD_IDENTITY_EXEMPTIONS_SQL.contains("expires_at_utc > $2"));
    assert!(LOAD_IDENTITY_EXEMPTIONS_SQL.contains("ORDER BY exemption.user_a"));
    assert!(!LOAD_IDENTITY_EXEMPTIONS_SQL.contains("CURRENT_TIMESTAMP"));
    assert!(!LOAD_IDENTITY_EXEMPTIONS_SQL.contains("NOW()"));
}

#[test]
fn exemption_interval_is_anchored_to_each_pair_edge() {
    let created_at = "2026-06-01T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let expires_at = created_at + Duration::minutes(10);
    let user_a = Uuid::from_u128(1);
    let user_b = Uuid::from_u128(2);
    let hash = vec![0x51; 32];
    let mut exemptions = IdentityExemptions::new();
    exemptions.insert(
        (user_a, user_b),
        vec![exemption(
            user_a,
            user_b,
            "Fingerprint",
            &hash,
            created_at,
            expires_at,
        )],
    );

    assert!(!identity_edge_is_exempt(
        &exemptions,
        user_b,
        user_a,
        "Fingerprint",
        &hash,
        created_at - Duration::milliseconds(1),
    ));
    assert!(identity_edge_is_exempt(
        &exemptions,
        user_a,
        user_b,
        "Fingerprint",
        &hash,
        created_at,
    ));
    assert!(!identity_edge_is_exempt(
        &exemptions,
        user_a,
        user_b,
        "Fingerprint",
        &hash,
        expires_at,
    ));

    exemptions.get_mut(&(user_a, user_b)).unwrap()[0].revoked_at_utc =
        Some(created_at + Duration::minutes(5));
    assert!(identity_edge_is_exempt(
        &exemptions,
        user_a,
        user_b,
        "Fingerprint",
        &hash,
        created_at + Duration::minutes(4),
    ));
    assert!(!identity_edge_is_exempt(
        &exemptions,
        user_a,
        user_b,
        "Fingerprint",
        &hash,
        created_at + Duration::minutes(5),
    ));
}

#[test]
fn append_only_intervals_preserve_expired_gaps() {
    let start = "2026-06-01T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let user_a = Uuid::from_u128(1);
    let user_b = Uuid::from_u128(2);
    let hash = vec![0x51; 32];
    let mut exemptions = IdentityExemptions::new();
    exemptions.insert(
        (user_a, user_b),
        vec![
            exemption(
                user_a,
                user_b,
                "Ip",
                &hash,
                start,
                start + Duration::minutes(10),
            ),
            exemption(
                user_a,
                user_b,
                "Ip",
                &hash,
                start + Duration::minutes(20),
                start + Duration::minutes(30),
            ),
        ],
    );

    assert!(identity_edge_is_exempt(
        &exemptions,
        user_a,
        user_b,
        "Ip",
        &hash,
        start + Duration::minutes(5),
    ));
    assert!(!identity_edge_is_exempt(
        &exemptions,
        user_a,
        user_b,
        "Ip",
        &hash,
        start + Duration::minutes(15),
    ));
    assert!(identity_edge_is_exempt(
        &exemptions,
        user_a,
        user_b,
        "Ip",
        &hash,
        start + Duration::minutes(20),
    ));
}

#[test]
fn later_expiry_needs_a_new_observation() {
    let created_at = "2026-06-01T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let expires_at = created_at + Duration::minutes(10);
    let hash = vec![0x52; 32];
    let users = [Uuid::from_u128(1), Uuid::from_u128(2)];
    let mut exemptions = IdentityExemptions::new();
    exemptions.insert(
        (users[0], users[1]),
        vec![exemption(
            users[0],
            users[1],
            "Fingerprint",
            &hash,
            created_at,
            expires_at,
        )],
    );
    let mut group = IdentityGroup::default();
    group.observe(&observation(
        1,
        users[0],
        1,
        101,
        "Fingerprint",
        &hash,
        created_at + Duration::minutes(1),
    ));
    group.observe(&observation(
        2,
        users[1],
        2,
        102,
        "Fingerprint",
        &hash,
        created_at + Duration::minutes(2),
    ));
    let members = group.members.values().collect::<Vec<_>>();
    assert_eq!(
        earliest_unexempt_edge(
            &exemptions,
            users[0],
            members[0],
            users[1],
            members[1],
            "Fingerprint",
        ),
        None,
        "reconciling after expiry cannot revive an edge observed under the grant"
    );

    group.observe(&observation(
        3,
        users[1],
        2,
        102,
        "Fingerprint",
        &hash,
        expires_at,
    ));
    let members = group.members.values().collect::<Vec<_>>();
    assert_eq!(
        earliest_unexempt_edge(
            &exemptions,
            users[0],
            members[0],
            users[1],
            members[1],
            "Fingerprint",
        ),
        Some(expires_at),
        "an observation at the exclusive expiry creates a reportable edge"
    );
}

#[test]
fn exempt_pair_does_not_remove_unexempt_third_user_edges() {
    let created_at = "2026-06-01T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let expires_at = created_at + Duration::minutes(10);
    let hash = vec![0x53; 32];
    let users = [Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3)];
    let mut exemptions = IdentityExemptions::new();
    exemptions.insert(
        (users[0], users[1]),
        vec![exemption(
            users[0],
            users[1],
            "Fingerprint",
            &hash,
            created_at,
            expires_at,
        )],
    );
    let mut group = IdentityGroup::default();
    for (index, user_id) in users.into_iter().enumerate() {
        group.observe(&observation(
            i64::try_from(index + 1).unwrap(),
            user_id,
            i32::try_from(index + 1).unwrap(),
            i32::try_from(index + 101).unwrap(),
            "Fingerprint",
            &hash,
            created_at + Duration::minutes(i64::try_from(index + 1).unwrap()),
        ));
    }
    let mut groups = BTreeMap::new();
    groups.insert(hash, group);
    let mut candidates = Candidates::new();
    add_group_candidates(
        &mut candidates,
        &groups,
        &exemptions,
        "Fingerprint",
        SuspicionType::SharedFingerprint,
        "shared-fingerprint",
        false,
    );

    assert_eq!(candidates.len(), 3);
    assert!(candidates
        .keys()
        .any(|(participation_id, _, _)| *participation_id == 101));
    assert!(candidates
        .keys()
        .any(|(participation_id, _, _)| *participation_id == 102));
    assert!(candidates
        .keys()
        .any(|(participation_id, _, _)| *participation_id == 103));
}

#[test]
fn subnet_edges_use_the_underlying_exact_ip_exemption_scope() {
    let created_at = "2026-06-01T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let user_a = Uuid::from_u128(1);
    let user_b = Uuid::from_u128(2);
    let exact_ip = vec![0x54; 32];
    let other_ip = vec![0x55; 32];
    let subnet_hash = vec![0x56; 32];
    let mut exemptions = IdentityExemptions::new();
    exemptions.insert(
        (user_a, user_b),
        vec![exemption(
            user_a,
            user_b,
            "Ip",
            &exact_ip,
            created_at,
            created_at + Duration::hours(1),
        )],
    );

    let mut same_ip_group = IdentityGroup::default();
    same_ip_group.observe(&observation(
        1,
        user_a,
        1,
        101,
        "Ip",
        &exact_ip,
        created_at + Duration::minutes(1),
    ));
    same_ip_group.observe(&observation(
        2,
        user_b,
        2,
        102,
        "Ip",
        &exact_ip,
        created_at + Duration::minutes(2),
    ));
    let mut groups = BTreeMap::new();
    groups.insert(subnet_hash.clone(), same_ip_group);
    let mut candidates = Candidates::new();
    add_group_candidates(
        &mut candidates,
        &groups,
        &exemptions,
        "Ip",
        SuspicionType::SubnetOverlap,
        "subnet-overlap",
        true,
    );
    assert!(candidates.is_empty());

    let mut different_ip_group = IdentityGroup::default();
    different_ip_group.observe(&observation(
        3,
        user_a,
        1,
        101,
        "Ip",
        &exact_ip,
        created_at + Duration::minutes(1),
    ));
    different_ip_group.observe(&observation(
        4,
        user_b,
        2,
        102,
        "Ip",
        &other_ip,
        created_at + Duration::minutes(2),
    ));
    groups.insert(subnet_hash, different_ip_group);
    candidates.clear();
    add_group_candidates(
        &mut candidates,
        &groups,
        &exemptions,
        "Ip",
        SuspicionType::SubnetOverlap,
        "subnet-overlap",
        true,
    );
    assert_eq!(candidates.len(), 2);
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn live_and_final_detectors_apply_temporal_pair_exemptions() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("rsctf_correlation_exemption_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin_pool)
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
          id INTEGER PRIMARY KEY, start_time_utc TIMESTAMPTZ NOT NULL,
          end_time_utc TIMESTAMPTZ NOT NULL,
          deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
        );
        CREATE TABLE "Teams" (
          id INTEGER PRIMARY KEY,
          deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
        );
        CREATE TABLE "Participations" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, team_id INTEGER NOT NULL,
          status SMALLINT NOT NULL, competitive_admitted_at_utc TIMESTAMPTZ,
          suspicion_score INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE "UserParticipations" (
          user_id UUID NOT NULL, game_id INTEGER NOT NULL, team_id INTEGER NOT NULL,
          participation_id INTEGER NOT NULL, PRIMARY KEY (user_id, game_id)
        );
        CREATE TABLE "IdentityObservations" (
          id BIGSERIAL PRIMARY KEY, user_id UUID NOT NULL, team_id INTEGER,
          game_id INTEGER, participation_id INTEGER, kind TEXT NOT NULL,
          value_hash BYTEA NOT NULL, subnet_group_hash BYTEA,
          broad_network_hash BYTEA, observed_at_utc TIMESTAMPTZ NOT NULL
        );
        CREATE TABLE "Submissions" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
          participation_id INTEGER NOT NULL, submit_remote_ip_hash BYTEA,
          submit_time_utc TIMESTAMPTZ NOT NULL
        );
        CREATE TABLE "AntiCheatExemptions" (
          user_a UUID NOT NULL, user_b UUID NOT NULL, kind TEXT NOT NULL,
          value_hash BYTEA NOT NULL, created_at_utc TIMESTAMPTZ NOT NULL,
          expires_at_utc TIMESTAMPTZ NOT NULL, revoked_at_utc TIMESTAMPTZ
        );
        CREATE TABLE "SuspicionRules" (
          rule_code TEXT PRIMARY KEY, weight INTEGER NOT NULL
        );
        CREATE TABLE "SuspicionEvents" (
          id BIGSERIAL PRIMARY KEY, game_id INTEGER NOT NULL,
          participation_id INTEGER NOT NULL, challenge_id INTEGER,
          kind SMALLINT NOT NULL, evidence_key TEXT NOT NULL,
          score_delta INTEGER NOT NULL, created_at TIMESTAMPTZ NOT NULL,
          UNIQUE (game_id, participation_id, kind, evidence_key)
        );

        INSERT INTO "Games" (id, start_time_utc, end_time_utc)
        VALUES (1, '2026-06-01T00:00:00Z', '2026-06-10T00:00:00Z');
        INSERT INTO "Teams" (id) VALUES (1), (2), (3);
        INSERT INTO "Participations"
          (id, game_id, team_id, status, competitive_admitted_at_utc)
        VALUES
          (101, 1, 1, 1, '2026-05-31T23:00:00Z'),
          (102, 1, 2, 1, '2026-05-31T23:00:00Z'),
          (103, 1, 3, 1, '2026-05-31T23:00:00Z');
        INSERT INTO "UserParticipations"
          (user_id, game_id, team_id, participation_id)
        VALUES
          ('00000000-0000-0000-0000-000000000001', 1, 1, 101),
          ('00000000-0000-0000-0000-000000000002', 1, 2, 102),
          ('00000000-0000-0000-0000-000000000003', 1, 3, 103);
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let user_a = Uuid::from_u128(1);
    let user_b = Uuid::from_u128(2);
    let user_c = Uuid::from_u128(3);
    let fingerprint_hash = vec![0x71_u8; 32];
    for (user_id, team_id, participation_id, observed_at) in [
        (user_a, 1, 101, "2026-06-01T12:01:00Z"),
        (user_b, 2, 102, "2026-06-01T12:02:00Z"),
    ] {
        sqlx::query(
            r#"INSERT INTO "IdentityObservations"
                 (user_id, team_id, game_id, participation_id, kind,
                  value_hash, observed_at_utc)
               VALUES ($1, $2, 1, $3, 'Fingerprint', $4, $5::timestamptz)"#,
        )
        .bind(user_id)
        .bind(team_id)
        .bind(participation_id)
        .bind(&fingerprint_hash)
        .bind(observed_at)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query(
        r#"INSERT INTO "AntiCheatExemptions"
             (user_a, user_b, kind, value_hash, created_at_utc, expires_at_utc)
           VALUES ($1, $2, 'Fingerprint', $3,
                   '2026-06-01T12:00:00Z', '2026-06-01T13:00:00Z')"#,
    )
    .bind(user_a)
    .bind(user_b)
    .bind(&fingerprint_hash)
    .execute(&pool)
    .await
    .unwrap();

    let db = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
    run_correlation_checks_for_snapshot(&db, 1, ReconciliationSnapshot::Live)
        .await
        .unwrap();
    let live_events: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "SuspicionEvents" WHERE kind = $1"#)
            .bind(SuspicionType::SharedFingerprint.kind())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(live_events, 0, "the exact live pair is exempt");

    sqlx::query(
        r#"INSERT INTO "IdentityObservations"
             (user_id, team_id, game_id, participation_id, kind,
              value_hash, observed_at_utc)
           VALUES ($1, 3, 1, 103, 'Fingerprint', $2,
                   '2026-06-01T12:03:00Z')"#,
    )
    .bind(user_c)
    .bind(&fingerprint_hash)
    .execute(&pool)
    .await
    .unwrap();
    run_correlation_checks_for_snapshot(&db, 1, ReconciliationSnapshot::Live)
        .await
        .unwrap();
    let live_participations: Vec<i32> = sqlx::query_scalar(
        r#"SELECT participation_id FROM "SuspicionEvents"
            WHERE kind = $1 ORDER BY participation_id"#,
    )
    .bind(SuspicionType::SharedFingerprint.kind())
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(live_participations, vec![101, 102, 103]);

    let ip_hash = vec![0x72_u8; 32];
    for (user_id, team_id, participation_id, observed_at) in [
        (user_a, 1, 101, "2026-06-01T12:11:00Z"),
        (user_b, 2, 102, "2026-06-01T12:12:00Z"),
    ] {
        sqlx::query(
            r#"INSERT INTO "IdentityObservations"
                 (user_id, team_id, game_id, participation_id, kind,
                  value_hash, observed_at_utc)
               VALUES ($1, $2, 1, $3, 'Ip', $4, $5::timestamptz)"#,
        )
        .bind(user_id)
        .bind(team_id)
        .bind(participation_id)
        .bind(&ip_hash)
        .bind(observed_at)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query(
        r#"INSERT INTO "AntiCheatExemptions"
             (user_a, user_b, kind, value_hash, created_at_utc, expires_at_utc)
           VALUES ($1, $2, 'Ip', $3,
                   '2026-06-01T12:10:00Z', '2026-06-01T13:00:00Z')"#,
    )
    .bind(user_a)
    .bind(user_b)
    .bind(&ip_hash)
    .execute(&pool)
    .await
    .unwrap();
    run_correlation_checks_for_snapshot(&db, 1, ReconciliationSnapshot::BarrierBackedFinal)
        .await
        .unwrap();
    let final_events: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "SuspicionEvents" WHERE kind = $1"#)
            .bind(SuspicionType::CrossTeamIp.kind())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        final_events, 0,
        "expiry after the observation cannot revive final evidence"
    );

    sqlx::query(
        r#"INSERT INTO "IdentityObservations"
             (user_id, team_id, game_id, participation_id, kind,
              value_hash, observed_at_utc)
           VALUES ($1, 2, 1, 102, 'Ip', $2,
                   '2026-06-01T13:00:00Z')"#,
    )
    .bind(user_b)
    .bind(&ip_hash)
    .execute(&pool)
    .await
    .unwrap();
    run_correlation_checks_for_snapshot(&db, 1, ReconciliationSnapshot::BarrierBackedFinal)
        .await
        .unwrap();
    let final_participations: Vec<i32> = sqlx::query_scalar(
        r#"SELECT participation_id FROM "SuspicionEvents"
            WHERE kind = $1 ORDER BY participation_id"#,
    )
    .bind(SuspicionType::CrossTeamIp.kind())
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(final_participations, vec![101, 102]);

    drop(db);
    pool.close().await;
    assert!(schema.starts_with("rsctf_correlation_exemption_"));
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin_pool)
        .await
        .unwrap();
    admin_pool.close().await;
}
