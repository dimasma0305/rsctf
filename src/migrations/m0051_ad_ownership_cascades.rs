//! Repair legacy A&D orphans and make the complete evidence graph game-owned.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                -- The initial entity-derived schema did not declare A&D foreign
                -- keys. Quiesce its writers while legacy rows are repaired and
                -- the durable ownership edges are installed.
                LOCK TABLE
                  "AdFlagDeliveryResults", "AdAttacks", "AdCheckResults", "AdFlags",
                  "AdTeamServices", "AdRounds", "AdVpnPeers", "AdTeamApiTokens",
                  "AdSshKeys", "AdEpochServiceRollups", "AdEpochTeamRollups"
                  IN SHARE ROW EXCLUSIVE MODE;

                DELETE FROM "AdFlagDeliveryResults" delivery
                 WHERE NOT EXISTS (
                   SELECT 1
                     FROM "AdFlags" flag
                     JOIN "AdRounds" round ON round.id = flag.round_id
                     JOIN "Games" game ON game.id = round.game_id
                     JOIN "AdTeamServices" service
                       ON service.id = flag.team_service_id
                      AND service.game_id = round.game_id
                     JOIN "Participations" participation
                       ON participation.id = service.participation_id
                      AND participation.game_id = service.game_id
                     JOIN "GameChallenges" challenge
                       ON challenge.id = service.challenge_id
                      AND challenge.game_id = service.game_id
                    WHERE flag.round_id = delivery.round_id
                      AND flag.team_service_id = delivery.team_service_id
                 );

                DELETE FROM "AdAttacks" attack
                 WHERE NOT EXISTS (
                   SELECT 1
                     FROM "AdRounds" submitted_round
                     JOIN "Games" game ON game.id = submitted_round.game_id
                     JOIN "Participations" attacker
                       ON attacker.id = attack.attacker_participation_id
                      AND attacker.game_id = submitted_round.game_id
                     JOIN "AdTeamServices" victim
                       ON victim.id = attack.victim_team_service_id
                      AND victim.game_id = submitted_round.game_id
                     JOIN "Participations" victim_participation
                       ON victim_participation.id = victim.participation_id
                      AND victim_participation.game_id = victim.game_id
                     JOIN "GameChallenges" challenge
                       ON challenge.id = victim.challenge_id
                      AND challenge.game_id = victim.game_id
                     JOIN "AdFlags" flag
                       ON flag.id = attack.flag_id
                      AND flag.team_service_id = victim.id
                     JOIN "AdRounds" planted_round
                       ON planted_round.id = flag.round_id
                      AND planted_round.game_id = submitted_round.game_id
                    WHERE submitted_round.id = attack.round_id
                 );

                DELETE FROM "AdCheckResults" result
                 WHERE NOT EXISTS (
                   SELECT 1
                     FROM "AdRounds" round
                     JOIN "Games" game ON game.id = round.game_id
                     JOIN "AdTeamServices" service
                       ON service.id = result.team_service_id
                      AND service.game_id = round.game_id
                     JOIN "Participations" participation
                       ON participation.id = service.participation_id
                      AND participation.game_id = service.game_id
                     JOIN "GameChallenges" challenge
                       ON challenge.id = service.challenge_id
                      AND challenge.game_id = service.game_id
                     JOIN "AdFlags" flag
                       ON flag.round_id = round.id
                      AND flag.team_service_id = service.id
                    WHERE round.id = result.round_id
                 );

                DELETE FROM "AdFlags" flag
                 WHERE NOT EXISTS (
                   SELECT 1
                     FROM "AdRounds" round
                     JOIN "Games" game ON game.id = round.game_id
                     JOIN "AdTeamServices" service
                       ON service.id = flag.team_service_id
                      AND service.game_id = round.game_id
                     JOIN "Participations" participation
                       ON participation.id = service.participation_id
                      AND participation.game_id = service.game_id
                     JOIN "GameChallenges" challenge
                       ON challenge.id = service.challenge_id
                      AND challenge.game_id = service.game_id
                    WHERE round.id = flag.round_id
                 );

                DELETE FROM "AdTeamServices" service
                 WHERE NOT EXISTS (
                   SELECT 1
                     FROM "Games" game
                     JOIN "Participations" participation
                       ON participation.game_id = game.id
                      AND participation.id = service.participation_id
                     JOIN "GameChallenges" challenge
                       ON challenge.game_id = game.id
                      AND challenge.id = service.challenge_id
                    WHERE game.id = service.game_id
                 );
                DELETE FROM "AdRounds" round
                 WHERE NOT EXISTS (SELECT 1 FROM "Games" game WHERE game.id = round.game_id);
                DELETE FROM "AdVpnPeers" peer
                 WHERE NOT EXISTS (
                   SELECT 1 FROM "Participations" participation
                    WHERE participation.id = peer.participation_id
                      AND participation.game_id = peer.game_id
                 );
                DELETE FROM "AdTeamApiTokens" token
                 WHERE NOT EXISTS (
                   SELECT 1 FROM "Participations" participation
                    WHERE participation.id = token.participation_id
                 );
                DELETE FROM "AdSshKeys" ssh_key
                 WHERE NOT EXISTS (
                   SELECT 1 FROM "Participations" participation
                    WHERE participation.id = ssh_key.participation_id
                 );
                DELETE FROM "AdEpochServiceRollups" rollup
                 WHERE NOT EXISTS (
                   SELECT 1
                     FROM "AdEpochRollups" epoch
                     JOIN "Participations" participation
                       ON participation.id = rollup.participation_id
                      AND participation.game_id = rollup.game_id
                     JOIN "GameChallenges" challenge
                       ON challenge.id = rollup.challenge_id
                      AND challenge.game_id = rollup.game_id
                    WHERE epoch.game_id = rollup.game_id
                      AND epoch.epoch = rollup.epoch
                 );
                DELETE FROM "AdEpochTeamRollups" rollup
                 WHERE NOT EXISTS (
                   SELECT 1
                     FROM "AdEpochRollups" epoch
                     JOIN "Participations" participation
                       ON participation.id = rollup.participation_id
                      AND participation.game_id = rollup.game_id
                    WHERE epoch.game_id = rollup.game_id
                      AND epoch.epoch = rollup.epoch
                 );

                CREATE INDEX IF NOT EXISTS ix_adteamservices_game_challenge
                  ON "AdTeamServices"(game_id, challenge_id);
                CREATE INDEX IF NOT EXISTS ix_adflags_team_service
                  ON "AdFlags"(team_service_id);
                CREATE INDEX IF NOT EXISTS ix_ad_flag_delivery_service
                  ON "AdFlagDeliveryResults"(team_service_id);
                CREATE INDEX IF NOT EXISTS ix_ad_epoch_service_rollups_game_challenge
                  ON "AdEpochServiceRollups"(game_id, challenge_id);

                DO $$
                BEGIN
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_ad_rounds_game' AND conrelid = '"AdRounds"'::regclass) THEN
                    ALTER TABLE "AdRounds" ADD CONSTRAINT fk_ad_rounds_game
                      FOREIGN KEY (game_id) REFERENCES "Games"(id) ON DELETE CASCADE NOT VALID;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_ad_team_services_game' AND conrelid = '"AdTeamServices"'::regclass) THEN
                    ALTER TABLE "AdTeamServices" ADD CONSTRAINT fk_ad_team_services_game
                      FOREIGN KEY (game_id) REFERENCES "Games"(id) ON DELETE CASCADE NOT VALID;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_ad_team_services_participation_game' AND conrelid = '"AdTeamServices"'::regclass) THEN
                    ALTER TABLE "AdTeamServices" ADD CONSTRAINT fk_ad_team_services_participation_game
                      FOREIGN KEY (game_id, participation_id)
                      REFERENCES "Participations"(game_id, id) ON DELETE CASCADE NOT VALID;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_ad_team_services_challenge_game' AND conrelid = '"AdTeamServices"'::regclass) THEN
                    ALTER TABLE "AdTeamServices" ADD CONSTRAINT fk_ad_team_services_challenge_game
                      FOREIGN KEY (game_id, challenge_id)
                      REFERENCES "GameChallenges"(game_id, id) ON DELETE CASCADE NOT VALID;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_ad_flags_round' AND conrelid = '"AdFlags"'::regclass) THEN
                    ALTER TABLE "AdFlags" ADD CONSTRAINT fk_ad_flags_round
                      FOREIGN KEY (round_id) REFERENCES "AdRounds"(id) ON DELETE CASCADE NOT VALID;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_ad_flags_team_service' AND conrelid = '"AdFlags"'::regclass) THEN
                    ALTER TABLE "AdFlags" ADD CONSTRAINT fk_ad_flags_team_service
                      FOREIGN KEY (team_service_id) REFERENCES "AdTeamServices"(id) ON DELETE CASCADE NOT VALID;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_ad_check_results_round' AND conrelid = '"AdCheckResults"'::regclass) THEN
                    ALTER TABLE "AdCheckResults" ADD CONSTRAINT fk_ad_check_results_round
                      FOREIGN KEY (round_id) REFERENCES "AdRounds"(id) ON DELETE CASCADE NOT VALID;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_ad_check_results_team_service' AND conrelid = '"AdCheckResults"'::regclass) THEN
                    ALTER TABLE "AdCheckResults" ADD CONSTRAINT fk_ad_check_results_team_service
                      FOREIGN KEY (team_service_id) REFERENCES "AdTeamServices"(id) ON DELETE CASCADE NOT VALID;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_ad_check_results_flag' AND conrelid = '"AdCheckResults"'::regclass) THEN
                    ALTER TABLE "AdCheckResults" ADD CONSTRAINT fk_ad_check_results_flag
                      FOREIGN KEY (round_id, team_service_id)
                      REFERENCES "AdFlags"(round_id, team_service_id) ON DELETE CASCADE NOT VALID;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_ad_attacks_round' AND conrelid = '"AdAttacks"'::regclass) THEN
                    ALTER TABLE "AdAttacks" ADD CONSTRAINT fk_ad_attacks_round
                      FOREIGN KEY (round_id) REFERENCES "AdRounds"(id) ON DELETE CASCADE NOT VALID;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_ad_attacks_attacker' AND conrelid = '"AdAttacks"'::regclass) THEN
                    ALTER TABLE "AdAttacks" ADD CONSTRAINT fk_ad_attacks_attacker
                      FOREIGN KEY (attacker_participation_id) REFERENCES "Participations"(id) ON DELETE CASCADE NOT VALID;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_ad_attacks_victim_service' AND conrelid = '"AdAttacks"'::regclass) THEN
                    ALTER TABLE "AdAttacks" ADD CONSTRAINT fk_ad_attacks_victim_service
                      FOREIGN KEY (victim_team_service_id) REFERENCES "AdTeamServices"(id) ON DELETE CASCADE NOT VALID;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_ad_attacks_flag' AND conrelid = '"AdAttacks"'::regclass) THEN
                    ALTER TABLE "AdAttacks" ADD CONSTRAINT fk_ad_attacks_flag
                      FOREIGN KEY (flag_id) REFERENCES "AdFlags"(id) ON DELETE CASCADE NOT VALID;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_ad_vpn_peers_game' AND conrelid = '"AdVpnPeers"'::regclass) THEN
                    ALTER TABLE "AdVpnPeers" ADD CONSTRAINT fk_ad_vpn_peers_game
                      FOREIGN KEY (game_id) REFERENCES "Games"(id) ON DELETE CASCADE NOT VALID;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_ad_vpn_peers_participation_game' AND conrelid = '"AdVpnPeers"'::regclass) THEN
                    ALTER TABLE "AdVpnPeers" ADD CONSTRAINT fk_ad_vpn_peers_participation_game
                      FOREIGN KEY (game_id, participation_id)
                      REFERENCES "Participations"(game_id, id) ON DELETE CASCADE NOT VALID;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_ad_team_api_tokens_participation' AND conrelid = '"AdTeamApiTokens"'::regclass) THEN
                    ALTER TABLE "AdTeamApiTokens" ADD CONSTRAINT fk_ad_team_api_tokens_participation
                      FOREIGN KEY (participation_id) REFERENCES "Participations"(id) ON DELETE CASCADE NOT VALID;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_ad_ssh_keys_participation' AND conrelid = '"AdSshKeys"'::regclass) THEN
                    ALTER TABLE "AdSshKeys" ADD CONSTRAINT fk_ad_ssh_keys_participation
                      FOREIGN KEY (participation_id) REFERENCES "Participations"(id) ON DELETE CASCADE NOT VALID;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_ad_epoch_service_participation_game' AND conrelid = '"AdEpochServiceRollups"'::regclass) THEN
                    ALTER TABLE "AdEpochServiceRollups" ADD CONSTRAINT fk_ad_epoch_service_participation_game
                      FOREIGN KEY (game_id, participation_id)
                      REFERENCES "Participations"(game_id, id) ON DELETE CASCADE NOT VALID;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_ad_epoch_service_challenge_game' AND conrelid = '"AdEpochServiceRollups"'::regclass) THEN
                    ALTER TABLE "AdEpochServiceRollups" ADD CONSTRAINT fk_ad_epoch_service_challenge_game
                      FOREIGN KEY (game_id, challenge_id)
                      REFERENCES "GameChallenges"(game_id, id) ON DELETE CASCADE NOT VALID;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_ad_epoch_team_participation_game' AND conrelid = '"AdEpochTeamRollups"'::regclass) THEN
                    ALTER TABLE "AdEpochTeamRollups" ADD CONSTRAINT fk_ad_epoch_team_participation_game
                      FOREIGN KEY (game_id, participation_id)
                      REFERENCES "Participations"(game_id, id) ON DELETE CASCADE NOT VALID;
                  END IF;
                END
                $$;

                ALTER TABLE "AdRounds" VALIDATE CONSTRAINT fk_ad_rounds_game;
                ALTER TABLE "AdTeamServices" VALIDATE CONSTRAINT fk_ad_team_services_game;
                ALTER TABLE "AdTeamServices" VALIDATE CONSTRAINT fk_ad_team_services_participation_game;
                ALTER TABLE "AdTeamServices" VALIDATE CONSTRAINT fk_ad_team_services_challenge_game;
                ALTER TABLE "AdFlags" VALIDATE CONSTRAINT fk_ad_flags_round;
                ALTER TABLE "AdFlags" VALIDATE CONSTRAINT fk_ad_flags_team_service;
                ALTER TABLE "AdCheckResults" VALIDATE CONSTRAINT fk_ad_check_results_round;
                ALTER TABLE "AdCheckResults" VALIDATE CONSTRAINT fk_ad_check_results_team_service;
                ALTER TABLE "AdCheckResults" VALIDATE CONSTRAINT fk_ad_check_results_flag;
                ALTER TABLE "AdAttacks" VALIDATE CONSTRAINT fk_ad_attacks_round;
                ALTER TABLE "AdAttacks" VALIDATE CONSTRAINT fk_ad_attacks_attacker;
                ALTER TABLE "AdAttacks" VALIDATE CONSTRAINT fk_ad_attacks_victim_service;
                ALTER TABLE "AdAttacks" VALIDATE CONSTRAINT fk_ad_attacks_flag;
                ALTER TABLE "AdVpnPeers" VALIDATE CONSTRAINT fk_ad_vpn_peers_game;
                ALTER TABLE "AdVpnPeers" VALIDATE CONSTRAINT fk_ad_vpn_peers_participation_game;
                ALTER TABLE "AdTeamApiTokens" VALIDATE CONSTRAINT fk_ad_team_api_tokens_participation;
                ALTER TABLE "AdSshKeys" VALIDATE CONSTRAINT fk_ad_ssh_keys_participation;
                ALTER TABLE "AdEpochServiceRollups" VALIDATE CONSTRAINT fk_ad_epoch_service_participation_game;
                ALTER TABLE "AdEpochServiceRollups" VALIDATE CONSTRAINT fk_ad_epoch_service_challenge_game;
                ALTER TABLE "AdEpochTeamRollups" VALIDATE CONSTRAINT fk_ad_epoch_team_participation_game;

                -- The orphan purge can radically change table cardinality. Refresh
                -- planner statistics in the same migration so the first live board
                -- query does not inherit the pre-cleanup row estimates.
                ANALYZE "AdCheckResults", "AdFlags", "AdAttacks", "AdRounds",
                        "AdTeamServices", "AdTeamApiTokens";
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE "AdEpochTeamRollups" DROP CONSTRAINT IF EXISTS fk_ad_epoch_team_participation_game;
                ALTER TABLE "AdEpochServiceRollups" DROP CONSTRAINT IF EXISTS fk_ad_epoch_service_challenge_game;
                ALTER TABLE "AdEpochServiceRollups" DROP CONSTRAINT IF EXISTS fk_ad_epoch_service_participation_game;
                ALTER TABLE "AdSshKeys" DROP CONSTRAINT IF EXISTS fk_ad_ssh_keys_participation;
                ALTER TABLE "AdTeamApiTokens" DROP CONSTRAINT IF EXISTS fk_ad_team_api_tokens_participation;
                ALTER TABLE "AdVpnPeers" DROP CONSTRAINT IF EXISTS fk_ad_vpn_peers_participation_game;
                ALTER TABLE "AdVpnPeers" DROP CONSTRAINT IF EXISTS fk_ad_vpn_peers_game;
                ALTER TABLE "AdAttacks" DROP CONSTRAINT IF EXISTS fk_ad_attacks_flag;
                ALTER TABLE "AdAttacks" DROP CONSTRAINT IF EXISTS fk_ad_attacks_victim_service;
                ALTER TABLE "AdAttacks" DROP CONSTRAINT IF EXISTS fk_ad_attacks_attacker;
                ALTER TABLE "AdAttacks" DROP CONSTRAINT IF EXISTS fk_ad_attacks_round;
                ALTER TABLE "AdCheckResults" DROP CONSTRAINT IF EXISTS fk_ad_check_results_flag;
                ALTER TABLE "AdCheckResults" DROP CONSTRAINT IF EXISTS fk_ad_check_results_team_service;
                ALTER TABLE "AdCheckResults" DROP CONSTRAINT IF EXISTS fk_ad_check_results_round;
                ALTER TABLE "AdFlags" DROP CONSTRAINT IF EXISTS fk_ad_flags_team_service;
                ALTER TABLE "AdFlags" DROP CONSTRAINT IF EXISTS fk_ad_flags_round;
                ALTER TABLE "AdTeamServices" DROP CONSTRAINT IF EXISTS fk_ad_team_services_challenge_game;
                ALTER TABLE "AdTeamServices" DROP CONSTRAINT IF EXISTS fk_ad_team_services_participation_game;
                ALTER TABLE "AdTeamServices" DROP CONSTRAINT IF EXISTS fk_ad_team_services_game;
                ALTER TABLE "AdRounds" DROP CONSTRAINT IF EXISTS fk_ad_rounds_game;
                DROP INDEX IF EXISTS ix_ad_epoch_service_rollups_game_challenge;
                DROP INDEX IF EXISTS ix_ad_flag_delivery_service;
                DROP INDEX IF EXISTS ix_adflags_team_service;
                DROP INDEX IF EXISTS ix_adteamservices_game_challenge;
                "#,
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sqlx::Connection;

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn ownership_constraints_are_validated_cascades() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to the migrated PostgreSQL database");
        let mut connection = sqlx::PgConnection::connect(&database_url).await.unwrap();
        let constraints: Vec<(String, bool, String)> = sqlx::query_as(
            r#"SELECT conname, convalidated, pg_get_constraintdef(oid)
                 FROM pg_constraint
                WHERE conname = ANY($1)
                ORDER BY conname"#,
        )
        .bind([
            "fk_ad_rounds_game",
            "fk_ad_team_services_game",
            "fk_ad_flags_round",
            "fk_ad_check_results_round",
            "fk_ad_check_results_flag",
            "fk_ad_attacks_round",
            "fk_ad_vpn_peers_game",
            "fk_ad_team_api_tokens_participation",
            "fk_ad_ssh_keys_participation",
        ])
        .fetch_all(&mut connection)
        .await
        .unwrap();
        assert_eq!(constraints.len(), 9);
        assert!(constraints
            .iter()
            .all(|(_, validated, definition)| *validated
                && definition.ends_with("ON DELETE CASCADE")));

        let orphan_checks: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*)
                 FROM "AdCheckResults" result
                 LEFT JOIN "AdRounds" round ON round.id = result.round_id
                 LEFT JOIN "AdTeamServices" service ON service.id = result.team_service_id
                WHERE round.id IS NULL OR service.id IS NULL"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(orphan_checks, 0);
    }
}
