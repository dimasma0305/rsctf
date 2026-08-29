use sea_orm_migration::prelude::*;

pub const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "GameNoticeOperations" (
    game_id INTEGER NOT NULL REFERENCES "Games"(id) ON DELETE CASCADE,
    operation_id UUID NOT NULL,
    request_fingerprint BYTEA NOT NULL CHECK (octet_length(request_fingerprint) = 32),
    result JSONB,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    completed_at_utc TIMESTAMPTZ,
    PRIMARY KEY (game_id, operation_id),
    CHECK ((result IS NULL) = (completed_at_utc IS NULL))
);

CREATE TABLE IF NOT EXISTS "GameNoticeOutbox" (
    id BIGSERIAL PRIMARY KEY,
    game_id INTEGER NOT NULL REFERENCES "Games"(id) ON DELETE CASCADE,
    notice_id INTEGER,
    operation_id UUID NOT NULL,
    event_kind SMALLINT NOT NULL CHECK (event_kind IN (0, 1)),
    payload JSONB NOT NULL,
    available_at_utc TIMESTAMPTZ NOT NULL,
    claim_token UUID,
    claimed_at_utc TIMESTAMPTZ,
    delivered_at_utc TIMESTAMPTZ,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (game_id, operation_id, event_kind)
);

CREATE INDEX IF NOT EXISTS ix_game_notice_outbox_due
    ON "GameNoticeOutbox" (available_at_utc, id)
    WHERE delivered_at_utc IS NULL;
CREATE INDEX IF NOT EXISTS ix_game_notice_operations_expiry
    ON "GameNoticeOperations" (created_at_utc, game_id, operation_id);
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

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

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn notice_intents_and_delivery_are_durable_and_bounded() {
        assert!(UP_SQL.contains("PRIMARY KEY (game_id, operation_id)"));
        assert!(UP_SQL.contains("request_fingerprint BYTEA"));
        assert!(UP_SQL.contains("UNIQUE (game_id, operation_id, event_kind)"));
        assert!(UP_SQL.contains("WHERE delivered_at_utc IS NULL"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn concurrent_notice_operation_and_outbox_claim_once() {
        use sqlx::postgres::PgPoolOptions;
        use uuid::Uuid;

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL").unwrap();
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("notice_delivery_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let scoped = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .after_connect(move |connection, _| {
                let statement = format!(r#"SET search_path TO "{scoped}""#);
                Box::pin(async move {
                    sqlx::query(&statement).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await
            .unwrap();
        sqlx::query(r#"CREATE TABLE "Games" (id integer PRIMARY KEY)"#)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::query(r#"INSERT INTO "Games" VALUES (7)"#)
            .execute(&pool)
            .await
            .unwrap();
        let operation = Uuid::new_v4();
        let claim = || {
            sqlx::query_scalar::<_, Uuid>(
                r#"INSERT INTO "GameNoticeOperations"
                       (game_id, operation_id, request_fingerprint)
                   VALUES (7, $1, $2) ON CONFLICT DO NOTHING
                RETURNING operation_id"#,
            )
            .bind(operation)
            .bind([9_u8; 32].as_slice())
            .fetch_optional(&pool)
        };
        let (first, second) = tokio::join!(claim(), claim());
        assert_eq!(
            usize::from(first.unwrap().is_some()) + usize::from(second.unwrap().is_some()),
            1
        );

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
