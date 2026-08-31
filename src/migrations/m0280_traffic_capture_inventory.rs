//! Durable, indexed metadata for traffic-capture files.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "TrafficCaptureInventoryState" (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    reconciled_at_utc TIMESTAMPTZ NULL,
    updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

INSERT INTO "TrafficCaptureInventoryState" (singleton)
VALUES (TRUE)
ON CONFLICT (singleton) DO NOTHING;

CREATE TABLE IF NOT EXISTS "TrafficCaptureBuckets" (
    challenge_id INTEGER NOT NULL CHECK (challenge_id > 0),
    participation_id INTEGER NOT NULL CHECK (participation_id > 0),
    file_count INTEGER NOT NULL DEFAULT 0 CHECK (file_count >= 0),
    total_bytes BIGINT NOT NULL DEFAULT 0 CHECK (total_bytes >= 0),
    latest_modified_at_utc TIMESTAMPTZ NULL,
    updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (challenge_id, participation_id)
);

CREATE TABLE IF NOT EXISTS "TrafficCaptureFiles" (
    challenge_id INTEGER NOT NULL CHECK (challenge_id > 0),
    participation_id INTEGER NOT NULL CHECK (participation_id > 0),
    file_name VARCHAR(255) NOT NULL
        CHECK (
            file_name <> ''
            AND file_name NOT LIKE '%/%'
            AND file_name NOT LIKE E'%\\\\%'
            AND file_name NOT LIKE '%..%'
            AND LOWER(RIGHT(file_name, 5)) = '.pcap'
        ),
    size_bytes BIGINT NOT NULL CHECK (size_bytes >= 0),
    modified_at_utc TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (challenge_id, participation_id, file_name)
);

CREATE INDEX IF NOT EXISTS ix_trafficcapturefiles_newest
    ON "TrafficCaptureFiles"
       (challenge_id, participation_id, modified_at_utc DESC, file_name DESC)
    INCLUDE (size_bytes);

CREATE INDEX IF NOT EXISTS ix_trafficcapturebuckets_challenge_newest
    ON "TrafficCaptureBuckets"
       (challenge_id, latest_modified_at_utc DESC NULLS LAST, participation_id DESC)
    INCLUDE (file_count, total_bytes);

CREATE OR REPLACE FUNCTION rsctf_update_traffic_capture_bucket()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
DECLARE
    bucket_challenge_id INTEGER;
    bucket_participation_id INTEGER;
BEGIN
    bucket_challenge_id := COALESCE(NEW.challenge_id, OLD.challenge_id);
    bucket_participation_id := COALESCE(NEW.participation_id, OLD.participation_id);

    IF TG_OP = 'INSERT' THEN
        INSERT INTO "TrafficCaptureBuckets"
            (challenge_id, participation_id, file_count, total_bytes,
             latest_modified_at_utc, updated_at_utc)
        VALUES
            (NEW.challenge_id, NEW.participation_id, 1, NEW.size_bytes,
             NEW.modified_at_utc, clock_timestamp())
        ON CONFLICT (challenge_id, participation_id) DO UPDATE
          SET file_count = "TrafficCaptureBuckets".file_count + 1,
              total_bytes = "TrafficCaptureBuckets".total_bytes + EXCLUDED.total_bytes,
              latest_modified_at_utc = CASE
                  WHEN "TrafficCaptureBuckets".latest_modified_at_utc IS NULL
                    THEN EXCLUDED.latest_modified_at_utc
                  ELSE GREATEST(
                      "TrafficCaptureBuckets".latest_modified_at_utc,
                      EXCLUDED.latest_modified_at_utc
                  )
              END,
              updated_at_utc = clock_timestamp();
    ELSIF TG_OP = 'UPDATE' THEN
        UPDATE "TrafficCaptureBuckets"
           SET total_bytes = GREATEST(total_bytes + NEW.size_bytes - OLD.size_bytes, 0),
               latest_modified_at_utc = CASE
                   WHEN latest_modified_at_utc IS NULL
                     OR NEW.modified_at_utc >= latest_modified_at_utc
                     THEN NEW.modified_at_utc
                   WHEN OLD.modified_at_utc >= latest_modified_at_utc
                     THEN (
                         SELECT MAX(file.modified_at_utc)
                           FROM "TrafficCaptureFiles" file
                          WHERE file.challenge_id = bucket_challenge_id
                            AND file.participation_id = bucket_participation_id
                     )
                   ELSE latest_modified_at_utc
               END,
               updated_at_utc = clock_timestamp()
         WHERE challenge_id = bucket_challenge_id
           AND participation_id = bucket_participation_id;
    ELSE
        DELETE FROM "TrafficCaptureBuckets"
         WHERE challenge_id = bucket_challenge_id
           AND participation_id = bucket_participation_id
           AND file_count <= 1;
        IF NOT FOUND THEN
            UPDATE "TrafficCaptureBuckets"
               SET file_count = file_count - 1,
                   total_bytes = GREATEST(total_bytes - OLD.size_bytes, 0),
                   latest_modified_at_utc = CASE
                       WHEN OLD.modified_at_utc >= latest_modified_at_utc
                         THEN (
                             SELECT MAX(file.modified_at_utc)
                               FROM "TrafficCaptureFiles" file
                              WHERE file.challenge_id = bucket_challenge_id
                                AND file.participation_id = bucket_participation_id
                         )
                       ELSE latest_modified_at_utc
                   END,
                   updated_at_utc = clock_timestamp()
             WHERE challenge_id = bucket_challenge_id
               AND participation_id = bucket_participation_id;
        END IF;
    END IF;

    UPDATE "TrafficCaptureInventoryState"
       SET updated_at_utc = clock_timestamp()
     WHERE singleton = TRUE;
    RETURN NULL;
END
$function$;

DROP TRIGGER IF EXISTS tr_trafficcapturefiles_bucket ON "TrafficCaptureFiles";
CREATE TRIGGER tr_trafficcapturefiles_bucket
    AFTER INSERT OR UPDATE OR DELETE ON "TrafficCaptureFiles"
    FOR EACH ROW
    EXECUTE FUNCTION rsctf_update_traffic_capture_bucket();
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Production migrations are forward-only. Older binaries safely ignore
        // the inventory tables while capture files remain authoritative.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn inventory_is_idempotent_counted_and_cursor_indexed() {
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"TrafficCaptureFiles\""));
        assert!(UP_SQL.contains("PRIMARY KEY (challenge_id, participation_id, file_name)"));
        assert!(UP_SQL.contains("ix_trafficcapturefiles_newest"));
        assert!(UP_SQL.contains("modified_at_utc DESC, file_name DESC"));
        assert!(UP_SQL.contains("ix_trafficcapturebuckets_challenge_newest"));
        assert!(UP_SQL.contains("ON CONFLICT (challenge_id, participation_id) DO UPDATE"));
        assert!(UP_SQL.contains("AFTER INSERT OR UPDATE OR DELETE"));
        assert!(UP_SQL.contains("TrafficCaptureInventoryState"));
    }
}
