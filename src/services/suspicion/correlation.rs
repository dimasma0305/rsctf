//! Game-scoped identity-correlation detectors.
//!
//! Identity evidence comes exclusively from append-only `IdentityObservations`
//! keyed by immutable user ids and durable `UserParticipations`. Mutable account
//! fields, current team membership, user names, and all-time anti-cheat blocks
//! are deliberately excluded: changing any of those after a game must not
//! rewrite that game's evidence.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Duration, Utc};
use sea_orm::DatabaseConnection;
use uuid::Uuid;

use super::{AppError, AppResult, SuspicionType};

const FINGERPRINT_CHURN_THRESHOLD: usize = 4;
const IP_CHURN_THRESHOLD: usize = 4;
const SESSION_WINDOW_MINUTES: i64 = 10;
const SESSION_CONCURRENCY_MIN_OCCURRENCES: usize = 3;
const SHARED_IDENTITY_MAX_TEAMS: usize = 4;

#[path = "correlation/incremental.rs"]
mod incremental;
pub(crate) use incremental::run_correlation_checks_incremental;

const LOAD_OBSERVATIONS_SQL: &str = r#"
    SELECT observation.id, observation.user_id,
           roster.team_id, roster.participation_id,
           observation.kind, observation.value_hash,
           observation.subnet_group_hash, observation.broad_network_hash,
           observation.observed_at_utc
      FROM "IdentityObservations" observation
      JOIN "UserParticipations" roster
        ON roster.user_id = observation.user_id
       AND roster.game_id = $1
       AND roster.team_id = observation.team_id
       AND roster.participation_id = observation.participation_id
      JOIN "Participations" participation
        ON participation.id = roster.participation_id
       AND participation.game_id = roster.game_id
     WHERE observation.game_id = $1
       AND observation.observed_at_utc >= $2
       AND observation.observed_at_utc < $3
       AND participation.competitive_admitted_at_utc IS NOT NULL
       AND participation.competitive_admitted_at_utc < $3
     ORDER BY observation.observed_at_utc, observation.id
"#;

const LOAD_SUBMISSION_IDENTITIES_SQL: &str = r#"
    SELECT submission.id, submission.participation_id,
           submission.submit_remote_ip_hash, submission.submit_time_utc
      FROM "Submissions" submission
      JOIN "Participations" participation
        ON participation.id = submission.participation_id
       AND participation.game_id = submission.game_id
     WHERE submission.game_id = $1
       AND submission.submit_time_utc >= $2
       AND submission.submit_time_utc < $3
       AND submission.submit_remote_ip_hash IS NOT NULL
       AND participation.competitive_admitted_at_utc IS NOT NULL
       AND participation.competitive_admitted_at_utc < $3
     ORDER BY submission.submit_time_utc, submission.id
"#;

const LOAD_IDENTITY_EXEMPTIONS_SQL: &str = r#"
    SELECT exemption.user_a, exemption.user_b,
           exemption.kind, exemption.value_hash,
           exemption.created_at_utc, exemption.expires_at_utc,
           exemption.revoked_at_utc
      FROM "AntiCheatExemptions" exemption
     WHERE exemption.created_at_utc < $3
       AND exemption.expires_at_utc > $2
       AND (exemption.revoked_at_utc IS NULL
            OR exemption.revoked_at_utc > $2)
       AND EXISTS (
            SELECT 1
              FROM "IdentityObservations" observation
              JOIN "UserParticipations" roster
                ON roster.user_id = observation.user_id
               AND roster.game_id = $1
               AND roster.team_id = observation.team_id
               AND roster.participation_id = observation.participation_id
              JOIN "Participations" participation
                ON participation.id = roster.participation_id
               AND participation.game_id = roster.game_id
             WHERE observation.game_id = $1
               AND observation.observed_at_utc >= $2
               AND observation.observed_at_utc < $3
               AND participation.competitive_admitted_at_utc IS NOT NULL
               AND participation.competitive_admitted_at_utc < $3
               AND observation.kind = exemption.kind
               AND observation.value_hash = exemption.value_hash
               AND observation.user_id IN (exemption.user_a, exemption.user_b)
       )
     ORDER BY exemption.user_a, exemption.user_b,
              exemption.kind, exemption.value_hash,
              exemption.created_at_utc, exemption.expires_at_utc
"#;

#[derive(Clone, Debug, sqlx::FromRow)]
struct Observation {
    #[allow(dead_code)]
    id: i64,
    user_id: Uuid,
    team_id: i32,
    participation_id: i32,
    kind: String,
    value_hash: Vec<u8>,
    subnet_group_hash: Option<Vec<u8>>,
    broad_network_hash: Option<Vec<u8>>,
    observed_at_utc: DateTime<Utc>,
}

#[derive(Debug, sqlx::FromRow)]
struct SubmissionIdentity {
    id: i32,
    participation_id: i32,
    submit_remote_ip_hash: Option<Vec<u8>>,
    submit_time_utc: DateTime<Utc>,
}

#[derive(Clone, Debug, sqlx::FromRow)]
struct IdentityExemption {
    user_a: Uuid,
    user_b: Uuid,
    kind: String,
    value_hash: Vec<u8>,
    created_at_utc: DateTime<Utc>,
    expires_at_utc: DateTime<Utc>,
    revoked_at_utc: Option<DateTime<Utc>>,
}

type Candidates = BTreeMap<(i32, i16, String), DateTime<Utc>>;
type IdentityExemptions = BTreeMap<(Uuid, Uuid), Vec<IdentityExemption>>;

#[derive(Default)]
struct IdentityGroup {
    members: BTreeMap<Uuid, IdentityMember>,
}

struct IdentityMember {
    team_id: i32,
    participation_id: i32,
    observations_by_value: BTreeMap<Vec<u8>, Vec<DateTime<Utc>>>,
}

impl IdentityGroup {
    fn observe(&mut self, observation: &Observation) {
        let member = self
            .members
            .entry(observation.user_id)
            .or_insert_with(|| IdentityMember {
                team_id: observation.team_id,
                participation_id: observation.participation_id,
                observations_by_value: BTreeMap::new(),
            });
        debug_assert_eq!(member.team_id, observation.team_id);
        debug_assert_eq!(member.participation_id, observation.participation_id);
        member
            .observations_by_value
            .entry(observation.value_hash.clone())
            .or_default()
            .push(observation.observed_at_utc);
    }

    fn team_count(&self) -> usize {
        self.members
            .values()
            .map(|member| member.team_id)
            .collect::<BTreeSet<_>>()
            .len()
    }
}

fn canonical_user_pair(left: Uuid, right: Uuid) -> (Uuid, Uuid) {
    if left.as_bytes() < right.as_bytes() {
        (left, right)
    } else {
        (right, left)
    }
}

fn identity_edge_is_exempt(
    exemptions: &IdentityExemptions,
    left: Uuid,
    right: Uuid,
    kind: &str,
    value_hash: &[u8],
    observed_at: DateTime<Utc>,
) -> bool {
    exemptions
        .get(&canonical_user_pair(left, right))
        .is_some_and(|grants| {
            grants.iter().any(|grant| {
                grant.kind == kind
                    && grant.value_hash == value_hash
                    && grant.created_at_utc <= observed_at
                    && observed_at < grant.expires_at_utc
                    && grant
                        .revoked_at_utc
                        .is_none_or(|revoked_at| observed_at < revoked_at)
            })
        })
}

fn earliest_unexempt_edge(
    exemptions: &IdentityExemptions,
    left_user: Uuid,
    left: &IdentityMember,
    right_user: Uuid,
    right: &IdentityMember,
    kind: &str,
) -> Option<DateTime<Utc>> {
    let mut earliest = None;
    for (left_hash, left_times) in &left.observations_by_value {
        let Some(left_first) = left_times.iter().min().copied() else {
            continue;
        };
        for (right_hash, right_times) in &right.observations_by_value {
            let Some(right_first) = right_times.iter().min().copied() else {
                continue;
            };
            let activation = left_first.max(right_first);
            let candidate = if left_hash == right_hash {
                left_times
                    .iter()
                    .chain(right_times)
                    .copied()
                    .filter(|observed_at| *observed_at >= activation)
                    .filter(|observed_at| {
                        !identity_edge_is_exempt(
                            exemptions,
                            left_user,
                            right_user,
                            kind,
                            left_hash,
                            *observed_at,
                        )
                    })
                    .min()
            } else {
                Some(activation)
            };
            if let Some(candidate) = candidate {
                earliest = Some(
                    earliest.map_or(candidate, |existing: DateTime<Utc>| existing.min(candidate)),
                );
            }
        }
    }
    earliest
}

fn reviewable_shared_identity(team_count: usize) -> bool {
    (2..=SHARED_IDENTITY_MAX_TEAMS).contains(&team_count)
}

fn identity_evidence_key(prefix: &str, hash: &[u8]) -> String {
    format!("{prefix}:{}", hex::encode(hash))
}

fn user_evidence_key(prefix: &str, user_id: Uuid) -> String {
    format!("{prefix}:user:{user_id}")
}

fn push_candidate(
    candidates: &mut Candidates,
    participation_id: i32,
    ty: SuspicionType,
    evidence_key: String,
    observed_at: DateTime<Utc>,
) {
    candidates
        .entry((participation_id, ty.kind(), evidence_key))
        .and_modify(|existing| *existing = (*existing).min(observed_at))
        .or_insert(observed_at);
}

fn session_concurrency_at(observations: &[&Observation]) -> Option<DateTime<Utc>> {
    let mut sessions = observations
        .iter()
        .filter_map(|observation| {
            observation
                .broad_network_hash
                .as_ref()
                .map(|network| (observation.observed_at_utc, network))
        })
        .collect::<Vec<_>>();
    sessions.sort_by_key(|session| session.0);

    let window = Duration::minutes(SESSION_WINDOW_MINUTES);
    let mut pair_times = Vec::new();
    for left in 0..sessions.len() {
        for right in (left + 1)..sessions.len() {
            if sessions[right].0 - sessions[left].0 > window {
                break;
            }
            if sessions[left].1 != sessions[right].1 {
                pair_times.push(sessions[right].0);
            }
        }
    }
    pair_times.sort_unstable();
    pair_times
        .get(SESSION_CONCURRENCY_MIN_OCCURRENCES - 1)
        .copied()
}

fn submission_ip_is_unknown(
    submit_hash: &[u8],
    submitted_at: DateTime<Utc>,
    observations: &[&Observation],
) -> bool {
    let mut has_baseline = false;
    for observation in observations {
        if observation.kind != "Ip" || observation.observed_at_utc > submitted_at {
            continue;
        }
        has_baseline = true;
        if observation.value_hash == submit_hash {
            return false;
        }
    }
    has_baseline
}

fn add_group_candidates(
    candidates: &mut Candidates,
    groups: &BTreeMap<Vec<u8>, IdentityGroup>,
    exemptions: &IdentityExemptions,
    identity_kind: &str,
    ty: SuspicionType,
    evidence_prefix: &str,
    suppress_large_groups: bool,
) {
    for (hash, group) in groups {
        let shared = if suppress_large_groups {
            reviewable_shared_identity(group.team_count())
        } else {
            group.team_count() >= 2
        };
        if !shared || group.members.len() < 2 {
            continue;
        }
        let key = identity_evidence_key(evidence_prefix, hash);
        let members = group.members.iter().collect::<Vec<_>>();
        for left in 0..members.len() {
            for right in (left + 1)..members.len() {
                let (left_user, left_member) = members[left];
                let (right_user, right_member) = members[right];
                if left_member.team_id == right_member.team_id {
                    continue;
                }
                let Some(observed_at) = earliest_unexempt_edge(
                    exemptions,
                    *left_user,
                    left_member,
                    *right_user,
                    right_member,
                    identity_kind,
                ) else {
                    continue;
                };
                push_candidate(
                    candidates,
                    left_member.participation_id,
                    ty,
                    key.clone(),
                    observed_at,
                );
                push_candidate(
                    candidates,
                    right_member.participation_id,
                    ty,
                    key.clone(),
                    observed_at,
                );
            }
        }
    }
}

fn add_same_team_group_candidates(
    candidates: &mut Candidates,
    groups: &BTreeMap<(i32, Vec<u8>), IdentityGroup>,
    exemptions: &IdentityExemptions,
) {
    for ((_, hash), group) in groups {
        if group.members.len() < 2 {
            continue;
        }
        let key = identity_evidence_key("shared-ip", hash);
        let members = group.members.iter().collect::<Vec<_>>();
        for left in 0..members.len() {
            for right in (left + 1)..members.len() {
                let (left_user, left_member) = members[left];
                let (right_user, right_member) = members[right];
                let Some(observed_at) = earliest_unexempt_edge(
                    exemptions,
                    *left_user,
                    left_member,
                    *right_user,
                    right_member,
                    "Ip",
                ) else {
                    continue;
                };
                push_candidate(
                    candidates,
                    left_member.participation_id,
                    SuspicionType::SharedIp,
                    key.clone(),
                    observed_at,
                );
                push_candidate(
                    candidates,
                    right_member.participation_id,
                    SuspicionType::SharedIp,
                    key.clone(),
                    observed_at,
                );
            }
        }
    }
}

fn add_final_bounded_group_candidates(
    candidates: &mut Candidates,
    groups: &BTreeMap<Vec<u8>, IdentityGroup>,
    exemptions: &IdentityExemptions,
    identity_kind: &str,
    ty: SuspicionType,
    evidence_prefix: &str,
    final_identity_snapshot: bool,
) {
    if final_identity_snapshot {
        add_group_candidates(
            candidates,
            groups,
            exemptions,
            identity_kind,
            ty,
            evidence_prefix,
            true,
        );
    }
}

/// Reconcile correlation evidence for a game. The operation is idempotent: each
/// concrete identity/user/submission becomes a stable evidence key and the
/// canonical event writer enforces uniqueness.
pub async fn run_correlation_checks(db: &DatabaseConnection, game_id: i32) -> AppResult<()> {
    run_correlation_checks_for_snapshot(db, game_id, super::detectors::ReconciliationSnapshot::Live)
        .await
}

pub(super) async fn run_correlation_checks_for_snapshot(
    db: &DatabaseConnection,
    game_id: i32,
    snapshot: super::detectors::ReconciliationSnapshot,
) -> AppResult<()> {
    let pool = db.get_postgres_connection_pool();
    let Some(window) = super::detectors::load_competitive_game_window(pool, game_id).await? else {
        return Ok(());
    };
    let (start, end) = (window.start, window.end);
    if end <= start {
        return Ok(());
    }
    let final_identity_snapshot = super::detectors::final_snapshot_ready(snapshot);

    let observations = sqlx::query_as::<_, Observation>(LOAD_OBSERVATIONS_SQL)
        .bind(game_id)
        .bind(start)
        .bind(end)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let submissions = sqlx::query_as::<_, SubmissionIdentity>(LOAD_SUBMISSION_IDENTITIES_SQL)
        .bind(game_id)
        .bind(start)
        .bind(end)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let exemption_rows = sqlx::query_as::<_, IdentityExemption>(LOAD_IDENTITY_EXEMPTIONS_SQL)
        .bind(game_id)
        .bind(start)
        .bind(end)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let mut exemptions = IdentityExemptions::new();
    for exemption in exemption_rows {
        exemptions
            .entry(canonical_user_pair(exemption.user_a, exemption.user_b))
            .or_default()
            .push(exemption);
    }

    let mut candidates = Candidates::new();
    let mut observations_by_user: HashMap<Uuid, Vec<&Observation>> = HashMap::new();
    let mut observations_by_participation: HashMap<i32, Vec<&Observation>> = HashMap::new();
    let mut exact_ips: BTreeMap<Vec<u8>, IdentityGroup> = BTreeMap::new();
    let mut fingerprints: BTreeMap<Vec<u8>, IdentityGroup> = BTreeMap::new();
    let mut subnets: BTreeMap<Vec<u8>, IdentityGroup> = BTreeMap::new();
    let mut team_ip_users: BTreeMap<(i32, Vec<u8>), IdentityGroup> = BTreeMap::new();

    for observation in &observations {
        observations_by_user
            .entry(observation.user_id)
            .or_default()
            .push(observation);
        observations_by_participation
            .entry(observation.participation_id)
            .or_default()
            .push(observation);

        let group = if observation.kind == "Ip" {
            exact_ips.entry(observation.value_hash.clone()).or_default()
        } else if observation.kind == "Fingerprint" {
            fingerprints
                .entry(observation.value_hash.clone())
                .or_default()
        } else {
            continue;
        };
        group.observe(observation);

        if observation.kind == "Ip" {
            let same_team = team_ip_users
                .entry((observation.team_id, observation.value_hash.clone()))
                .or_default();
            same_team.observe(observation);
            if let Some(subnet_hash) = &observation.subnet_group_hash {
                let subnet = subnets.entry(subnet_hash.clone()).or_default();
                subnet.observe(observation);
            }
        }
    }

    // Same-team sharing is contextual; one event is enough for each exact
    // non-exempt user pair and concrete address.
    add_same_team_group_candidates(&mut candidates, &team_ip_users, &exemptions);
    add_final_bounded_group_candidates(
        &mut candidates,
        &exact_ips,
        &exemptions,
        "Ip",
        SuspicionType::CrossTeamIp,
        "cross-team-ip",
        final_identity_snapshot,
    );
    add_group_candidates(
        &mut candidates,
        &fingerprints,
        &exemptions,
        "Fingerprint",
        SuspicionType::SharedFingerprint,
        "shared-fingerprint",
        false,
    );
    add_final_bounded_group_candidates(
        &mut candidates,
        &subnets,
        &exemptions,
        "Ip",
        SuspicionType::SubnetOverlap,
        "subnet-overlap",
        final_identity_snapshot,
    );

    for (user_id, user_observations) in &observations_by_user {
        let Some(first) = user_observations.first() else {
            continue;
        };
        let mut fingerprints = BTreeSet::new();
        let fingerprint_churn_at = user_observations.iter().find_map(|observation| {
            if observation.kind != "Fingerprint" {
                return None;
            }
            fingerprints.insert(&observation.value_hash);
            (fingerprints.len() == FINGERPRINT_CHURN_THRESHOLD)
                .then_some(observation.observed_at_utc)
        });
        if let Some(observed_at) = fingerprint_churn_at {
            push_candidate(
                &mut candidates,
                first.participation_id,
                SuspicionType::FingerprintChurn,
                user_evidence_key("fingerprint-churn", *user_id),
                observed_at,
            );
        }

        let ip_observations = user_observations
            .iter()
            .copied()
            .filter(|observation| observation.kind == "Ip")
            .collect::<Vec<_>>();
        let mut ips = BTreeSet::new();
        let ip_churn_at = ip_observations.iter().find_map(|observation| {
            ips.insert(&observation.value_hash);
            (ips.len() == IP_CHURN_THRESHOLD).then_some(observation.observed_at_utc)
        });
        if let Some(observed_at) = ip_churn_at {
            push_candidate(
                &mut candidates,
                first.participation_id,
                SuspicionType::IpChurn,
                user_evidence_key("ip-churn", *user_id),
                observed_at,
            );
        }
        if let Some(observed_at) = session_concurrency_at(&ip_observations) {
            push_candidate(
                &mut candidates,
                first.participation_id,
                SuspicionType::SessionConcurrency,
                user_evidence_key("session-concurrency", *user_id),
                observed_at,
            );
        }
    }

    // ClusteredRegistration intentionally does not emit. The per-user
    // registration/member linkage has no immutable competitive observation
    // time, so a post-end roster change could otherwise rewrite final evidence.

    // A submission address is "unknown" only when the participant already has
    // at least one immutable IP observation before that submission. Legacy rows
    // without a submit-time hash and sessions without a baseline are skipped.
    if final_identity_snapshot {
        for submission in submissions {
            let Some(submit_hash) = submission.submit_remote_ip_hash.as_ref() else {
                continue;
            };
            let baselines = observations_by_participation
                .get(&submission.participation_id)
                .map(Vec::as_slice)
                .unwrap_or_default();
            if !submission_ip_is_unknown(submit_hash, submission.submit_time_utc, baselines) {
                continue;
            }
            push_candidate(
                &mut candidates,
                submission.participation_id,
                SuspicionType::UnknownIp,
                format!("submission:{}", submission.id),
                submission.submit_time_utc,
            );
        }
    }

    let mut codes = Vec::new();
    for ((participation_id, kind, evidence_key), observed_at) in candidates {
        let Some(ty) = SuspicionType::from_kind(kind) else {
            return Err(AppError::internal("invalid correlation suspicion kind"));
        };
        super::detectors::record_with_dedup_at(
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
    use super::*;

    #[test]
    fn shared_identity_suppresses_singletons_and_large_nat_groups() {
        assert!(!reviewable_shared_identity(0));
        assert!(!reviewable_shared_identity(1));
        assert!(reviewable_shared_identity(2));
        assert!(reviewable_shared_identity(4));
        assert!(!reviewable_shared_identity(5));
        assert!(!reviewable_shared_identity(100));
    }

    #[test]
    fn bounded_ip_groups_emit_only_from_the_final_population() {
        fn group(team_count: usize, hash: &[u8]) -> IdentityGroup {
            let start = Utc::now();
            let mut group = IdentityGroup::default();
            for index in 0..team_count {
                let team_id = i32::try_from(index + 1).unwrap();
                let user_id = Uuid::from_u128(u128::try_from(index + 1).unwrap());
                group.observe(&Observation {
                    id: i64::try_from(index + 1).unwrap(),
                    user_id,
                    team_id,
                    participation_id: 100 + team_id,
                    kind: "Ip".to_string(),
                    value_hash: hash.to_vec(),
                    subnet_group_hash: None,
                    broad_network_hash: None,
                    observed_at_utc: start + Duration::seconds(i64::try_from(index).unwrap()),
                });
            }
            group
        }

        let hash = vec![0x42; 32];
        let exemptions = IdentityExemptions::new();
        let mut two_team_groups = BTreeMap::new();
        two_team_groups.insert(hash.clone(), group(2, &hash));

        let mut live_candidates = Candidates::new();
        add_final_bounded_group_candidates(
            &mut live_candidates,
            &two_team_groups,
            &exemptions,
            "Ip",
            SuspicionType::CrossTeamIp,
            "cross-team-ip",
            false,
        );
        assert!(live_candidates.is_empty());

        let mut final_two_team_candidates = Candidates::new();
        add_final_bounded_group_candidates(
            &mut final_two_team_candidates,
            &two_team_groups,
            &exemptions,
            "Ip",
            SuspicionType::CrossTeamIp,
            "cross-team-ip",
            true,
        );
        assert_eq!(final_two_team_candidates.len(), 2);

        let mut five_team_groups = BTreeMap::new();
        five_team_groups.insert(hash.clone(), group(5, &hash));
        let mut final_five_team_candidates = Candidates::new();
        add_final_bounded_group_candidates(
            &mut final_five_team_candidates,
            &five_team_groups,
            &exemptions,
            "Ip",
            SuspicionType::CrossTeamIp,
            "cross-team-ip",
            true,
        );
        assert!(final_five_team_candidates.is_empty());
    }

    #[test]
    fn immutable_query_uses_ids_and_historical_roster() {
        assert!(LOAD_OBSERVATIONS_SQL.contains("\"IdentityObservations\""));
        assert!(LOAD_OBSERVATIONS_SQL.contains("\"UserParticipations\""));
        assert!(LOAD_OBSERVATIONS_SQL.contains("observation.user_id"));
        assert!(LOAD_OBSERVATIONS_SQL.contains("competitive_admitted_at_utc < $3"));
        assert!(LOAD_SUBMISSION_IDENTITIES_SQL.contains("competitive_admitted_at_utc < $3"));
        assert!(!LOAD_OBSERVATIONS_SQL.contains("TeamMembers"));
        assert!(!LOAD_OBSERVATIONS_SQL.contains("user_name"));
    }

    #[test]
    fn identity_keys_do_not_expose_raw_values() {
        let key = identity_evidence_key("ip", &[0xab; 32]);
        assert_eq!(key, format!("ip:{}", "ab".repeat(32)));
        assert!(key.len() <= 128);
    }

    #[test]
    fn clustered_registration_has_no_actionable_emitter() {
        let source = include_str!("correlation.rs");
        let needle = ["SuspicionType::Clustered", "Registration"].concat();
        assert!(!source.contains(&needle));
    }

    #[test]
    fn unknown_ip_waits_for_the_final_baseline_population() {
        let submitted_at = Utc::now();
        let user_id = Uuid::from_u128(1);
        let mismatch = Observation {
            id: 1,
            user_id,
            team_id: 10,
            participation_id: 20,
            kind: "Ip".to_string(),
            value_hash: vec![0x11; 32],
            subnet_group_hash: None,
            broad_network_hash: None,
            observed_at_utc: submitted_at - Duration::minutes(2),
        };
        let late_matching_baseline = Observation {
            id: 2,
            value_hash: vec![0x22; 32],
            observed_at_utc: submitted_at - Duration::minutes(1),
            ..mismatch.clone()
        };
        assert!(submission_ip_is_unknown(
            &[0x22; 32],
            submitted_at,
            &[&mismatch]
        ));
        assert!(!submission_ip_is_unknown(
            &[0x22; 32],
            submitted_at,
            &[&mismatch, &late_matching_baseline]
        ));
    }
}

#[cfg(test)]
#[path = "correlation_tests.rs"]
mod temporal_tests;
