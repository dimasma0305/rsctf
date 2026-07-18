//! services/suspicion/container_access.rs — container-access cheat detectors.
//!
//! Ported from RSCTF `Services/ContainerAccessSubmissionDetector.cs`. For each
//! ACCEPTED submission on a container challenge, correlate the submitter's
//! [`container_access_event`](crate::models::data::container_access_event) rows —
//! the proxy opens of their OWN team's container of that challenge — against the
//! solve and raise:
//!
//! * [`SuspicionType::DelayedSolveSubmission`] — the submitter personally opened
//!   the container > 60 min before submitting (held the flag / late relay).
//! * [`SuspicionType::InstantSubmitAfterAccess`] — submitted < 3 s after their
//!   first access (automated solver pipeline). Mutually exclusive with Delayed,
//!   exactly like RSCTF's `if … else if …`.
//! * [`SuspicionType::SubmitterNeverAccessedContainer`] — the submitter never
//!   opened the container but a teammate did (a teammate solved and passed the
//!   flag). Separate from the timing branch.
//! * [`SuspicionType::AccessIpMismatchAtSubmission`] — the submitter's submit-time
//!   IP is not among the IPs they used to access the container. Separate check.
//!
//! Thresholds mirror `CheatDetectionConfig` (`InstantSubmitThresholdSeconds = 3`,
//! `DelayedSubmissionThresholdMinutes = 60`). Sweep signals deduplicate per
//! challenge via [`super::detectors::record_with_dedup`].
//!
//! ## Cross-team access
//! [`SuspicionType::CrossTeamContainerAccess`] is raised at access time, not at
//! submission time — see [`record_cross_team_container_access`], called from
//! [`crate::controllers::proxy`].

use super::*;
use uuid::Uuid;

use crate::app_state::SharedState;

/// `CheatDetectionConfig.InstantSubmitThresholdSeconds` — a submitter solving
/// within this many seconds of their first proxy access looks scripted.
const INSTANT_SUBMIT_THRESHOLD_SECS: i64 = 3;
/// `CheatDetectionConfig.DelayedSubmissionThresholdMinutes` — a solve arriving
/// this many minutes after the submitter's first proxy access looks like a held
/// / relayed flag.
const DELAYED_SUBMISSION_THRESHOLD_MINS: i64 = 60;

/// Raise [`SuspicionType::CrossTeamContainerAccess`] against the ACCESSOR
/// participation (RSCTF `ContainerAccessLogger` raises this at access time — the
/// cheater reached into another team's container). Deduped per
/// `(game, participation)` like every other signal, so repeated cross-team opens
/// score once. Exposed for [`crate::controllers::proxy`], which resolves the
/// accessor/owner participations on a proxy open.
pub async fn record_cross_team_container_access(
    db: &DatabaseConnection,
    game_id: i32,
    accessor_participation_id: i32,
    challenge_id: Option<i32>,
) -> AppResult<()> {
    let mut codes: Vec<i16> = Vec::new();
    let evidence_key = challenge_id
        .map(challenge_evidence_key)
        .unwrap_or_else(|| GLOBAL_EVIDENCE_KEY.to_string());
    super::detectors::record_with_dedup(
        db,
        game_id,
        accessor_participation_id,
        challenge_id,
        SuspicionType::CrossTeamContainerAccess,
        &evidence_key,
        &mut codes,
    )
    .await
}

/// Run the submission-time container-access cheat checks for one game: sweep
/// every accepted submission on a container challenge and raise the four
/// access-correlated signals. Ported from
/// `ContainerAccessSubmissionDetector.RunChecks`, adapted from RSCTF's
/// per-submission call to a per-game sweep (the cheat-report entry point).
pub async fn run_container_access_checks(st: &SharedState, game_id: i32) -> AppResult<()> {
    use crate::models::data::{container_access_event, game_challenge, submission, user};
    use crate::utils::enums::AnswerResult;
    use sea_orm::{ColumnTrait, QueryFilter};
    use std::collections::{HashMap, HashSet};

    let db = &st.db;

    // Container challenges are the only ones that yield ContainerAccessEvent rows
    // to correlate; everything else is skipped up front.
    let challenges = game_challenge::Entity::find()
        .filter(game_challenge::Column::GameId.eq(game_id))
        .all(db)
        .await?;
    let container_challenge_ids: HashSet<i32> = challenges
        .iter()
        .filter(|c| c.challenge_type.is_container())
        .map(|c| c.id)
        .collect();
    if container_challenge_ids.is_empty() {
        return Ok(());
    }

    // Accepted submissions in this game (the events we score against).
    let subs = submission::Entity::find()
        .filter(submission::Column::GameId.eq(game_id))
        .filter(submission::Column::Status.eq(AnswerResult::Accepted))
        .all(db)
        .await?;

    // Cache each submitter's resolved last-login IP (RSCTF `submission.User.IP`).
    let mut user_ip_cache: HashMap<Uuid, Option<String>> = HashMap::new();
    // Scratch vec for record_with_dedup; durable evidence-key dedup is handled
    // inside the helper, so reusing one vec across submissions is fine.
    let mut codes: Vec<i16> = Vec::new();

    for sub in &subs {
        if !container_challenge_ids.contains(&sub.challenge_id) {
            continue;
        }
        let Some(user_id) = sub.user_id else {
            continue; // No user identity — nothing access-attributable to compare.
        };
        let evidence_key = challenge_evidence_key(sub.challenge_id);

        // Access events for THIS team's OWN container of the challenge, up to the
        // submit time (RSCTF's projection: a member poking a rival's container is
        // the separate CrossTeamContainerAccess signal and must not anchor this
        // team's solve timing).
        let rows = container_access_event::Entity::find()
            .filter(container_access_event::Column::ChallengeId.eq(sub.challenge_id))
            .filter(
                container_access_event::Column::ContainerOwnerParticipationId
                    .eq(sub.participation_id),
            )
            .filter(container_access_event::Column::ConnectedAtUtc.lte(sub.submit_time_utc))
            .all(db)
            .await?;
        if rows.is_empty() {
            continue; // No access events at all (predates instrumentation, or none).
        }

        let submitter_rows: Vec<&container_access_event::Model> = rows
            .iter()
            .filter(|r| r.accessing_user_id == Some(user_id))
            .collect();
        let team_rows: Vec<&container_access_event::Model> = rows
            .iter()
            .filter(|r| r.accessing_participation_id == Some(sub.participation_id))
            .collect();

        // 1) DelayedSolveSubmission / 2) InstantSubmitAfterAccess (mutually excl.).
        if let Some(first_access) = submitter_rows.iter().map(|r| r.connected_at_utc).min() {
            // Clock-skew safety: negative latency (submit "before" first access by
            // a few ms across services) is clamped to zero, exactly like RSCTF.
            let mut latency = sub.submit_time_utc - first_access;
            if latency < chrono::Duration::zero() {
                latency = chrono::Duration::zero();
            }
            // Compare the Duration directly (not `num_minutes()`, which truncates
            // to whole minutes and would only fire at ≥ 61 min): RSCTF uses
            // `TotalMinutes > 60`, so a 60m30s solve must fire here too.
            if latency > chrono::Duration::minutes(DELAYED_SUBMISSION_THRESHOLD_MINS) {
                super::detectors::record_with_dedup(
                    db,
                    game_id,
                    sub.participation_id,
                    Some(sub.challenge_id),
                    SuspicionType::DelayedSolveSubmission,
                    &evidence_key,
                    &mut codes,
                )
                .await?;
            } else if latency < chrono::Duration::seconds(INSTANT_SUBMIT_THRESHOLD_SECS) {
                super::detectors::record_with_dedup(
                    db,
                    game_id,
                    sub.participation_id,
                    Some(sub.challenge_id),
                    SuspicionType::InstantSubmitAfterAccess,
                    &evidence_key,
                    &mut codes,
                )
                .await?;
            }
        }

        // 3) SubmitterNeverAccessedContainer — submitter user didn't, teammate did.
        if submitter_rows.is_empty() && !team_rows.is_empty() {
            super::detectors::record_with_dedup(
                db,
                game_id,
                sub.participation_id,
                Some(sub.challenge_id),
                SuspicionType::SubmitterNeverAccessedContainer,
                &evidence_key,
                &mut codes,
            )
            .await?;
        }

        // 4) AccessIpMismatchAtSubmission — only when we have submitter access
        //    events to compare against.
        if !submitter_rows.is_empty() {
            // Normalize the stored access IPs the same way the submitter IP is
            // normalized, or every dual-stack (::ffff:1.2.3.4 vs 1.2.3.4) solve
            // trips a false mismatch.
            let submitter_access_ips: HashSet<String> = submitter_rows
                .iter()
                .map(|r| norm_ip(&r.remote_ip))
                .filter(|s| !s.is_empty())
                .collect();
            if !submitter_access_ips.is_empty() {
                // RSCTF compares `submission.User.IP` (the last-login IP). rsctf
                // models the exact same value as `user.ip`, so use it directly —
                // more faithful than resolving from the buffered `Logs` sink,
                // whose just-emitted accept row may not be persisted yet.
                let submitter_ip = match user_ip_cache.get(&user_id) {
                    Some(v) => v.clone(),
                    None => {
                        let ip = user::Entity::find_by_id(user_id)
                            .one(db)
                            .await?
                            .map(|u| norm_ip(&u.ip))
                            .filter(|s| !s.is_empty() && !is_any_ip(s));
                        user_ip_cache.insert(user_id, ip.clone());
                        ip
                    }
                };
                if let Some(ip) = submitter_ip {
                    if !submitter_access_ips.contains(&ip) {
                        super::detectors::record_with_dedup(
                            db,
                            game_id,
                            sub.participation_id,
                            Some(sub.challenge_id),
                            SuspicionType::AccessIpMismatchAtSubmission,
                            &evidence_key,
                            &mut codes,
                        )
                        .await?;
                    }
                }
            }
        }
    }

    Ok(())
}

/// Canonicalize an IP for cross-source comparison: fold an IPv4-mapped IPv6
/// address (`::ffff:1.2.3.4`) down to its IPv4 form and lowercase, so a stored
/// dual-stack access IP and a plain-IPv4 login IP compare equal. Mirrors RSCTF's
/// `NormalizeIp` / `NormalizeIpString` and the sibling `correlation::norm_ip`.
fn norm_ip(ip: &str) -> String {
    let lower = ip.trim().to_ascii_lowercase();
    match lower.strip_prefix("::ffff:") {
        Some(rest) if rest.contains('.') => rest.to_string(),
        _ => lower,
    }
}

/// The all-zeros wildcard addresses RSCTF excludes (`IPAddress.Any` / `IPv6Any`).
fn is_any_ip(ip: &str) -> bool {
    matches!(ip, "0.0.0.0" | "::" | "::0")
}
