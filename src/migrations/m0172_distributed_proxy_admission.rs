//! Cross-replica live proxy and open-churn admission leases.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "ProxyTunnelLeases" (
  lease_id UUID PRIMARY KEY,
  user_id UUID NOT NULL,
  scope_kind SMALLINT NOT NULL CHECK (scope_kind BETWEEN 0 AND 2),
  scope_id TEXT NOT NULL CHECK (LENGTH(scope_id) BETWEEN 1 AND 64),
  source_ip TEXT NOT NULL CHECK (LENGTH(source_ip) BETWEEN 2 AND 64),
  event_id INTEGER NULL,
  workload_id UUID NOT NULL,
  expires_at_utc TIMESTAMPTZ NOT NULL,
  created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX IF NOT EXISTS ix_proxy_tunnel_lease_expiry
  ON "ProxyTunnelLeases"(expires_at_utc);
CREATE INDEX IF NOT EXISTS ix_proxy_tunnel_lease_user
  ON "ProxyTunnelLeases"(user_id, expires_at_utc);
CREATE INDEX IF NOT EXISTS ix_proxy_tunnel_lease_scope
  ON "ProxyTunnelLeases"(scope_kind, scope_id, expires_at_utc);
CREATE INDEX IF NOT EXISTS ix_proxy_tunnel_lease_source
  ON "ProxyTunnelLeases"(source_ip, expires_at_utc);
CREATE INDEX IF NOT EXISTS ix_proxy_tunnel_lease_event
  ON "ProxyTunnelLeases"(event_id, expires_at_utc) WHERE event_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_proxy_tunnel_lease_workload
  ON "ProxyTunnelLeases"(workload_id, expires_at_utc);

CREATE TABLE IF NOT EXISTS "ProxyOpenBudgets" (
  bucket_start_utc TIMESTAMPTZ NOT NULL,
  source_key TEXT NOT NULL CHECK (LENGTH(source_key) BETWEEN 1 AND 64),
  open_count INTEGER NOT NULL CHECK (open_count BETWEEN 1 AND 128),
  PRIMARY KEY (bucket_start_utc, source_key),
  CONSTRAINT ck_proxy_open_bucket CHECK (
    bucket_start_utc = date_trunc('second', bucket_start_utc)
  )
);
CREATE INDEX IF NOT EXISTS ix_proxy_open_budget_expiry
  ON "ProxyOpenBudgets"(bucket_start_utc);
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
