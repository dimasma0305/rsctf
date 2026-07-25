//! Exact `(participant VPN address, hill endpoint)` champion-cooldown policy.

use std::collections::{BTreeMap, HashSet};
use std::net::Ipv4Addr;
use std::process::Command;

use ipnet::Ipv4Net;
use sea_orm::DatabaseConnection;

use super::firewall::CooldownBlock;
use super::{peer_address_allowed, vpn_target};
use crate::services::ad::roster::shared_credential_team_predicate_sql;
use crate::utils::enums::{ParticipationStatus, Role};
use crate::utils::error::{AppError, AppResult};

#[derive(Debug, sqlx::FromRow)]
struct RawCooldownBlock {
    cycle_id: i64,
    participation_id: i32,
    game_id: i32,
    peer: Option<String>,
    host: Option<String>,
    port: Option<i32>,
    container_id: Option<String>,
    replacement_container_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RequiredCooldownBlock {
    pub(super) cycle_id: i64,
    pub(super) participation_id: i32,
    pub(super) game_id: i32,
    pub(super) block: CooldownBlock,
}

fn command_error(program: &str, args: &[&str], output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    format!(
        "{} {} failed with {}{}",
        program,
        args.join(" "),
        output.status,
        if stderr.is_empty() {
            String::new()
        } else {
            format!(": {stderr}")
        }
    )
}

fn referenced_cooldown_set(rules: &str, game_id: i32) -> Option<String> {
    let prefix = ["rsv_c_", &game_id.unsigned_abs().to_string(), "_"].concat();
    let names = rules
        .lines()
        .filter_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            fields.windows(3).find_map(|window| {
                (window[0] == "--match-set"
                    && window[1].starts_with(&prefix)
                    && window[2] == "src,dst,dst")
                    .then(|| window[1].to_string())
            })
        })
        .collect::<HashSet<_>>();
    (names.len() == 1)
        .then(|| names.into_iter().next())
        .flatten()
}

fn chain_rules(chain: &str) -> Result<Option<String>, String> {
    let args = ["-t", "filter", "-S", chain];
    let output = Command::new("iptables")
        .args(["-w", "5"])
        .args(args)
        .output()
        .map_err(|error| format!("inspect VPN firewall chain {chain}: {error}"))?;
    if output.status.success() {
        Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
    } else if output.status.code() == Some(1) {
        Ok(None)
    } else {
        Err(command_error("iptables", &args, &output))
    }
}

/// Verify exact cooldown members in the live generation referenced by both
/// traffic paths. A successful policy build alone is insufficient evidence if
/// a later external firewall mutation removed a set or member.
pub(super) fn blocks_installed(required: &[(i32, CooldownBlock)]) -> Result<bool, String> {
    if required.is_empty() {
        return Ok(true);
    }
    let Some(forward_rules) = chain_rules(super::firewall::FORWARD_CHAIN)? else {
        return Ok(false);
    };
    let Some(input_rules) = chain_rules(super::firewall::INPUT_CHAIN)? else {
        return Ok(false);
    };
    let mut by_game = BTreeMap::<i32, Vec<&CooldownBlock>>::new();
    for (game_id, block) in required {
        by_game.entry(*game_id).or_default().push(block);
    }
    for (game_id, blocks) in by_game {
        let Some(set_name) = referenced_cooldown_set(&forward_rules, game_id) else {
            return Ok(false);
        };
        if referenced_cooldown_set(&input_rules, game_id).as_deref() != Some(set_name.as_str()) {
            return Ok(false);
        }
        let args = ["save", set_name.as_str()];
        let output = Command::new("ipset")
            .args(args)
            .output()
            .map_err(|error| format!("inspect cooldown ipset {set_name}: {error}"))?;
        if !output.status.success() {
            return if output.status.code() == Some(1) {
                Ok(false)
            } else {
                Err(command_error("ipset", &args, &output))
            };
        }
        let members = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.strip_prefix(&format!("add {set_name} ")))
            .map(str::to_string)
            .collect::<HashSet<_>>();
        if blocks.iter().any(|block| {
            !members.contains(&format!(
                "{},tcp:{},{}",
                block.peer, block.target.port, block.target.address
            ))
        }) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn validate_required_blocks(
    rows: Vec<RawCooldownBlock>,
    client_network: &Ipv4Net,
    service_networks: &[Ipv4Net],
) -> AppResult<Vec<RequiredCooldownBlock>> {
    rows.into_iter()
        .map(|row| {
            let peer_text = row.peer.as_deref().ok_or_else(|| {
                AppError::conflict(format!(
                    "KotH cooldown participant {} in cycle {} has no active VPN peer",
                    row.participation_id, row.cycle_id
                ))
            })?;
            let peer = peer_text.parse::<Ipv4Addr>().ok().filter(|_| {
                peer_address_allowed(peer_text, client_network, service_networks)
            });
            let peer = peer.ok_or_else(|| {
                AppError::conflict(format!(
                    "KotH cooldown participant {} in cycle {} has an invalid VPN address",
                    row.participation_id, row.cycle_id
                ))
            })?;
            if row.container_id.as_deref() != row.replacement_container_id.as_deref()
                || row.container_id.as_deref().is_none_or(str::is_empty)
            {
                return Err(AppError::conflict(format!(
                    "KotH cooldown cycle {} is not bound to its exact replacement container",
                    row.cycle_id
                )));
            }
            let host = row.host.as_deref().ok_or_else(|| {
                AppError::conflict(format!(
                    "KotH cooldown cycle {} has no enforceable target address",
                    row.cycle_id
                ))
            })?;
            let target = row
                .port
                .and_then(|port| vpn_target(host, port, client_network, service_networks))
                .ok_or_else(|| {
                    AppError::conflict(format!(
                        "KotH cooldown cycle {} targets an address outside the managed VPN networks",
                        row.cycle_id
                    ))
                })?;
            Ok(RequiredCooldownBlock {
                cycle_id: row.cycle_id,
                participation_id: row.participation_id,
                game_id: row.game_id,
                block: CooldownBlock { peer, target },
            })
        })
        .collect()
}

const LOAD_REQUIRED_BLOCKS_SQL: &str = concat!(
    r#"SELECT cycle.id AS cycle_id, cooldown.participation_id,
              cycle.game_id, peer.address AS peer,
              target.host, target.port, target.container_id,
              cycle.replacement_container_id
         FROM "KothCycleCooldowns" cooldown
         JOIN "KothCrownCycles" cycle ON cycle.id = cooldown.cycle_id
         JOIN "Games" game ON game.id = cycle.game_id
         JOIN "Participations" participation
           ON participation.id = cooldown.participation_id
          AND participation.game_id = cycle.game_id
         JOIN "Teams" team ON team.id = participation.team_id
         LEFT JOIN "AdVpnPeers" peer
           ON peer.game_id = cycle.game_id
          AND peer.participation_id = cooldown.participation_id
         LEFT JOIN "KothTargets" target
           ON target.game_id = cycle.game_id
          AND target.challenge_id = cycle.challenge_id
         JOIN LATERAL (
           SELECT MAX(number) AS number FROM "AdRounds"
            WHERE game_id = cycle.game_id
         ) current_round ON TRUE
        WHERE cycle.phase IN ('FirewallPending','Active')
          AND cooldown.network_released_at IS NULL
          AND current_round.number BETWEEN cooldown.starts_round
                                       AND cooldown.expires_after_round
          AND game.deletion_pending = FALSE
          AND game.start_time_utc <= clock_timestamp()
          AND clock_timestamp() <= game.end_time_utc
          AND participation.status = $3
          AND "#,
    shared_credential_team_predicate_sql!("team", "$2"),
    r#"
          AND ($1::bigint IS NULL OR cycle.id = $1)
        ORDER BY cycle.id, cooldown.participation_id"#
);

async fn load_required_blocks_from_pool(
    pool: &sqlx::PgPool,
    cycle_id: Option<i64>,
    client_network: &Ipv4Net,
    service_networks: &[Ipv4Net],
) -> AppResult<Vec<RequiredCooldownBlock>> {
    let rows = sqlx::query_as::<_, RawCooldownBlock>(LOAD_REQUIRED_BLOCKS_SQL)
        .bind(cycle_id)
        .bind(Role::Banned as i16)
        .bind(ParticipationStatus::Accepted as i16)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    validate_required_blocks(rows, client_network, service_networks)
}

async fn load_required_blocks(
    db: &DatabaseConnection,
    cycle_id: Option<i64>,
    client_network: &Ipv4Net,
    service_networks: &[Ipv4Net],
) -> AppResult<Vec<RequiredCooldownBlock>> {
    load_required_blocks_from_pool(
        db.get_postgres_connection_pool(),
        cycle_id,
        client_network,
        service_networks,
    )
    .await
}

pub(super) async fn load_active_blocks(
    db: &DatabaseConnection,
    client_network: &Ipv4Net,
    service_networks: &[Ipv4Net],
) -> AppResult<Vec<(i32, CooldownBlock)>> {
    Ok(
        load_required_blocks(db, None, client_network, service_networks)
            .await?
            .into_iter()
            .map(|required| (required.game_id, required.block))
            .collect(),
    )
}

pub(super) async fn load_cycle_blocks(
    db: &DatabaseConnection,
    cycle_id: i64,
    client_network: &Ipv4Net,
    service_networks: &[Ipv4Net],
) -> AppResult<Vec<RequiredCooldownBlock>> {
    load_required_blocks(db, Some(cycle_id), client_network, service_networks).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;
    use uuid::Uuid;

    fn networks() -> (Ipv4Net, Vec<Ipv4Net>) {
        (
            "10.13.37.0/24".parse().unwrap(),
            vec!["10.13.40.0/24".parse().unwrap()],
        )
    }

    fn row() -> RawCooldownBlock {
        RawCooldownBlock {
            cycle_id: 11,
            participation_id: 7,
            game_id: 3,
            peer: Some("10.13.37.7".to_string()),
            host: Some("10.13.40.9".to_string()),
            port: Some(8080),
            container_id: Some("replacement-11".to_string()),
            replacement_container_id: Some("replacement-11".to_string()),
        }
    }

    #[test]
    fn every_required_tuple_must_be_exact_and_routeable() {
        let (client, services) = networks();
        let blocks = validate_required_blocks(vec![row()], &client, &services).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0].block.peer,
            "10.13.37.7".parse::<Ipv4Addr>().unwrap()
        );
        assert_eq!(
            blocks[0].block.target.address,
            "10.13.40.9".parse::<Ipv4Addr>().unwrap()
        );
    }

    #[test]
    fn missing_peer_is_not_silently_filtered_out() {
        let (client, services) = networks();
        let mut missing = row();
        missing.peer = None;
        assert!(validate_required_blocks(vec![missing], &client, &services).is_err());
    }

    #[test]
    fn cooldown_query_matches_peer_eligibility_boundaries() {
        assert!(LOAD_REQUIRED_BLOCKS_SQL.contains("game.deletion_pending = FALSE"));
        assert!(LOAD_REQUIRED_BLOCKS_SQL.contains("game.start_time_utc <= clock_timestamp()"));
        assert!(LOAD_REQUIRED_BLOCKS_SQL.contains("clock_timestamp() <= game.end_time_utc"));
        assert!(LOAD_REQUIRED_BLOCKS_SQL.contains("participation.status = $3"));
        assert!(LOAD_REQUIRED_BLOCKS_SQL.contains("NOT team.deletion_pending"));
        assert!(LOAD_REQUIRED_BLOCKS_SQL.contains("account.role = $2"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn ended_or_ineligible_cooldown_cannot_block_an_unrelated_event() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("vpn_cooldown_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Games" (
                id INTEGER PRIMARY KEY,
                start_time_utc TIMESTAMPTZ NOT NULL,
                end_time_utc TIMESTAMPTZ NOT NULL,
                deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "AspNetUsers" (
                id UUID PRIMARY KEY,
                role SMALLINT NOT NULL
            );
            CREATE TABLE "Teams" (
                id INTEGER PRIMARY KEY,
                captain_id UUID NOT NULL,
                deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "TeamMembers" (
                team_id INTEGER NOT NULL,
                user_id UUID NOT NULL
            );
            CREATE TABLE "Participations" (
                id INTEGER PRIMARY KEY,
                game_id INTEGER NOT NULL,
                team_id INTEGER NOT NULL,
                status SMALLINT NOT NULL
            );
            CREATE TABLE "KothCrownCycles" (
                id BIGINT PRIMARY KEY,
                game_id INTEGER NOT NULL,
                challenge_id INTEGER NOT NULL,
                phase TEXT NOT NULL,
                replacement_container_id TEXT
            );
            CREATE TABLE "KothCycleCooldowns" (
                cycle_id BIGINT NOT NULL,
                participation_id INTEGER NOT NULL,
                starts_round INTEGER NOT NULL,
                expires_after_round INTEGER NOT NULL,
                network_released_at TIMESTAMPTZ
            );
            CREATE TABLE "AdVpnPeers" (
                game_id INTEGER NOT NULL,
                participation_id INTEGER NOT NULL,
                address TEXT NOT NULL
            );
            CREATE TABLE "KothTargets" (
                game_id INTEGER NOT NULL,
                challenge_id INTEGER NOT NULL,
                host TEXT NOT NULL,
                port INTEGER NOT NULL,
                container_id TEXT
            );
            CREATE TABLE "AdRounds" (
                game_id INTEGER NOT NULL,
                number INTEGER NOT NULL
            );

            INSERT INTO "Games" VALUES
                (1, now() - interval '2 hours', now() - interval '1 hour', FALSE),
                (2, now() - interval '1 hour', now() + interval '1 hour', FALSE),
                (3, now() - interval '1 hour', now() + interval '1 hour', FALSE);
            INSERT INTO "AspNetUsers" VALUES
                ('00000000-0000-0000-0000-000000000001', 1),
                ('00000000-0000-0000-0000-000000000002', 1),
                ('00000000-0000-0000-0000-000000000003', 1);
            INSERT INTO "Teams" VALUES
                (1, '00000000-0000-0000-0000-000000000001', FALSE),
                (2, '00000000-0000-0000-0000-000000000002', FALSE),
                (3, '00000000-0000-0000-0000-000000000003', FALSE);
            INSERT INTO "Participations" VALUES
                (11, 1, 1, 1),
                (22, 2, 2, 1),
                (33, 3, 3, 0);
            INSERT INTO "KothCrownCycles" VALUES
                (101, 1, 10, 'Active', 'replacement-ended'),
                (202, 2, 20, 'Active', 'replacement-live'),
                (303, 3, 30, 'Active', 'replacement-rejected');
            INSERT INTO "KothCycleCooldowns" VALUES
                (101, 11, 4, 6, NULL),
                (202, 22, 4, 6, NULL),
                (303, 33, 4, 6, NULL);
            INSERT INTO "AdVpnPeers" VALUES
                (2, 22, '10.13.37.22');
            INSERT INTO "KothTargets" VALUES
                (1, 10, '10.13.40.10', 8080, 'replacement-ended'),
                (2, 20, '10.13.40.20', 8080, 'replacement-live'),
                (3, 30, '10.13.40.30', 8080, 'replacement-rejected');
            INSERT INTO "AdRounds" VALUES (1, 5), (2, 5), (3, 5);
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let (client, services) = networks();
        let blocks = load_required_blocks_from_pool(&pool, None, &client, &services)
            .await
            .unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].game_id, 2);
        assert_eq!(blocks[0].participation_id, 22);

        sqlx::query(r#"DELETE FROM "AdVpnPeers" WHERE game_id = 2"#)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            load_required_blocks_from_pool(&pool, None, &client, &services)
                .await
                .is_err(),
            "an active eligible participant without a peer must still fail closed"
        );

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }

    #[test]
    fn external_or_stale_target_is_not_claimed_as_enforced() {
        let (client, services) = networks();
        let mut external = row();
        external.host = Some("203.0.113.10".to_string());
        assert!(validate_required_blocks(vec![external], &client, &services).is_err());

        let mut stale = row();
        stale.container_id = Some("old-container".to_string());
        assert!(validate_required_blocks(vec![stale], &client, &services).is_err());
    }

    #[test]
    fn kernel_receipt_accepts_one_exact_live_generation_only() {
        let rules = "-A RSCTF_VPN_FORWARD -i wg0 -p tcp -m set --match-set rsv_c_10_deadbeef src,dst,dst -j DROP\n";
        assert_eq!(
            referenced_cooldown_set(rules, 10),
            Some("rsv_c_10_deadbeef".to_string())
        );
        assert_eq!(referenced_cooldown_set(rules, 11), None);
        let ambiguous = format!(
            "{rules}-A RSCTF_VPN_FORWARD -m set --match-set rsv_c_10_cafebabe src,dst,dst -j DROP\n"
        );
        assert_eq!(referenced_cooldown_set(&ambiguous, 10), None);
    }
}
