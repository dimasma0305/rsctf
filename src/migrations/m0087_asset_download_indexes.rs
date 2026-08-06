//! Reverse-reference indexes for the attachment authorization gate.
//!
//! Asset URLs start from a content hash and must resolve every owner. The
//! schema's foreign-key direction favors owner-to-file writes, so without
//! these indexes a cache fill scans users, teams, challenges, participations,
//! and instance attachments. Partial indexes keep null-heavy tables compact.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS ix_users_avatar_hash
  ON "AspNetUsers" (avatar_hash) WHERE avatar_hash IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_teams_avatar_hash
  ON "Teams" (avatar_hash) WHERE avatar_hash IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_games_poster_hash
  ON "Games" (poster_hash) WHERE poster_hash IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_attachments_local_file
  ON "Attachments" (local_file_id) WHERE local_file_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_challenges_attachment
  ON "GameChallenges" (attachment_id) WHERE attachment_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_flagcontexts_attachment
  ON "FlagContexts" (attachment_id) WHERE attachment_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_instances_flag
  ON "GameInstances" (flag_id) WHERE flag_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_participations_writeup
  ON "Participations" (writeup_id) WHERE writeup_id IS NOT NULL;
"#;

const DOWN_SQL: &str = r#"
DROP INDEX IF EXISTS ix_participations_writeup;
DROP INDEX IF EXISTS ix_instances_flag;
DROP INDEX IF EXISTS ix_flagcontexts_attachment;
DROP INDEX IF EXISTS ix_challenges_attachment;
DROP INDEX IF EXISTS ix_attachments_local_file;
DROP INDEX IF EXISTS ix_games_poster_hash;
DROP INDEX IF EXISTS ix_teams_avatar_hash;
DROP INDEX IF EXISTS ix_users_avatar_hash;
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(DOWN_SQL)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{DOWN_SQL, UP_SQL};

    #[test]
    fn every_reverse_lookup_has_an_idempotent_partial_index() {
        for (name, table, column) in [
            ("ix_users_avatar_hash", "AspNetUsers", "avatar_hash"),
            ("ix_teams_avatar_hash", "Teams", "avatar_hash"),
            ("ix_games_poster_hash", "Games", "poster_hash"),
            ("ix_attachments_local_file", "Attachments", "local_file_id"),
            (
                "ix_challenges_attachment",
                "GameChallenges",
                "attachment_id",
            ),
            (
                "ix_flagcontexts_attachment",
                "FlagContexts",
                "attachment_id",
            ),
            ("ix_instances_flag", "GameInstances", "flag_id"),
            ("ix_participations_writeup", "Participations", "writeup_id"),
        ] {
            assert!(UP_SQL.contains(&format!("CREATE INDEX IF NOT EXISTS {name}")));
            assert!(UP_SQL.contains(&format!("ON \"{table}\" ({column})")));
            assert!(UP_SQL.contains(&format!("WHERE {column} IS NOT NULL")));
            assert!(DOWN_SQL.contains(&format!("DROP INDEX IF EXISTS {name}")));
        }
    }
}
