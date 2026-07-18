//! Enforce one roster entry per team/user pair and index user-first lookups.
//!
//! Four older non-unique indexes became redundant after later migrations added
//! unique indexes over the same columns. Removing only those exact duplicates
//! reduces write amplification without changing the supported lookup order.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
    LOCK TABLE "TeamMembers" IN SHARE ROW EXCLUSIVE MODE;

    DELETE FROM "TeamMembers" duplicate
     USING "TeamMembers" canonical
     WHERE duplicate.team_id = canonical.team_id
       AND duplicate.user_id = canonical.user_id
       AND duplicate.id > canonical.id;

    CREATE UNIQUE INDEX IF NOT EXISTS ux_teammembers_team_user
      ON "TeamMembers"(team_id, user_id);
    CREATE INDEX IF NOT EXISTS ix_teammembers_user_team
      ON "TeamMembers"(user_id, team_id);

    DROP INDEX IF EXISTS ix_adflags_round_service;
    DROP INDEX IF EXISTS ix_adrounds_game_number;
    DROP INDEX IF EXISTS ix_kothcontrol_game_challenge_round;
    DROP INDEX IF EXISTS ix_kothtargets_game_challenge;
"#;

const DOWN_SQL: &str = r#"
    DROP INDEX IF EXISTS ix_teammembers_user_team;
    DROP INDEX IF EXISTS ux_teammembers_team_user;

    CREATE INDEX IF NOT EXISTS ix_adflags_round_service
      ON "AdFlags"(round_id, team_service_id);
    CREATE INDEX IF NOT EXISTS ix_adrounds_game_number
      ON "AdRounds"(game_id, number);
    CREATE INDEX IF NOT EXISTS ix_kothcontrol_game_challenge_round
      ON "KothControlResults"(game_id, challenge_id, ad_round_id);
    CREATE INDEX IF NOT EXISTS ix_kothtargets_game_challenge
      ON "KothTargets"(game_id, challenge_id);
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
    use super::UP_SQL;

    #[test]
    fn adds_roster_indexes_and_drops_only_the_redundant_indexes() {
        assert!(UP_SQL.contains("LOCK TABLE \"TeamMembers\" IN SHARE ROW EXCLUSIVE MODE"));
        assert!(UP_SQL.contains("DELETE FROM \"TeamMembers\" duplicate"));
        assert!(UP_SQL.contains("duplicate.id > canonical.id"));
        assert!(UP_SQL.contains(
            "CREATE UNIQUE INDEX IF NOT EXISTS ux_teammembers_team_user\n      ON \"TeamMembers\"(team_id, user_id)"
        ));
        assert!(UP_SQL.contains(
            "CREATE INDEX IF NOT EXISTS ix_teammembers_user_team\n      ON \"TeamMembers\"(user_id, team_id)"
        ));

        let dropped: Vec<_> = UP_SQL
            .lines()
            .filter_map(|line| {
                line.trim()
                    .strip_prefix("DROP INDEX IF EXISTS ")
                    .map(|name| name.trim_end_matches(';'))
            })
            .collect();
        assert_eq!(
            dropped,
            [
                "ix_adflags_round_service",
                "ix_adrounds_game_number",
                "ix_kothcontrol_game_challenge_round",
                "ix_kothtargets_game_challenge",
            ]
        );
    }
}
