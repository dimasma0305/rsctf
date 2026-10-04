//! Bounded context-finding derivation from captured telemetry cursors.

use super::*;
use crate::services::suspicion::SourceCursor;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FusionCursors {
    pub dns: Option<SourceCursor>,
    pub peer: Option<SourceCursor>,
    pub flag: Option<SourceCursor>,
    pub suspicion: Option<SourceCursor>,
    pub cheat: Option<SourceCursor>,
}

impl FusionCursors {
    pub(crate) fn has_work(self) -> bool {
        [self.dns, self.peer, self.flag, self.suspicion, self.cheat]
            .into_iter()
            .flatten()
            .any(|cursor| cursor.after < cursor.through)
    }
}

fn bounds(cursor: Option<SourceCursor>) -> (i64, i64) {
    cursor
        .map(|cursor| (cursor.after, cursor.through))
        .unwrap_or((0, 0))
}

const MAX_INCREMENTAL_DELTAS: i64 = crate::services::suspicion::SOURCE_BATCH;
const MAX_SUPPORT_CONTEXT_ROWS: i64 = 4;

const INCREMENTAL_FINDINGS_SQL: &str = r#"
    WITH dns_delta AS MATERIALIZED (
        SELECT dns.* FROM "VpnDnsProviderBuckets" dns
         WHERE dns.game_id = $1
           AND dns.reconciliation_version > $2
           AND dns.reconciliation_version <= $3
         ORDER BY dns.reconciliation_version LIMIT $8
    ), peer_delta AS MATERIALIZED (
        SELECT network.* FROM "VpnPeerNetworkObservations" network
         WHERE network.game_id = $1
           AND network.reconciliation_version > $4
           AND network.reconciliation_version <= $5
         ORDER BY network.reconciliation_version LIMIT $8
    ), flag_delta AS MATERIALIZED (
        SELECT flag.* FROM "VpnFlagTransportEvents" flag
         WHERE flag.game_id = $1
           AND flag.reconciliation_version > $6
           AND flag.reconciliation_version <= $7
         ORDER BY flag.reconciliation_version LIMIT $8
    ), candidates AS (
        SELECT dns.game_id, dns.participation_id, dns.user_id,
               'AiProviderDns'::text AS detector_code,
               1::integer AS detector_version,
               1::smallint AS evidence_family,
               0::smallint AS evidence_tier,
               0::integer AS score_delta,
               'dns:' || dns.id::text AS evidence_key,
               dns.first_seen_at_utc AS occurred_at_utc,
               jsonb_build_object(
                   'providerCategory', dns.provider_category,
                   'queryCount', dns.query_count,
                   'meaning', 'network context only; not proof of AI use'
               ) AS details
          FROM dns_delta dns
        UNION ALL
        SELECT network.game_id, network.participation_id, network.user_id,
               'HostingNetworkSource', 1, 1, 0, 0,
               'network:' || network.id::text, network.first_seen_at_utc,
               jsonb_build_object(
                   'networkClass', network.network_class,
                   'sourceAsn', network.source_asn,
                   'meaning', 'network context only; shared/VPS networks are not proof'
               )
          FROM peer_delta network WHERE network.network_class <> 0
        UNION ALL
        SELECT flag.game_id, flag.receiving_participation_id,
               flag.receiving_user_id, 'ForeignFlagTransport', 1, 4, 0, 0,
               'flag-transport:' || flag.id::text, flag.observed_at_utc,
               jsonb_build_object(
                   'challengeId', flag.challenge_id,
                   'owningParticipationId', flag.owning_participation_id,
                   'transport', flag.transport,
                   'meaning', 'exact foreign flag bytes crossed the VPN; framing is not proven'
               )
          FROM flag_delta flag
    ), inserted AS (
        INSERT INTO "AntiCheatFindings"
          (game_id, participation_id, user_id, detector_code, detector_version,
           evidence_family, evidence_tier, score_delta, evidence_key,
           occurred_at_utc, details, shadow)
        SELECT candidate.game_id, candidate.participation_id, candidate.user_id,
               candidate.detector_code, candidate.detector_version,
               candidate.evidence_family, candidate.evidence_tier,
               candidate.score_delta, candidate.evidence_key,
               candidate.occurred_at_utc, candidate.details, TRUE
          FROM candidates candidate
          JOIN "Participations" participation
            ON participation.game_id = candidate.game_id
           AND participation.id = candidate.participation_id
         WHERE participation.competitive_admitted_at_utc IS NOT NULL
        ON CONFLICT DO NOTHING RETURNING 1
    ) SELECT COUNT(*)::bigint FROM inserted
"#;

const INCREMENTAL_PEER_SHARING_SQL: &str = r#"
    WITH peer_delta AS MATERIALIZED (
        SELECT observation.game_id, observation.participation_id,
               observation.user_id, observation.peer_id
          FROM "VpnPeerNetworkObservations" observation
         WHERE observation.game_id = $1
           AND observation.reconciliation_version > $2
           AND observation.reconciliation_version <= $3
         ORDER BY observation.reconciliation_version
         LIMIT $4
    ), changed_peers AS MATERIALIZED (
        SELECT DISTINCT game_id, participation_id, user_id, peer_id
          FROM peer_delta
    ), candidates AS (
        SELECT changed.game_id, changed.participation_id, changed.user_id,
               changed.peer_id,
               GREATEST(first_endpoint.first_seen_at_utc,
                        second_endpoint.first_seen_at_utc) AS occurred_at_utc,
               2::integer AS endpoints
          FROM changed_peers changed
          JOIN LATERAL (
              SELECT observation.endpoint_hash,
                     observation.first_seen_at_utc
                FROM "VpnPeerNetworkObservations" observation
               WHERE observation.game_id = changed.game_id
                 AND observation.peer_id = changed.peer_id
               ORDER BY observation.endpoint_hash,
                        observation.first_seen_at_utc, observation.id
               LIMIT 1
          ) first_endpoint ON TRUE
          JOIN LATERAL (
              SELECT observation.endpoint_hash,
                     observation.first_seen_at_utc
                FROM "VpnPeerNetworkObservations" observation
               WHERE observation.game_id = changed.game_id
                 AND observation.peer_id = changed.peer_id
                 AND observation.endpoint_hash > first_endpoint.endpoint_hash
               ORDER BY observation.endpoint_hash,
                        observation.first_seen_at_utc, observation.id
               LIMIT 1
          ) second_endpoint ON TRUE
    ), inserted AS (
        INSERT INTO "AntiCheatFindings"
          (game_id, participation_id, user_id, detector_code, detector_version,
           evidence_family, evidence_tier, score_delta, evidence_key,
           occurred_at_utc, details, shadow)
        SELECT game_id, participation_id, user_id,
               'VpnPeerDeviceSharing', 1, 1, 0, 0,
               'peer:' || peer_id::text, occurred_at_utc,
               jsonb_build_object(
                   'endpointCount', endpoints,
                   'endpointCountCapped', TRUE,
                   'meaning', 'one event VPN profile appeared from multiple endpoints; context only'
               ), TRUE
          FROM candidates
        ON CONFLICT DO NOTHING RETURNING 1
    ) SELECT COUNT(*)::bigint FROM inserted
"#;

const INCREMENTAL_DERIVED_RELATIONSHIPS_SQL: &str = r#"
    WITH dns_delta AS MATERIALIZED (
        SELECT id FROM "VpnDnsProviderBuckets"
         WHERE game_id = $1 AND reconciliation_version > $2
           AND reconciliation_version <= $3
         ORDER BY reconciliation_version LIMIT $8
    ), peer_delta AS MATERIALIZED (
        SELECT id, peer_id FROM "VpnPeerNetworkObservations"
         WHERE game_id = $1 AND reconciliation_version > $4
           AND reconciliation_version <= $5
         ORDER BY reconciliation_version LIMIT $8
    ), flag_delta AS MATERIALIZED (
        SELECT id FROM "VpnFlagTransportEvents"
         WHERE game_id = $1 AND reconciliation_version > $6
           AND reconciliation_version <= $7
         ORDER BY reconciliation_version LIMIT $8
    ), nominations AS MATERIALIZED (
        SELECT 'AiProviderDns'::text AS detector_code,
               'dns:' || id::text AS evidence_key,
               'VpnDnsProviderBucket'::text AS source_type
          FROM dns_delta
        UNION
        SELECT 'HostingNetworkSource', 'network:' || id::text,
               'VpnPeerNetworkObservation' FROM peer_delta
        UNION
        SELECT 'VpnPeerDeviceSharing', 'peer:' || peer_id::text,
               'VpnPeerNetworkObservation' FROM peer_delta
        UNION
        SELECT 'ForeignFlagTransport', 'flag-transport:' || id::text,
               'VpnFlagTransportEvent' FROM flag_delta
    )
    INSERT INTO "AntiCheatEvidenceRelationships"
      (game_id, finding_id, relation_kind,
       related_source_type, related_source_key)
    SELECT $1, finding.id, $9, nomination.source_type,
           nomination.evidence_key
      FROM nominations nomination
      JOIN "AntiCheatFindings" finding
        ON finding.game_id = $1
       AND finding.detector_code = nomination.detector_code
       AND finding.evidence_key = nomination.evidence_key
    ON CONFLICT DO NOTHING
"#;

const INCREMENTAL_SUPPORT_RELATIONSHIPS_SQL: &str = r#"
    WITH flag_delta AS MATERIALIZED (
        SELECT flag.* FROM "VpnFlagTransportEvents" flag
         WHERE flag.game_id = $1 AND flag.reconciliation_version > $2
           AND flag.reconciliation_version <= $3
         ORDER BY flag.reconciliation_version LIMIT $8
    ), event_delta AS MATERIALIZED (
        SELECT event.* FROM "SuspicionEvents" event
         WHERE event.game_id = $1 AND event.reconciliation_version > $4
           AND event.reconciliation_version <= $5 AND event.kind = 0
         ORDER BY event.reconciliation_version LIMIT $8
    ), cheat_delta AS MATERIALIZED (
        SELECT cheat.* FROM "CheatInfo" cheat
         WHERE cheat.game_id = $1 AND cheat.reconciliation_version > $6
           AND cheat.reconciliation_version <= $7
         ORDER BY cheat.reconciliation_version LIMIT $8
    ), nominated_cheats AS MATERIALIZED (
        SELECT cheat.id, cheat.game_id, cheat.challenge_id,
               cheat.submit_participation_id, cheat.source_participation_id,
               cheat.evidence_key, cheat.observed_at_utc
          FROM cheat_delta cheat
        UNION
        SELECT cheat.id, cheat.game_id, cheat.challenge_id,
               cheat.submit_participation_id, cheat.source_participation_id,
               cheat.evidence_key, cheat.observed_at_utc
          FROM flag_delta flag
          JOIN LATERAL (
              SELECT cheat.* FROM "CheatInfo" cheat
               WHERE cheat.game_id = flag.game_id
                 AND cheat.challenge_id = flag.challenge_id
                 AND cheat.submit_participation_id
                       = flag.receiving_participation_id
                 AND cheat.source_participation_id
                       = flag.owning_participation_id
                 AND cheat.observed_at_utc >= flag.observed_at_utc
               ORDER BY cheat.observed_at_utc, cheat.id LIMIT $9
          ) cheat ON TRUE
        UNION
        SELECT cheat.id, cheat.game_id, cheat.challenge_id,
               cheat.submit_participation_id, cheat.source_participation_id,
               cheat.evidence_key, cheat.observed_at_utc
          FROM event_delta event
          JOIN LATERAL (
              SELECT cheat.* FROM "CheatInfo" cheat
               WHERE cheat.game_id = event.game_id
                 AND cheat.submit_participation_id = event.participation_id
                 AND cheat.evidence_key = event.evidence_key
               ORDER BY cheat.id LIMIT $9
          ) cheat ON TRUE
    ), candidates AS MATERIALIZED (
        SELECT cheat.game_id, transport.id AS transport_id,
               event.id AS event_id
          FROM nominated_cheats cheat
          JOIN LATERAL (
              SELECT event.id FROM "SuspicionEvents" event
               WHERE event.game_id = cheat.game_id
                 AND event.participation_id = cheat.submit_participation_id
                 AND event.kind = 0
                 AND event.evidence_key = cheat.evidence_key
               ORDER BY event.id LIMIT 1
          ) event ON TRUE
          JOIN LATERAL (
              SELECT transport.id FROM "VpnFlagTransportEvents" transport
               WHERE transport.game_id = cheat.game_id
                 AND transport.challenge_id = cheat.challenge_id
                 AND transport.receiving_participation_id
                       = cheat.submit_participation_id
                 AND transport.owning_participation_id
                       = cheat.source_participation_id
                 AND transport.observed_at_utc <= cheat.observed_at_utc
               ORDER BY transport.observed_at_utc DESC, transport.id DESC
               LIMIT $9
          ) transport ON TRUE
    )
    INSERT INTO "AntiCheatEvidenceRelationships"
      (game_id, finding_id, relation_kind,
       related_source_type, related_source_key)
    SELECT candidate.game_id, finding.id, $10,
           'SuspicionEvent', 'event:' || candidate.event_id::text
      FROM candidates candidate
      JOIN "AntiCheatFindings" finding
        ON finding.game_id = candidate.game_id
       AND finding.detector_code = 'ForeignFlagTransport'
       AND finding.evidence_key
             = 'flag-transport:' || candidate.transport_id::text
    ON CONFLICT DO NOTHING
"#;

pub(crate) async fn derive_context_findings_incremental(
    st: &SharedState,
    game_id: i32,
    cursors: FusionCursors,
) -> AppResult<usize> {
    let (dns_after, dns_through) = bounds(cursors.dns);
    let (peer_after, peer_through) = bounds(cursors.peer);
    let (flag_after, flag_through) = bounds(cursors.flag);
    let (suspicion_after, suspicion_through) = bounds(cursors.suspicion);
    let (cheat_after, cheat_through) = bounds(cursors.cheat);
    let mut transaction = st
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    let inserted: i64 = sqlx::query_scalar(INCREMENTAL_FINDINGS_SQL)
        .bind(game_id)
        .bind(dns_after)
        .bind(dns_through)
        .bind(peer_after)
        .bind(peer_through)
        .bind(flag_after)
        .bind(flag_through)
        .bind(MAX_INCREMENTAL_DELTAS)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    let sharing: i64 = sqlx::query_scalar(INCREMENTAL_PEER_SHARING_SQL)
        .bind(game_id)
        .bind(peer_after)
        .bind(peer_through)
        .bind(MAX_INCREMENTAL_DELTAS)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    sqlx::query(INCREMENTAL_DERIVED_RELATIONSHIPS_SQL)
        .bind(game_id)
        .bind(dns_after)
        .bind(dns_through)
        .bind(peer_after)
        .bind(peer_through)
        .bind(flag_after)
        .bind(flag_through)
        .bind(MAX_INCREMENTAL_DELTAS)
        .bind(EvidenceRelationshipKind::DerivedFrom as i16)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    sqlx::query(INCREMENTAL_SUPPORT_RELATIONSHIPS_SQL)
        .bind(game_id)
        .bind(flag_after)
        .bind(flag_through)
        .bind(suspicion_after)
        .bind(suspicion_through)
        .bind(cheat_after)
        .bind(cheat_through)
        .bind(MAX_INCREMENTAL_DELTAS)
        .bind(MAX_SUPPORT_CONTEXT_ROWS)
        .bind(EvidenceRelationshipKind::Supports as i16)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(usize::try_from(inserted + sharing).unwrap_or(usize::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_presence_requires_a_nonempty_range() {
        let empty = SourceCursor {
            kind: 3,
            after: 4,
            through: 4,
        };
        let work = SourceCursor {
            kind: 3,
            after: 4,
            through: 5,
        };
        assert!(!FusionCursors {
            dns: Some(empty),
            ..FusionCursors::default()
        }
        .has_work());
        assert!(FusionCursors {
            dns: Some(work),
            ..FusionCursors::default()
        }
        .has_work());
    }

    #[test]
    fn every_finding_delta_uses_the_stored_version_and_cap() {
        assert_eq!(INCREMENTAL_FINDINGS_SQL.matches("ORDER BY ").count(), 3);
        assert_eq!(INCREMENTAL_FINDINGS_SQL.matches("LIMIT $8").count(), 3);
        assert_eq!(
            INCREMENTAL_FINDINGS_SQL
                .matches("reconciliation_version >")
                .count(),
            3
        );
    }

    #[test]
    fn peer_sharing_uses_two_index_seeks_instead_of_history_aggregation() {
        assert_eq!(
            INCREMENTAL_PEER_SHARING_SQL.matches("JOIN LATERAL").count(),
            2
        );
        assert_eq!(INCREMENTAL_PEER_SHARING_SQL.matches("LIMIT 1").count(), 2);
        assert!(INCREMENTAL_PEER_SHARING_SQL.contains("LIMIT $4"));
        assert!(INCREMENTAL_PEER_SHARING_SQL
            .contains("observation.endpoint_hash > first_endpoint.endpoint_hash"));
        assert!(!INCREMENTAL_PEER_SHARING_SQL.contains("COUNT(DISTINCT"));
    }

    #[test]
    fn derived_relationships_are_delta_nominated_exact_index_lookups() {
        assert_eq!(
            INCREMENTAL_DERIVED_RELATIONSHIPS_SQL
                .matches("LIMIT $8")
                .count(),
            3
        );
        assert!(INCREMENTAL_DERIVED_RELATIONSHIPS_SQL
            .contains("finding.detector_code = nomination.detector_code"));
        assert!(INCREMENTAL_DERIVED_RELATIONSHIPS_SQL
            .contains("finding.evidence_key = nomination.evidence_key"));
        assert!(!INCREMENTAL_DERIVED_RELATIONSHIPS_SQL.contains("split_part"));
        assert!(!INCREMENTAL_DERIVED_RELATIONSHIPS_SQL
            .contains("FROM \"AntiCheatFindings\" finding\n     WHERE"));
    }

    #[test]
    fn support_relationships_have_bounded_delta_and_context_fanout() {
        assert_eq!(
            INCREMENTAL_SUPPORT_RELATIONSHIPS_SQL
                .matches("LIMIT $8")
                .count(),
            3
        );
        assert_eq!(
            INCREMENTAL_SUPPORT_RELATIONSHIPS_SQL
                .matches("LIMIT $9")
                .count(),
            3
        );
        assert_eq!(
            INCREMENTAL_SUPPORT_RELATIONSHIPS_SQL
                .matches("JOIN LATERAL")
                .count(),
            4
        );
        assert!(INCREMENTAL_SUPPORT_RELATIONSHIPS_SQL
            .contains("finding.evidence_key\n             = 'flag-transport:'"));
        assert!(!INCREMENTAL_SUPPORT_RELATIONSHIPS_SQL
            .contains("FROM \"AntiCheatFindings\" finding\n     JOIN"));
    }
}
