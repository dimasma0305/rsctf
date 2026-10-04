//! Cover the latest-result probes used by the five-second A&D/KotH operator reads.
//!
//! Both indexes order newest evidence first for one configured service/hill, so
//! a lateral `LIMIT 1` does not walk the event's accumulated result history.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS ix_adcheckresults_service_latest
    ON "AdCheckResults" (team_service_id, checked_at DESC, id DESC)
    INCLUDE (status);

CREATE INDEX IF NOT EXISTS ix_kothcontrol_game_challenge_latest
    ON "KothControlResults" (game_id, challenge_id, checked_at DESC, id DESC)
    INCLUDE (status, ad_round_id, confirmed_participation_id, cycle_id, token_window_attempt);
"#;

const DOWN_SQL: &str = r#"
DROP INDEX IF EXISTS ix_kothcontrol_game_challenge_latest;
DROP INDEX IF EXISTS ix_adcheckresults_service_latest;
"#;

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
    use super::*;

    #[test]
    fn latest_result_indexes_match_lateral_probe_order() {
        assert!(UP_SQL.contains("team_service_id, checked_at DESC, id DESC"));
        assert!(UP_SQL.contains("game_id, challenge_id, checked_at DESC, id DESC"));
        assert_eq!(UP_SQL.matches("CREATE INDEX IF NOT EXISTS").count(), 2);
        assert_eq!(UP_SQL.matches("INCLUDE (").count(), 2);
    }
}
