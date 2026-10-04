//! SQL-aggregated live identity deltas.
//!
//! Every historical lookup is a capped lateral index walk nominated by one
//! captured delta row. Population-relative IP/subnet rules and any live edge
//! outside these context caps remain covered by the authoritative final sweep.

use chrono::{DateTime, Utc};
use sea_orm::DatabaseConnection;

use super::*;

const MAX_DELTA_OBSERVATIONS: i64 = super::super::reconciliation::SOURCE_BATCH;
const MAX_SHARED_MATCH_ROWS: i64 = 8;
const MAX_CHURN_PREDECESSORS: i64 = 31;
const MAX_SESSION_PREDECESSORS: i64 = 15;

const INCREMENTAL_CORRELATION_SQL: &str = r#"
    WITH delta AS MATERIALIZED (
        SELECT observation.id, observation.user_id, roster.team_id,
               roster.participation_id, observation.kind,
               observation.value_hash, observation.broad_network_hash,
               observation.observed_at_utc
          FROM "IdentityObservations" observation
          JOIN "UserParticipations" roster
            ON roster.user_id = observation.user_id
           AND roster.game_id = observation.game_id
           AND roster.team_id = observation.team_id
           AND roster.participation_id = observation.participation_id
          JOIN "Participations" participation
            ON participation.id = roster.participation_id
           AND participation.game_id = roster.game_id
         JOIN "Games" game ON game.id = observation.game_id
         WHERE observation.game_id = $1
           AND observation.reconciliation_version > $2
           AND observation.reconciliation_version <= $3
           AND observation.observed_at_utc >= game.start_time_utc
           AND observation.observed_at_utc < game.end_time_utc
           AND participation.competitive_admitted_at_utc IS NOT NULL
           AND participation.competitive_admitted_at_utc < game.end_time_utc
         ORDER BY observation.reconciliation_version
         LIMIT $4
    ), shared_edges AS MATERIALIZED (
        SELECT delta.participation_id AS left_participation_id,
               matched.participation_id AS right_participation_id,
               delta.user_id AS left_user_id,
               matched.user_id AS right_user_id,
               delta.kind, delta.value_hash,
               GREATEST(delta.observed_at_utc,
                        matched.observed_at_utc) AS observed_at
          FROM delta
          JOIN LATERAL (
              SELECT observation.id, observation.user_id,
                     roster.team_id, roster.participation_id,
                     observation.observed_at_utc
                FROM (
                    SELECT candidate.id, candidate.user_id,
                           candidate.team_id, candidate.participation_id,
                           candidate.observed_at_utc
                      FROM "IdentityObservations" candidate
                     WHERE candidate.game_id = $1
                       AND candidate.kind = delta.kind
                       AND candidate.value_hash = delta.value_hash
                     ORDER BY candidate.observed_at_utc DESC, candidate.id DESC
                     LIMIT $5
                ) observation
                JOIN "UserParticipations" roster
                  ON roster.user_id = observation.user_id
                 AND roster.game_id = $1
                 AND roster.team_id = observation.team_id
                 AND roster.participation_id = observation.participation_id
                JOIN "Participations" participation
                  ON participation.id = roster.participation_id
                 AND participation.game_id = roster.game_id
                JOIN "Games" game ON game.id = $1
               WHERE observation.user_id <> delta.user_id
                 AND observation.observed_at_utc >= game.start_time_utc
                 AND observation.observed_at_utc < game.end_time_utc
                 AND participation.competitive_admitted_at_utc IS NOT NULL
                 AND participation.competitive_admitted_at_utc < game.end_time_utc
                 AND (
                      (delta.kind = 'Fingerprint'
                       AND roster.team_id <> delta.team_id)
                      OR (delta.kind = 'Ip'
                          AND roster.team_id = delta.team_id)
                 )
                 AND NOT EXISTS (
                     SELECT 1 FROM "AntiCheatExemptions" exemption
                      WHERE exemption.user_a = LEAST(
                                delta.user_id, observation.user_id)
                        AND exemption.user_b = GREATEST(
                                delta.user_id, observation.user_id)
                        AND exemption.kind = delta.kind
                        AND exemption.value_hash = delta.value_hash
                        AND exemption.created_at_utc <= GREATEST(
                                delta.observed_at_utc,
                                observation.observed_at_utc)
                        AND GREATEST(delta.observed_at_utc,
                                     observation.observed_at_utc)
                              < exemption.expires_at_utc
                        AND (exemption.revoked_at_utc IS NULL
                             OR GREATEST(delta.observed_at_utc,
                                         observation.observed_at_utc)
                                  < exemption.revoked_at_utc)
                 )
          ) matched ON TRUE
         WHERE delta.kind IN ('Fingerprint', 'Ip')
    ), churn_context AS MATERIALIZED (
        SELECT delta.id AS anchor_id, delta.participation_id,
               delta.user_id, delta.kind, delta.value_hash,
               delta.observed_at_utc AS anchor_observed_at
          FROM delta
         WHERE delta.kind IN ('Fingerprint', 'Ip')
        UNION ALL
        SELECT delta.id, delta.participation_id, delta.user_id, delta.kind,
               predecessor.value_hash, delta.observed_at_utc
          FROM delta
          JOIN LATERAL (
              SELECT observation.value_hash
                FROM "IdentityObservations" observation
                JOIN "Games" game ON game.id = observation.game_id
               WHERE observation.game_id = $1
                 AND observation.user_id = delta.user_id
                 AND observation.kind = delta.kind
                 AND (observation.observed_at_utc, observation.id)
                       < (delta.observed_at_utc, delta.id)
                 AND observation.observed_at_utc >= game.start_time_utc
                 AND observation.observed_at_utc < game.end_time_utc
               ORDER BY observation.observed_at_utc DESC, observation.id DESC
               LIMIT $6
          ) predecessor ON TRUE
         WHERE delta.kind IN ('Fingerprint', 'Ip')
    ), churn_hits AS MATERIALIZED (
        SELECT anchor_id, participation_id, user_id, kind,
               MIN(anchor_observed_at) AS observed_at
          FROM churn_context
         GROUP BY anchor_id, participation_id, user_id, kind
        HAVING COUNT(DISTINCT value_hash) >= 4
    ), session_context AS MATERIALIZED (
        SELECT delta.id AS anchor_id, delta.participation_id,
               delta.user_id, delta.broad_network_hash,
               delta.observed_at_utc, delta.observed_at_utc AS anchor_observed_at,
               delta.id AS observation_id
          FROM delta
         WHERE delta.kind = 'Ip' AND delta.broad_network_hash IS NOT NULL
        UNION ALL
        SELECT delta.id, delta.participation_id, delta.user_id,
               predecessor.broad_network_hash, predecessor.observed_at_utc,
               delta.observed_at_utc, predecessor.id
          FROM delta
          JOIN LATERAL (
              SELECT observation.id, observation.broad_network_hash,
                     observation.observed_at_utc
                FROM "IdentityObservations" observation
               WHERE observation.game_id = $1
                 AND observation.user_id = delta.user_id
                 AND observation.kind = 'Ip'
                 AND observation.broad_network_hash IS NOT NULL
                 AND observation.observed_at_utc
                       >= delta.observed_at_utc - INTERVAL '10 minutes'
                 AND (observation.observed_at_utc, observation.id)
                       < (delta.observed_at_utc, delta.id)
               ORDER BY observation.observed_at_utc DESC, observation.id DESC
               LIMIT $7
          ) predecessor ON TRUE
         WHERE delta.kind = 'Ip' AND delta.broad_network_hash IS NOT NULL
    ), session_hits AS MATERIALIZED (
        SELECT left_context.anchor_id, left_context.participation_id,
               left_context.user_id,
               MIN(left_context.anchor_observed_at) AS observed_at
          FROM session_context left_context
          JOIN session_context right_context
            ON right_context.anchor_id = left_context.anchor_id
           AND (right_context.observed_at_utc, right_context.observation_id)
                 > (left_context.observed_at_utc, left_context.observation_id)
           AND right_context.broad_network_hash
                 <> left_context.broad_network_hash
         GROUP BY left_context.anchor_id, left_context.participation_id,
                  left_context.user_id
        HAVING COUNT(*) >= 3
    ), raw_candidates AS (
        SELECT edge.left_participation_id AS participation_id,
               CASE edge.kind WHEN 'Fingerprint' THEN $8::smallint
                              ELSE $9::smallint END AS kind,
               CASE edge.kind WHEN 'Fingerprint' THEN 'shared-fingerprint:'
                              ELSE 'shared-ip:' END
                    || encode(edge.value_hash, 'hex') AS evidence_key,
               edge.observed_at
          FROM shared_edges edge
        UNION ALL
        SELECT edge.right_participation_id,
               CASE edge.kind WHEN 'Fingerprint' THEN $8::smallint
                              ELSE $9::smallint END,
               CASE edge.kind WHEN 'Fingerprint' THEN 'shared-fingerprint:'
                              ELSE 'shared-ip:' END
                    || encode(edge.value_hash, 'hex'),
               edge.observed_at
          FROM shared_edges edge
        UNION ALL
        SELECT participation_id,
               CASE kind WHEN 'Fingerprint' THEN $10::smallint
                         ELSE $11::smallint END,
               CASE kind WHEN 'Fingerprint' THEN 'fingerprint-churn:user:'
                         ELSE 'ip-churn:user:' END || user_id::text,
               observed_at
          FROM churn_hits
        UNION ALL
        SELECT participation_id, $12::smallint,
               'session-concurrency:user:' || user_id::text, observed_at
          FROM session_hits
    )
    SELECT participation_id, kind, evidence_key, MIN(observed_at) AS observed_at
      FROM raw_candidates
     GROUP BY participation_id, kind, evidence_key
     ORDER BY observed_at, participation_id, kind, evidence_key
"#;

pub(crate) async fn run_correlation_checks_incremental(
    db: &DatabaseConnection,
    game_id: i32,
    cursor: Option<super::super::reconciliation::SourceCursor>,
) -> AppResult<()> {
    let Some(cursor) = cursor.filter(|cursor| cursor.after < cursor.through) else {
        return Ok(());
    };
    let candidates: Vec<(i32, i16, String, DateTime<Utc>)> =
        sqlx::query_as(INCREMENTAL_CORRELATION_SQL)
            .bind(game_id)
            .bind(cursor.after)
            .bind(cursor.through)
            .bind(MAX_DELTA_OBSERVATIONS)
            .bind(MAX_SHARED_MATCH_ROWS)
            .bind(MAX_CHURN_PREDECESSORS)
            .bind(MAX_SESSION_PREDECESSORS)
            .bind(SuspicionType::SharedFingerprint.kind())
            .bind(SuspicionType::SharedIp.kind())
            .bind(SuspicionType::FingerprintChurn.kind())
            .bind(SuspicionType::IpChurn.kind())
            .bind(SuspicionType::SessionConcurrency.kind())
            .fetch_all(db.get_postgres_connection_pool())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    let mut codes = Vec::new();
    for (participation_id, kind, evidence_key, observed_at) in candidates {
        let ty = SuspicionType::from_kind(kind)
            .ok_or_else(|| AppError::internal("invalid incremental correlation kind"))?;
        super::super::detectors::record_with_dedup_at(
            db,
            game_id,
            participation_id,
            None,
            ty,
            &evidence_key,
            observed_at,
            &mut codes,
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::INCREMENTAL_CORRELATION_SQL;

    #[test]
    fn every_live_identity_intermediate_is_delta_nominated_and_capped() {
        assert!(INCREMENTAL_CORRELATION_SQL.contains("observation.reconciliation_version > $2"));
        assert!(INCREMENTAL_CORRELATION_SQL.contains("ORDER BY observation.reconciliation_version"));
        for cap in ["LIMIT $4", "LIMIT $5", "LIMIT $6", "LIMIT $7"] {
            assert!(INCREMENTAL_CORRELATION_SQL.contains(cap), "missing {cap}");
        }
        assert_eq!(
            INCREMENTAL_CORRELATION_SQL.matches("JOIN LATERAL").count(),
            3
        );
        assert!(INCREMENTAL_CORRELATION_SQL.contains("< (delta.observed_at_utc, delta.id)"));
        assert!(
            INCREMENTAL_CORRELATION_SQL.contains("delta.observed_at_utc - INTERVAL '10 minutes'")
        );
        assert!(!INCREMENTAL_CORRELATION_SQL.contains("identity_members"));
        assert!(!INCREMENTAL_CORRELATION_SQL.contains("ROW_NUMBER()"));
        assert!(!INCREMENTAL_CORRELATION_SQL.contains("LIMIT $13"));
    }
}
