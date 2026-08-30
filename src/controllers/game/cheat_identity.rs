//! Game-scoped identity overlap reporting.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde_json::Value as Json;

use crate::utils::error::{AppError, AppResult};

const MAX_SHARED_NETWORK_TEAMS: i64 = 4;
const MAX_IDENTITY_GROUPS: i64 = 200;
const MAX_IDENTITY_INPUT_ROWS: i64 = 2_000;
const MAX_IDENTITY_ROWS_PER_VALUE: i64 = 64;

#[derive(Debug, sqlx::FromRow)]
struct IdentityGroupRow {
    kind: String,
    value_hint: String,
    latest: DateTime<Utc>,
    team_ids: Vec<i32>,
    team_names: Vec<String>,
    edge_left_team_ids: Vec<i32>,
    edge_right_team_ids: Vec<i32>,
}

const IDENTITY_ANALYSIS_SQL: &str = r#"
    WITH recent AS MATERIALIZED (
        SELECT observation.*
          FROM "IdentityObservations" observation
          JOIN "Games" game ON game.id = observation.game_id
         WHERE observation.game_id = $1
           AND observation.team_id IS NOT NULL
           AND observation.participation_id IS NOT NULL
           AND observation.observed_at_utc >= game.start_time_utc
           AND observation.observed_at_utc < game.end_time_utc
         ORDER BY observation.observed_at_utc DESC, observation.id DESC
         LIMIT $4
    ), scoped_ranked AS (
        SELECT observation.id,
               observation.user_id,
               observation.kind,
               observation.value_hash,
               observation.value_hint,
               observation.observed_at_utc,
               observation.team_id,
               team.name AS team_name,
               ROW_NUMBER() OVER (
                   PARTITION BY observation.kind, observation.value_hash
                   ORDER BY observation.observed_at_utc DESC, observation.id DESC
               ) AS value_rank
          FROM recent observation
          JOIN "Participations" participation
            ON participation.id = observation.participation_id
           AND participation.game_id = observation.game_id
           AND participation.team_id = observation.team_id
          JOIN "Teams" team ON team.id = observation.team_id
    ), scoped AS (
        SELECT id, user_id, kind, value_hash, value_hint,
               observed_at_utc, team_id, team_name
          FROM scoped_ranked
         WHERE value_rank <= $5
    ), qualified AS (
        SELECT kind, value_hash, COUNT(DISTINCT team_id) AS team_count
          FROM scoped
         GROUP BY kind, value_hash
        HAVING COUNT(DISTINCT team_id) >= 2
           AND COUNT(DISTINCT user_id) >= 2
           AND (kind = 'Fingerprint' OR COUNT(DISTINCT team_id) <= $2)
    ), per_user AS (
        SELECT scoped.kind, scoped.value_hash, scoped.user_id,
               scoped.team_id, scoped.team_name,
               (ARRAY_AGG(scoped.value_hint
                          ORDER BY scoped.observed_at_utc DESC, scoped.id DESC))[1]
                   AS value_hint,
               MIN(scoped.observed_at_utc) AS first_observed_at,
               ARRAY_AGG(scoped.observed_at_utc
                         ORDER BY scoped.observed_at_utc, scoped.id) AS observed_times
          FROM scoped
          JOIN qualified
            ON qualified.kind = scoped.kind
           AND qualified.value_hash = scoped.value_hash
         GROUP BY scoped.kind, scoped.value_hash, scoped.user_id,
                  scoped.team_id, scoped.team_name
    ), pair_edges AS (
        SELECT left_identity.kind, left_identity.value_hash,
               left_identity.user_id AS left_user_id,
               left_identity.team_id AS left_team_id,
               left_identity.team_name AS left_team_name,
               left_identity.value_hint AS left_value_hint,
               right_identity.user_id AS right_user_id,
               right_identity.team_id AS right_team_id,
               right_identity.team_name AS right_team_name,
               right_identity.value_hint AS right_value_hint,
               edge.observed_at_utc
          FROM per_user left_identity
          JOIN per_user right_identity
            ON right_identity.kind = left_identity.kind
           AND right_identity.value_hash = left_identity.value_hash
           AND left_identity.user_id < right_identity.user_id
           AND left_identity.team_id <> right_identity.team_id
         CROSS JOIN LATERAL (
              SELECT candidate.observed_at_utc
                FROM UNNEST(
                         left_identity.observed_times
                         || right_identity.observed_times
                     ) AS candidate(observed_at_utc)
               WHERE candidate.observed_at_utc >= GREATEST(
                         left_identity.first_observed_at,
                         right_identity.first_observed_at
                     )
                 AND NOT EXISTS (
                      SELECT 1
                        FROM "AntiCheatExemptions" exemption
                       WHERE exemption.user_a = LEAST(
                                 left_identity.user_id,
                                 right_identity.user_id
                             )
                         AND exemption.user_b = GREATEST(
                                 left_identity.user_id,
                                 right_identity.user_id
                             )
                         AND exemption.kind = left_identity.kind
                         AND exemption.value_hash = left_identity.value_hash
                         AND exemption.created_at_utc <= candidate.observed_at_utc
                         AND candidate.observed_at_utc < exemption.expires_at_utc
                         AND (exemption.revoked_at_utc IS NULL
                              OR candidate.observed_at_utc < exemption.revoked_at_utc)
                 )
               ORDER BY candidate.observed_at_utc
               LIMIT 1
         ) edge
    ), edge_members AS (
        SELECT kind, value_hash, observed_at_utc,
               left_user_id AS user_id, left_team_id AS team_id,
               left_team_name AS team_name, left_value_hint AS value_hint
          FROM pair_edges
        UNION ALL
        SELECT kind, value_hash, observed_at_utc,
               right_user_id AS user_id, right_team_id AS team_id,
               right_team_name AS team_name, right_value_hint AS value_hint
          FROM pair_edges
    ), per_team AS (
        SELECT DISTINCT ON (kind, value_hash, team_id)
               kind, value_hash, value_hint, observed_at_utc,
               user_id, team_id, team_name
          FROM edge_members
         ORDER BY kind, value_hash, team_id,
                  observed_at_utc DESC, user_id
    ), grouped AS (
        SELECT kind, value_hash,
               (ARRAY_AGG(value_hint
                          ORDER BY observed_at_utc DESC, user_id))[1] AS value_hint,
               MAX(observed_at_utc) AS latest,
               ARRAY_AGG(team_id ORDER BY team_id) AS team_ids,
               ARRAY_AGG(team_name ORDER BY team_id) AS team_names,
               COUNT(*) AS team_count
          FROM per_team
         GROUP BY kind, value_hash
    ), edge_lists AS (
        SELECT kind, value_hash,
               ARRAY_AGG(left_team_id
                         ORDER BY left_team_id, right_team_id,
                                  left_user_id, right_user_id) AS edge_left_team_ids,
               ARRAY_AGG(right_team_id
                         ORDER BY left_team_id, right_team_id,
                                  left_user_id, right_user_id) AS edge_right_team_ids
          FROM pair_edges
         GROUP BY kind, value_hash
    )
    SELECT grouped.kind, grouped.value_hint, grouped.latest,
           grouped.team_ids, grouped.team_names,
           edge_lists.edge_left_team_ids, edge_lists.edge_right_team_ids
      FROM grouped
      JOIN edge_lists
        ON edge_lists.kind = grouped.kind
       AND edge_lists.value_hash = grouped.value_hash
     ORDER BY CASE WHEN grouped.kind = 'Fingerprint' THEN 0 ELSE 1 END,
              grouped.team_count, grouped.value_hash
     LIMIT $3
"#;

/// Build `ipAnalysis` and `identityOverlaps` exclusively from append-only login
/// observations attributed to historical per-game memberships. Mutable global
/// team rosters, current usernames, and rejection/block rows are intentionally
/// absent. PostgreSQL deduplicates repeated logins, suppresses large shared
/// networks, and bounds the result before any rows reach the application.
pub(super) async fn build_identity_analysis(
    pool: &sqlx::PgPool,
    game_id: i32,
) -> AppResult<(Vec<Json>, Vec<Json>)> {
    let groups = sqlx::query_as::<_, IdentityGroupRow>(IDENTITY_ANALYSIS_SQL)
        .bind(game_id)
        .bind(MAX_SHARED_NETWORK_TEAMS)
        .bind(MAX_IDENTITY_GROUPS)
        .bind(MAX_IDENTITY_INPUT_ROWS)
        .bind(MAX_IDENTITY_ROWS_PER_VALUE)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    let mut ip_rows: Vec<(i32, DateTime<Utc>, Json)> = Vec::new();
    let mut overlap_rows = Vec::with_capacity(groups.len());
    for group in groups {
        if group.team_ids.len() != group.team_names.len() {
            return Err(AppError::internal(
                "identity aggregate returned inconsistent team arrays",
            ));
        }
        if group.edge_left_team_ids.len() != group.edge_right_team_ids.len() {
            return Err(AppError::internal(
                "identity aggregate returned inconsistent edge arrays",
            ));
        }
        let teams: Vec<(i32, String)> = group.team_ids.into_iter().zip(group.team_names).collect();
        let team_names_by_id = teams.iter().cloned().collect::<BTreeMap<_, _>>();
        let edge_pairs = group
            .edge_left_team_ids
            .into_iter()
            .zip(group.edge_right_team_ids)
            .collect::<Vec<_>>();
        let kind = if group.kind.eq_ignore_ascii_case("fingerprint") {
            "fingerprint"
        } else {
            "ip"
        };
        let is_fingerprint = kind == "fingerprint";
        let rule_code = if is_fingerprint {
            "SharedFingerprint"
        } else {
            "CrossTeamIP"
        };
        let masked = crate::services::anti_cheat::redacted_identity_hint(kind, &group.value_hint);
        overlap_rows.push(serde_json::json!({
            "kind": kind,
            "value": masked,
            "teamCount": teams.len(),
            "teamNames": teams.iter().map(|(_, name)| name).collect::<Vec<_>>(),
            "userNames": Vec::<String>::new(),
        }));

        let label = if is_fingerprint {
            "browser fingerprint"
        } else {
            "IP network"
        };
        for (team_id, team_name) in &teams {
            let related_ids = edge_pairs
                .iter()
                .filter_map(|(left_team_id, right_team_id)| {
                    if left_team_id == team_id {
                        Some(*right_team_id)
                    } else if right_team_id == team_id {
                        Some(*left_team_id)
                    } else {
                        None
                    }
                })
                .collect::<BTreeSet<_>>();
            let related = related_ids
                .into_iter()
                .filter_map(|related_team_id| team_names_by_id.get(&related_team_id).cloned())
                .collect::<Vec<_>>();
            let details = format!(
                "Summary: Same {label} observed across multiple teams\nTarget: team '{team_name}'\nMasked value: {masked}\nSource teams: {}",
                related
                    .iter()
                    .map(|name| format!("team '{name}'"))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            ip_rows.push((
                *team_id,
                group.latest,
                serde_json::json!({
                    "teamId": team_id,
                    "teamName": team_name,
                    "type": rule_code,
                    "ip": masked,
                    "time": group.latest.timestamp_millis(),
                    "details": details,
                    "relatedTeams": related,
                    "userNames": Vec::<String>::new(),
                    "relatedUsers": Vec::<String>::new(),
                }),
            ));
        }
    }

    ip_rows.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    Ok((
        ip_rows.into_iter().map(|(_, _, row)| row).collect(),
        overlap_rows,
    ))
}

#[cfg(test)]
mod tests {
    use super::IDENTITY_ANALYSIS_SQL;

    #[test]
    fn identity_report_query_filters_temporal_pair_edges() {
        assert!(IDENTITY_ANALYSIS_SQL.contains("left_identity.user_id < right_identity.user_id"));
        assert!(IDENTITY_ANALYSIS_SQL.contains("left_identity.team_id <> right_identity.team_id"));
        assert!(IDENTITY_ANALYSIS_SQL.contains("exemption.user_a = LEAST("));
        assert!(IDENTITY_ANALYSIS_SQL.contains("exemption.user_b = GREATEST("));
        assert!(
            IDENTITY_ANALYSIS_SQL.contains("exemption.created_at_utc <= candidate.observed_at_utc")
        );
        assert!(
            IDENTITY_ANALYSIS_SQL.contains("candidate.observed_at_utc < exemption.expires_at_utc")
        );
        assert!(
            IDENTITY_ANALYSIS_SQL.contains("candidate.observed_at_utc < exemption.revoked_at_utc")
        );
        assert!(
            IDENTITY_ANALYSIS_SQL.contains("observation.observed_at_utc >= game.start_time_utc")
        );
        assert!(IDENTITY_ANALYSIS_SQL.contains("observation.observed_at_utc < game.end_time_utc"));
        assert!(IDENTITY_ANALYSIS_SQL.contains("LIMIT $4"));
        assert!(IDENTITY_ANALYSIS_SQL.contains("WHERE value_rank <= $5"));
        assert!(!IDENTITY_ANALYSIS_SQL.contains("CURRENT_TIMESTAMP"));
        assert!(!IDENTITY_ANALYSIS_SQL.contains("NOW()"));
    }

    #[test]
    fn identity_report_retains_only_members_of_non_exempt_edges() {
        let pair_edges = IDENTITY_ANALYSIS_SQL.find("pair_edges AS").unwrap();
        let exemption_filter = IDENTITY_ANALYSIS_SQL
            .find("FROM \"AntiCheatExemptions\" exemption")
            .unwrap();
        let edge_members = IDENTITY_ANALYSIS_SQL.find("edge_members AS").unwrap();
        assert!(pair_edges < exemption_filter);
        assert!(exemption_filter < edge_members);
        assert!(IDENTITY_ANALYSIS_SQL.contains("edge_members AS"));
        assert!(IDENTITY_ANALYSIS_SQL.contains("edge_lists AS"));
        assert!(IDENTITY_ANALYSIS_SQL.contains("edge_left_team_ids"));
        assert!(IDENTITY_ANALYSIS_SQL.contains("edge_right_team_ids"));
        assert!(IDENTITY_ANALYSIS_SQL.contains("FROM pair_edges"));
    }
}
