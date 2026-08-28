//! Deployment-wide bounded admission for challenge-variant generator sandboxes.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "VariantGenerationSlots" (
    slot SMALLINT PRIMARY KEY,
    owner_id UUID NULL,
    lease_expires_at_utc TIMESTAMPTZ NULL,
    CONSTRAINT ck_variant_generation_slot_number CHECK (slot BETWEEN 0 AND 1),
    CONSTRAINT ck_variant_generation_slot_owner CHECK (
        (owner_id IS NULL) = (lease_expires_at_utc IS NULL)
    )
);
INSERT INTO "VariantGenerationSlots" (slot) VALUES (0), (1)
ON CONFLICT (slot) DO NOTHING;
CREATE INDEX IF NOT EXISTS ix_variant_generation_slot_recovery
    ON "VariantGenerationSlots" (lease_expires_at_utc, slot)
    WHERE owner_id IS NOT NULL;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Production migrations are forward-only. Retain distributed leases so
        // rollback tooling cannot silently remove the active-work fence.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn two_recoverable_slots_bound_all_replicas() {
        assert!(UP_SQL.contains("slot BETWEEN 0 AND 1"));
        assert!(UP_SQL.contains("VALUES (0), (1)"));
        assert!(UP_SQL.contains("ON CONFLICT (slot) DO NOTHING"));
        assert!(UP_SQL.contains("ix_variant_generation_slot_recovery"));
    }
}
