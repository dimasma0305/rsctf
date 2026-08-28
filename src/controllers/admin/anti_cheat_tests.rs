use super::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

#[test]
fn pagination_is_bounded_before_reaching_postgres() {
    assert_eq!(bounded_page(0, 0), (1, 0));
    assert_eq!(bounded_page(u64::MAX, u64::MAX), (500, 1_000_000));
}

#[test]
fn vpn_override_operation_digest_binds_every_semantic_input() {
    let id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
    let create = override_request_digest("create", "incident response", 15, None, 7);
    assert_eq!(
        create,
        override_request_digest("create", "incident response", 15, None, 7)
    );
    assert_ne!(
        create,
        override_request_digest("create", "incident response", 16, None, 7)
    );
    assert_ne!(
        create,
        override_request_digest("create", "different reason", 15, None, 7)
    );
    assert_ne!(
        create,
        override_request_digest("create", "incident response", 15, None, 8)
    );
    assert_ne!(
        create,
        override_request_digest("revoke", "", 0, Some(id), 7)
    );
    assert_ne!(
        create,
        override_request_digest_v1("create", "incident response", 15, None)
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn vpn_override_recovery_is_scoped_to_the_owning_admin_and_game() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect test database");
    let schema = format!("vpn_recovery_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin_pool)
        .await
        .expect("create isolated schema");
    let options = PgConnectOptions::from_str(&database_url)
        .expect("parse database URL")
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .expect("connect isolated schema");
    sqlx::raw_sql(
        r#"CREATE TABLE "EventVpnGateOverrides" (
               id UUID PRIMARY KEY,
               expires_at_utc TIMESTAMPTZ NOT NULL
           );
           CREATE TABLE "EventVpnOverrideOperations" (
               game_id INTEGER NOT NULL,
               operation_id UUID NOT NULL,
               actor_user_id UUID NOT NULL,
               override_id UUID NOT NULL REFERENCES "EventVpnGateOverrides"(id),
               result_revision BIGINT NOT NULL,
               PRIMARY KEY (game_id, operation_id)
           );"#,
    )
    .execute(&pool)
    .await
    .expect("create recovery fixture");
    let owner = Uuid::new_v4();
    let other_admin = Uuid::new_v4();
    let operation_id = Uuid::new_v4();
    let override_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "EventVpnGateOverrides" (id, expires_at_utc)
           VALUES ($1, clock_timestamp() + INTERVAL '5 minutes')"#,
    )
    .bind(override_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "EventVpnOverrideOperations"
                  (game_id, operation_id, actor_user_id, override_id, result_revision)
           VALUES (7, $1, $2, $3, 11)"#,
    )
    .bind(operation_id)
    .bind(owner)
    .bind(override_id)
    .execute(&pool)
    .await
    .unwrap();

    assert!(
        load_event_vpn_override_operation(&pool, 7, operation_id, owner)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        load_event_vpn_override_operation(&pool, 7, operation_id, other_admin)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        load_event_vpn_override_operation(&pool, 8, operation_id, owner)
            .await
            .unwrap()
            .is_none()
    );

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin_pool)
        .await
        .expect("drop isolated schema");
}

#[test]
fn retained_block_wire_shape_uses_millis_and_redacts_identity_values() {
    let occurred_at_utc = "2026-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let adjudicated_at_utc = "2026-01-02T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let exemption_expires_at_utc = "2026-01-09T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let adjudicator = Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap();
    let model = AntiCheatBlockModel::from(AntiCheatBlockRow {
        id: 7,
        user_id: Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
        user_name: Some("blocked".to_string()),
        conflict_user_id: Some(Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap()),
        conflict_user_name: Some("conflict".to_string()),
        kind: "Ip".to_string(),
        conflicting_value: Some("198.51.100.42".to_string()),
        occurred_at_utc,
        adjudicated_at_utc: Some(adjudicated_at_utc),
        adjudicated_by_user_id: Some(adjudicator),
        exemption_expires_at_utc: Some(exemption_expires_at_utc),
    });
    let value = serde_json::to_value(model).unwrap();
    assert_eq!(value["conflictingValue"], "198.51.100.x");
    assert_eq!(value["occurredAtUtc"], occurred_at_utc.timestamp_millis());
    assert_eq!(
        value["adjudicatedAtUtc"],
        adjudicated_at_utc.timestamp_millis()
    );
    assert_eq!(value["adjudicatedByUserId"], adjudicator.to_string());
    assert_eq!(
        value["exemptionExpiresAtUtc"],
        exemption_expires_at_utc.timestamp_millis()
    );
}
