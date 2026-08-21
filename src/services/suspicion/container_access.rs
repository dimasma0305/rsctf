//! Container-access suspicion correlation.
//!
//! Historical correlation is deliberately submission-anchored: an accepted
//! submission must carry the immutable container UUID and submit-time IP hash
//! captured by its grading transaction. Legacy rows without that evidence are
//! skipped instead of being correlated to the current account IP or to an
//! earlier container generation.

use std::collections::HashMap;
#[cfg(test)]
use std::net::IpAddr;

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::utils::enums::{AnswerResult, ChallengeType};
use crate::utils::error::{AppError, AppResult};

use super::*;

const INSTANT_SUBMIT_THRESHOLD_SECS: i64 = 3;
const DELAYED_SUBMISSION_THRESHOLD_MINS: i64 = 60;

const CANONICAL_CONTAINER_SUBMISSIONS_SQL: &str = r#"
    SELECT submission.id,
           submission.participation_id,
           submission.challenge_id,
           submission.user_id,
           submission.submit_time_utc,
           submission.submit_remote_ip_hash,
           submission.container_id
      FROM "Submissions" submission
      JOIN "FirstSolves" first_solve
        ON first_solve.submission_id = submission.id
       AND first_solve.participation_id = submission.participation_id
       AND first_solve.challenge_id = submission.challenge_id
      JOIN "GameChallenges" challenge
        ON challenge.id = submission.challenge_id
       AND challenge.game_id = submission.game_id
      JOIN "Games" game ON game.id = submission.game_id
     WHERE submission.game_id = $1
       AND submission.status = $2
       AND submission.submit_time_utc >= game.start_time_utc
       AND submission.submit_time_utc < game.end_time_utc
       AND challenge."Type" = ANY($3)
     ORDER BY submission.submit_time_utc, submission.id
"#;

const COMPETITIVE_ACCESS_EVENTS_SQL: &str = r#"
    SELECT access.challenge_id,
           access.container_owner_participation_id,
           access.container_id,
           access.accessing_user_id,
           access.remote_ip_hash,
           access.connected_at_utc
      FROM "ContainerAccessEvents" access
      JOIN "Games" game ON game.id = access.game_id
     WHERE access.game_id = $1
       AND access.is_monitor = FALSE
       AND access.connected_at_utc >= game.start_time_utc
       AND access.connected_at_utc < game.end_time_utc
     ORDER BY access.connected_at_utc, access.id
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
struct SubmissionObservation {
    id: i32,
    participation_id: i32,
    challenge_id: i32,
    user_id: Option<Uuid>,
    submit_time_utc: DateTime<Utc>,
    submit_remote_ip_hash: Option<Vec<u8>>,
    container_id: Option<Uuid>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AccessObservation {
    challenge_id: i32,
    container_owner_participation_id: i32,
    container_id: Uuid,
    accessing_user_id: Option<Uuid>,
    remote_ip_hash: Option<Vec<u8>>,
    connected_at_utc: DateTime<Utc>,
}

type ContainerGeneration = (i32, i32, Uuid);

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

fn aggregate_snapshot_is_final(snapshot: super::detectors::ReconciliationSnapshot) -> bool {
    super::detectors::final_snapshot_ready(snapshot)
}

/// Canonicalize an IP for audit lookup and tests. Invalid or unspecified input
/// is not evidence and maps to the empty string.
#[cfg(test)]
pub(super) fn norm_ip(ip: &str) -> String {
    let Ok(parsed) = ip.trim().parse::<IpAddr>() else {
        return String::new();
    };
    if parsed.is_unspecified() {
        return String::new();
    }
    crate::services::anti_cheat::normalize_ip(parsed)
}

fn matching_generation<'a>(
    submission: &SubmissionObservation,
    events: &'a [AccessObservation],
) -> Vec<&'a AccessObservation> {
    let Some(container_id) = submission.container_id else {
        return Vec::new();
    };
    events
        .iter()
        .filter(|event| {
            event.challenge_id == submission.challenge_id
                && event.container_owner_participation_id == submission.participation_id
                && event.container_id == container_id
                && event.connected_at_utc <= submission.submit_time_utc
        })
        .collect()
}

fn signals_for_submission(
    submission: &SubmissionObservation,
    generation_events: &[AccessObservation],
) -> Vec<SuspicionType> {
    // A legacy NULL container UUID cannot be tied to one runtime generation.
    // Selecting the earliest access across restarts would manufacture timing
    // evidence, so it is intentionally non-actionable.
    if submission.container_id.is_none() {
        return Vec::new();
    }
    let Some(user_id) = submission.user_id else {
        return Vec::new();
    };
    let rows = matching_generation(submission, generation_events);
    if rows.is_empty() {
        return Vec::new();
    }

    let submitter_rows: Vec<&AccessObservation> = rows
        .iter()
        .copied()
        .filter(|event| event.accessing_user_id == Some(user_id))
        .collect();
    let mut signals = Vec::with_capacity(2);

    if let Some(first_access) = submitter_rows
        .iter()
        .map(|event| event.connected_at_utc)
        .min()
    {
        let latency = (submission.submit_time_utc - first_access).max(Duration::zero());
        if latency > Duration::minutes(DELAYED_SUBMISSION_THRESHOLD_MINS) {
            signals.push(SuspicionType::DelayedSolveSubmission);
        } else if latency < Duration::seconds(INSTANT_SUBMIT_THRESHOLD_SECS) {
            signals.push(SuspicionType::InstantSubmitAfterAccess);
        }
    }

    // Do not emit SubmitterNeverAccessedContainer. ContainerAccessEvents are a
    // best-effort forensic stream; a teammate's positive event cannot prove
    // that the submitter's event was absent rather than dropped. Historical
    // rows retain their rule identity, but new absence-derived evidence is
    // intentionally telemetry-only.

    // The submit hash is immutable evidence captured by the submission
    // transaction. Never substitute AspNetUsers.ip: that field is mutable and
    // represents a later/last login, not this submission.
    if let Some(submit_hash) = submission
        .submit_remote_ip_hash
        .as_deref()
        .filter(|hash| hash.len() == 32)
    {
        let has_access_hash = submitter_rows
            .iter()
            .filter_map(|event| event.remote_ip_hash.as_deref())
            .any(|hash| hash == submit_hash);
        let has_comparable_access = submitter_rows.iter().any(|event| {
            event
                .remote_ip_hash
                .as_deref()
                .is_some_and(|hash| hash.len() == 32)
        });
        if has_comparable_access && !has_access_hash {
            signals.push(SuspicionType::AccessIpMismatchAtSubmission);
        }
    }

    signals
}

async fn load_submissions(
    pool: &sqlx::PgPool,
    game_id: i32,
) -> AppResult<Vec<SubmissionObservation>> {
    sqlx::query_as::<
        _,
        (
            i32,
            i32,
            i32,
            Option<Uuid>,
            DateTime<Utc>,
            Option<Vec<u8>>,
            Option<Uuid>,
        ),
    >(CANONICAL_CONTAINER_SUBMISSIONS_SQL)
    .bind(game_id)
    .bind(AnswerResult::Accepted as i16)
    .bind(
        &[
            ChallengeType::StaticContainer as i16,
            ChallengeType::DynamicContainer as i16,
            ChallengeType::AttackDefense as i16,
            ChallengeType::KingOfTheHill as i16,
        ][..],
    )
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.into_iter()
            .map(
                |(
                    id,
                    participation_id,
                    challenge_id,
                    user_id,
                    submit_time_utc,
                    submit_remote_ip_hash,
                    container_id,
                )| SubmissionObservation {
                    id,
                    participation_id,
                    challenge_id,
                    user_id,
                    submit_time_utc,
                    submit_remote_ip_hash,
                    container_id,
                },
            )
            .collect()
    })
    .map_err(database_error)
}

async fn load_access_events(
    st: &SharedState,
    game_id: i32,
) -> AppResult<HashMap<ContainerGeneration, Vec<AccessObservation>>> {
    let rows = sqlx::query_as::<_, (i32, i32, Uuid, Option<Uuid>, Option<Vec<u8>>, DateTime<Utc>)>(
        COMPETITIVE_ACCESS_EVENTS_SQL,
    )
    .bind(game_id)
    .fetch_all(st.pg())
    .await
    .map_err(database_error)?;

    let mut by_generation: HashMap<ContainerGeneration, Vec<AccessObservation>> = HashMap::new();
    for (
        challenge_id,
        owner_id,
        container_id,
        accessing_user_id,
        remote_ip_hash,
        connected_at_utc,
    ) in rows
    {
        by_generation
            .entry((challenge_id, owner_id, container_id))
            .or_default()
            .push(AccessObservation {
                challenge_id,
                container_owner_participation_id: owner_id,
                container_id,
                accessing_user_id,
                remote_ip_hash,
                connected_at_utc,
            });
    }
    Ok(by_generation)
}

/// Reconcile accepted container submissions against access events from the
/// exact container generation captured by each submission.
pub async fn run_container_access_checks(st: &SharedState, game_id: i32) -> AppResult<()> {
    run_container_access_checks_for_snapshot(
        st,
        game_id,
        super::detectors::ReconciliationSnapshot::Live,
    )
    .await
}

pub(super) async fn run_container_access_checks_for_snapshot(
    st: &SharedState,
    game_id: i32,
    snapshot: super::detectors::ReconciliationSnapshot,
) -> AppResult<()> {
    // Earliest access and "no matching hash" are non-monotonic while a
    // pre-submit access transaction can still arrive. Immutable suspicion
    // events may therefore be emitted only from the barrier-backed final set.
    if !aggregate_snapshot_is_final(snapshot) {
        return Ok(());
    }
    let submissions = load_submissions(st.pg(), game_id).await?;
    if submissions.is_empty() {
        return Ok(());
    }
    let access_by_generation = load_access_events(st, game_id).await?;
    let mut codes = Vec::new();

    for submission in &submissions {
        let Some(container_id) = submission.container_id else {
            continue;
        };
        let key = (
            submission.challenge_id,
            submission.participation_id,
            container_id,
        );
        let Some(events) = access_by_generation.get(&key) else {
            continue;
        };
        let evidence_key = submission_evidence_key(submission.id);
        for signal in signals_for_submission(submission, events) {
            super::detectors::record_with_dedup_at(
                &st.db,
                game_id,
                submission.participation_id,
                Some(submission.challenge_id),
                signal,
                &evidence_key,
                submission.submit_time_utc,
                &mut codes,
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        aggregate_snapshot_is_final, norm_ip, signals_for_submission, AccessObservation,
        SubmissionObservation, CANONICAL_CONTAINER_SUBMISSIONS_SQL, COMPETITIVE_ACCESS_EVENTS_SQL,
    };
    use crate::models::internal::configs::AppConfig;
    use crate::services::suspicion::detectors::ReconciliationSnapshot;
    use crate::services::suspicion::SuspicionType;
    use chrono::{Duration, TimeZone, Utc};
    use uuid::Uuid;

    fn base() -> (SubmissionObservation, AccessObservation) {
        let user_id = Uuid::new_v4();
        let container_id = Uuid::new_v4();
        let submit_time = Utc.with_ymd_and_hms(2020, 1, 1, 1, 0, 0).unwrap();
        (
            SubmissionObservation {
                id: 1,
                participation_id: 10,
                challenge_id: 20,
                user_id: Some(user_id),
                submit_time_utc: submit_time,
                submit_remote_ip_hash: Some(vec![7; 32]),
                container_id: Some(container_id),
            },
            AccessObservation {
                challenge_id: 20,
                container_owner_participation_id: 10,
                container_id,
                accessing_user_id: Some(user_id),
                remote_ip_hash: Some(vec![7; 32]),
                connected_at_utc: submit_time - Duration::minutes(10),
            },
        )
    }

    #[test]
    fn only_the_submissions_container_generation_is_correlated() {
        let (submission, current_generation) = base();
        let mut old_generation = current_generation.clone();
        old_generation.container_id = Uuid::new_v4();
        old_generation.connected_at_utc = submission.submit_time_utc - Duration::hours(2);

        assert!(signals_for_submission(&submission, &[old_generation.clone()]).is_empty());
        let signals =
            signals_for_submission(&submission, &[old_generation.clone(), current_generation]);
        assert!(!signals.contains(&SuspicionType::DelayedSolveSubmission));

        let mut legacy = submission.clone();
        legacy.container_id = None;
        assert!(signals_for_submission(&legacy, &[]).is_empty());
    }

    #[test]
    fn timing_thresholds_are_strict_at_exact_boundaries() {
        let (submission, mut access) = base();
        access.connected_at_utc = submission.submit_time_utc - Duration::minutes(60);
        assert!(!signals_for_submission(&submission, &[access.clone()])
            .contains(&SuspicionType::DelayedSolveSubmission));

        access.connected_at_utc =
            submission.submit_time_utc - Duration::minutes(60) - Duration::milliseconds(1);
        assert!(signals_for_submission(&submission, &[access.clone()])
            .contains(&SuspicionType::DelayedSolveSubmission));

        access.connected_at_utc = submission.submit_time_utc - Duration::seconds(3);
        assert!(!signals_for_submission(&submission, &[access.clone()])
            .contains(&SuspicionType::InstantSubmitAfterAccess));

        access.connected_at_utc =
            submission.submit_time_utc - Duration::seconds(3) + Duration::milliseconds(1);
        assert!(signals_for_submission(&submission, &[access])
            .contains(&SuspicionType::InstantSubmitAfterAccess));
    }

    #[test]
    fn aggregate_rules_require_explicit_barrier_backed_final_authority() {
        assert!(!aggregate_snapshot_is_final(ReconciliationSnapshot::Live));
        assert!(aggregate_snapshot_is_final(
            ReconciliationSnapshot::BarrierBackedFinal
        ));
    }

    #[test]
    fn late_earlier_matching_access_can_invalidate_a_live_partial_signal_set() {
        let (submission, mut near_submit) = base();
        near_submit.connected_at_utc = submission.submit_time_utc - Duration::seconds(1);
        near_submit.remote_ip_hash = Some(vec![9; 32]);
        let partial = signals_for_submission(&submission, &[near_submit.clone()]);
        assert!(partial.contains(&SuspicionType::InstantSubmitAfterAccess));
        assert!(partial.contains(&SuspicionType::AccessIpMismatchAtSubmission));

        let mut late_matching = near_submit.clone();
        late_matching.connected_at_utc = submission.submit_time_utc - Duration::minutes(10);
        late_matching.remote_ip_hash = submission.submit_remote_ip_hash.clone();
        let completed = signals_for_submission(&submission, &[near_submit.clone(), late_matching]);
        assert!(completed.is_empty());

        let mut late_much_earlier = near_submit.clone();
        late_much_earlier.connected_at_utc =
            submission.submit_time_utc - Duration::minutes(60) - Duration::milliseconds(1);
        let changed = signals_for_submission(&submission, &[near_submit, late_much_earlier]);
        assert!(changed.contains(&SuspicionType::DelayedSolveSubmission));
        assert!(!changed.contains(&SuspicionType::InstantSubmitAfterAccess));
    }

    #[test]
    fn ip_mismatch_uses_immutable_hash_and_normalizes_mapped_ipv4() {
        let mut config = AppConfig::default();
        config.identity_hash_key = "test-identity-key-with-more-than-32-bytes".to_string();
        let plain = crate::services::anti_cheat::hash_ip_identity(&config, "192.0.2.7")
            .expect("plain IPv4");
        let mapped = crate::services::anti_cheat::hash_ip_identity(&config, "::ffff:192.0.2.7")
            .expect("mapped IPv4");
        assert_eq!(plain.normalized, "192.0.2.7");
        assert_eq!(plain.exact, mapped.exact);
        assert_eq!(norm_ip(" ::FFFF:192.0.2.7 "), "192.0.2.7");

        let (mut submission, mut access) = base();
        submission.submit_remote_ip_hash = Some(plain.exact);
        access.remote_ip_hash = Some(mapped.exact);
        assert!(!signals_for_submission(&submission, &[access.clone()])
            .contains(&SuspicionType::AccessIpMismatchAtSubmission));

        access.remote_ip_hash = Some(vec![9; 32]);
        assert!(signals_for_submission(&submission, &[access])
            .contains(&SuspicionType::AccessIpMismatchAtSubmission));
    }

    #[test]
    fn current_user_ip_cannot_enter_the_correlation() {
        let (mut submission, mut access) = base();
        submission.submit_remote_ip_hash = None;
        access.remote_ip_hash = Some(vec![99; 32]);
        assert!(!signals_for_submission(&submission, &[access])
            .contains(&SuspicionType::AccessIpMismatchAtSubmission));
    }

    #[test]
    fn missing_submitter_event_is_not_actionable_best_effort_evidence() {
        let (submission, mut teammate_access) = base();
        teammate_access.accessing_user_id = Some(Uuid::new_v4());
        let signals = signals_for_submission(&submission, &[teammate_access]);
        assert!(!signals.contains(&SuspicionType::SubmitterNeverAccessedContainer));
        assert!(signals.is_empty());
    }

    #[test]
    fn replayed_accepted_rows_are_excluded_by_canonical_first_solve_identity() {
        assert!(CANONICAL_CONTAINER_SUBMISSIONS_SQL.contains("JOIN \"FirstSolves\""));
        assert!(CANONICAL_CONTAINER_SUBMISSIONS_SQL
            .contains("first_solve.submission_id = submission.id"));
        assert!(CANONICAL_CONTAINER_SUBMISSIONS_SQL
            .contains("first_solve.participation_id = submission.participation_id"));
        assert!(CANONICAL_CONTAINER_SUBMISSIONS_SQL
            .contains("first_solve.challenge_id = submission.challenge_id"));
        assert!(CANONICAL_CONTAINER_SUBMISSIONS_SQL
            .contains("submission.submit_time_utc >= game.start_time_utc"));
        assert!(CANONICAL_CONTAINER_SUBMISSIONS_SQL
            .contains("submission.submit_time_utc < game.end_time_utc"));
        assert!(COMPETITIVE_ACCESS_EVENTS_SQL
            .contains("access.connected_at_utc >= game.start_time_utc"));
        assert!(COMPETITIVE_ACCESS_EVENTS_SQL.contains("access.is_monitor = FALSE"));
        assert!(
            COMPETITIVE_ACCESS_EVENTS_SQL.contains("access.connected_at_utc < game.end_time_utc")
        );
        assert_ne!(
            super::submission_evidence_key(1),
            super::submission_evidence_key(2)
        );
    }
}
