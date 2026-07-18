//! Suspicion detectors (behavioral rules + fingerprint/IP correlation).
use super::*;

// ─────────────────────────────────────────────────────────────────────────────
// Behavioral-rule thresholds (mirrors the constants in RSCTF
// `CheatReportController` Checks 6/8/A/B/H, adapted to the per-participation model)
// ─────────────────────────────────────────────────────────────────────────────

/// Burst: this many distinct challenges solved …
const BURST_MIN_SOLVES: usize = 3;
/// … within this window (seconds) trips [`SuspicionType::Burst`].
const BURST_WINDOW_SECS: i64 = 60;
/// HighWrongRate needs at least this many wrong answers before it can fire
/// (absolute floor, so a handful of typos never trips a Strong-tier rule).
const HIGH_WRONG_MIN: usize = 40;
/// … and the wrong:accepted ratio must be at least this (brute-force shape).
const HIGH_WRONG_RATIO: i64 = 10;
/// ZeroWrongAttempts is suppressed unless the challenge has at least this many
/// distinct solvers (mirrors RSCTF's `challengeSolveCount >= 5` gate — a
/// perfect first try only implicates on a challenge others struggled with).
const ZERO_WRONG_MIN_SOLVERS: usize = 5;
/// Hoarding: solved this long (seconds) after the instance's last container
/// operation (a destroy, in the fire case) — RSCTF uses 60 minutes.
const HOARDING_MIN_GAP_SECS: i64 = 60 * 60;
const MAX_EVIDENCE_KEY_BYTES: usize = 128;

fn valid_evidence_key(evidence_key: &str) -> bool {
    !evidence_key.trim().is_empty() && evidence_key.len() <= MAX_EVIDENCE_KEY_BYTES
}

const INSERT_SUSPICION_EVENT_SQL: &str = r#"
    WITH participant AS MATERIALIZED (
        SELECT id
          FROM "Participations"
         WHERE id = $2 AND game_id = $1
         FOR UPDATE
    ), inserted AS (
        INSERT INTO "SuspicionEvents"
            (game_id, participation_id, challenge_id, kind, evidence_key,
             score_delta, created_at)
        SELECT $1, participant.id, $3, $4, $5, $6, $7
          FROM participant
        ON CONFLICT (game_id, participation_id, kind, evidence_key) DO NOTHING
        RETURNING id
    ), updated AS (
        UPDATE "Participations" participation
           SET suspicion_score = participation.suspicion_score + $6
         WHERE participation.id = $2
           AND participation.game_id = $1
           AND EXISTS (SELECT 1 FROM inserted)
        RETURNING participation.suspicion_score
    )
    SELECT EXISTS (SELECT 1 FROM participant),
           EXISTS (SELECT 1 FROM inserted),
           (SELECT suspicion_score FROM updated)
"#;

/// Persist one rule observation and its score delta as one PostgreSQL statement.
/// The unique index installed by migration m0052 is the concurrency gate: only
/// the statement that inserts the evidence row may increment the running score.
async fn persist_suspicion_event_with_weight(
    pool: &sqlx::PgPool,
    game_id: i32,
    participation_id: i32,
    challenge_id: Option<i32>,
    ty: SuspicionType,
    evidence_key: &str,
    weight: i32,
    description: &str,
) -> AppResult<bool> {
    if !valid_evidence_key(evidence_key) {
        return Err(AppError::internal("invalid suspicion evidence key"));
    }
    let (participant_exists, inserted, new_score): (bool, bool, Option<i32>) =
        sqlx::query_as(INSERT_SUSPICION_EVENT_SQL)
            .bind(game_id)
            .bind(participation_id)
            .bind(challenge_id)
            .bind(ty.kind())
            .bind(evidence_key)
            .bind(weight)
            .bind(chrono::Utc::now())
            .fetch_one(pool)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;

    if !participant_exists {
        return Err(AppError::not_found("participation not found"));
    }
    if inserted {
        let new_score = new_score.ok_or_else(|| {
            AppError::internal("suspicion evidence was inserted without updating its score")
        })?;
        tracing::info!(
            participation_id,
            delta = weight,
            reason = description,
            new_score,
            "suspicion event recorded"
        );
    }
    Ok(inserted)
}

/// Persist one suspicion event for `ty` unless a row already exists for its
/// `(game_id, participation_id, kind, evidence_key)`, then bump the
/// participation's running score in the same SQL statement. This is the write path used by
/// [`correlate_fingerprints`] and the behavioral rules in
/// [`evaluate_submission`]. On a fresh insert the rule's `kind` is appended to
/// `codes`.
pub(super) async fn record_with_dedup(
    db: &DatabaseConnection,
    game_id: i32,
    participation_id: i32,
    challenge_id: Option<i32>,
    ty: SuspicionType,
    evidence_key: &str,
    codes: &mut Vec<i16>,
) -> AppResult<()> {
    let kind = ty.kind();
    let (weight, description) = resolve_entry(db, ty).await?;
    let inserted = persist_suspicion_event_with_weight(
        db.get_postgres_connection_pool(),
        game_id,
        participation_id,
        challenge_id,
        ty,
        evidence_key,
        weight,
        description,
    )
    .await?;

    if inserted && !codes.contains(&kind) {
        codes.push(kind);
    }
    Ok(())
}

/// Run the DB-tractable cheat-suspicion rule checks for a single flag
/// submission, persist a [`suspicion_event`] row per distinct rule that fires,
/// bump the participation's running score, and return the rule codes
/// (`SuspicionEvents.kind`) that hit.
///
/// Ported from RSCTF `GameInstanceRepository.CheckCheat` (the live flag-sharing
/// detector gated behind `FlagChecker`). Only dynamic challenges hand out
/// per-team flags, so an identical answer across teams is meaningless for a
/// static challenge — the whole check is gated on [`ChallengeType::is_dynamic`].
///
/// Rules evaluated here:
/// * **StolenFlag** — the submitted answer equals the per-team dynamic flag of a
///   *different* participation's live instance of this challenge (direct
///   `CheckCheat` port), OR — for the same phenomenon after the source instance
///   / flag context has been recycled — the identical answer was already
///   `Accepted` for this challenge by a different participation.
/// * **ZeroWrongAttempts** — a dynamic challenge solved with no prior
///   `WrongAnswer` submissions on it, suppressed on low-solver challenges
///   (RSCTF Check A).
/// * **HighWrongRate** — a brute-force-shaped wrong:accepted ratio for the
///   participation across the game (RSCTF Check H).
/// * **Burst** — `BURST_MIN_SOLVES`+ distinct challenges solved within
///   `BURST_WINDOW_SECS` (RSCTF Check 8).
/// * **WrongFlagLeakage** — a `WrongAnswer` from this participation equals
///   another participation's valid dynamic flag for this challenge (RSCTF Check
///   B).
/// * **Hoarding** — a container challenge solved `HOARDING_MIN_GAP_SECS` after
///   its instance's last container operation (RSCTF Check 6).
///
/// Submission-backed stolen flags retain one row per committed submission.
/// Aggregate behavioral rules deduplicate per challenge.
///
/// Non-DB heuristics (browser-fingerprint correlation, IP correlation) require a
/// `Log` / request-context join that rsctf does not model here — see the TODO.
pub async fn evaluate_submission(
    db: &DatabaseConnection,
    game_id: i32,
    participation_id: i32,
    submission_id: i32,
    challenge: &crate::models::data::game_challenge::Model,
    answer: &str,
) -> AppResult<Vec<i16>> {
    use crate::models::data::{flag_context, game_instance, submission};
    use crate::utils::enums::AnswerResult;
    use sea_orm::{ColumnTrait, QueryFilter};

    let challenge_id = challenge.id;
    let mut fired: Vec<SuspicionType> = Vec::new();

    // Per-team dynamic flags only exist for dynamic challenges; static
    // challenges share a single flag across all teams so an identical answer
    // proves nothing (matches the `FlagChecker` gate on `CheckCheat`). The
    // challenge is passed in (already loaded by the submit handler) rather than
    // re-fetched here — one fewer query on every submit.
    let is_dynamic = challenge.challenge_type.is_dynamic();

    if is_dynamic {
        // ── Rule: StolenFlag (live flag-context match) ───────────────────────
        // Direct port of GameInstanceRepository.CheckCheat: find a game instance
        // of this challenge belonging to a *different* participation whose
        // assigned flag equals the submitted answer. The scoping to this
        // challenge is enforced on the instance (`challenge_id`), and `flag_id
        // IN (…)` implies the flag context is non-null — mirroring the C# join
        // on `FlagContext.Flag == answer` with no `FlagContext.ChallengeId`
        // predicate.
        let matching_flag_ids: Vec<i32> = flag_context::Entity::find()
            .filter(flag_context::Column::Flag.eq(answer))
            .all(db)
            .await?
            .into_iter()
            .map(|f| f.id)
            .collect();

        let mut stolen = false;
        if !matching_flag_ids.is_empty() {
            let cross = game_instance::Entity::find()
                .filter(game_instance::Column::ChallengeId.eq(challenge_id))
                .filter(game_instance::Column::ParticipationId.ne(participation_id))
                .filter(game_instance::Column::FlagId.is_in(matching_flag_ids))
                .one(db)
                .await?;
            stolen = cross.is_some();
        }

        // ── Rule: StolenFlag (accepted-submission history) ───────────────────
        // Same phenomenon, resilient to a recycled instance / flag context: the
        // identical answer was already `Accepted` for this dynamic challenge by
        // a different participation. Every team should have a unique flag, so a
        // shared accepted string is flag sharing.
        if !stolen {
            let prior = submission::Entity::find()
                .filter(submission::Column::GameId.eq(game_id))
                .filter(submission::Column::ChallengeId.eq(challenge_id))
                .filter(submission::Column::ParticipationId.ne(participation_id))
                .filter(submission::Column::Answer.eq(answer))
                .filter(submission::Column::Status.eq(AnswerResult::Accepted))
                .one(db)
                .await?;
            stolen = prior.is_some();
        }

        if stolen {
            fired.push(SuspicionType::StolenFlag);
        }
    }

    // Fingerprint correlation (SharedFingerprint) is handled out-of-band by
    // [`correlate_fingerprints`], which groups the login fingerprints captured in
    // `log_entry`. IP-based heuristics (SharedIP / CrossTeamIP / IpChurn /
    // SessionConcurrency) would need per-request IP capture and belong to the
    // ingest-time detectors, not this per-submission evaluation.

    // Persist one SuspicionEvent per distinct rule that fired, bump the running
    // score, and return the detected `kind` codes. A repeated stolen-flag
    // submission receives its own durable incident key and remains in `codes`
    // for this observation; that prevents the same answer from being
    // reclassified as WrongFlagLeakage below.
    let mut codes: Vec<i16> = Vec::new();
    for ty in fired {
        let kind = ty.kind();
        if codes.contains(&kind) {
            continue;
        }
        let evidence_key = submission_evidence_key(submission_id);

        record_with_dedup(
            db,
            game_id,
            participation_id,
            Some(challenge_id),
            ty,
            &evidence_key,
            &mut codes,
        )
        .await?;
        if !codes.contains(&kind) {
            codes.push(kind);
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Behavioral / brute-force rules over this participation's submission
    // history (all DB-computable from `submission` + `game_instance`). Each
    // persists at most one event per stable aggregate evidence key, deduped
    // against the audit table by [`record_with_dedup`].
    // ─────────────────────────────────────────────────────────────────────────
    let subs = submission::Entity::find()
        .filter(submission::Column::GameId.eq(game_id))
        .filter(submission::Column::ParticipationId.eq(participation_id))
        .all(db)
        .await?;

    // Participation-wide accept/wrong tallies across the whole game.
    let total_accepted = subs
        .iter()
        .filter(|s| s.status == AnswerResult::Accepted)
        .count();
    let total_wrong = subs
        .iter()
        .filter(|s| s.status == AnswerResult::WrongAnswer)
        .count();

    // This challenge's accept picture for this participation.
    let earliest_accept_here = subs
        .iter()
        .filter(|s| s.challenge_id == challenge_id && s.status == AnswerResult::Accepted)
        .map(|s| s.submit_time_utc)
        .min();

    // ── Rule: ZeroWrongAttempts ──────────────────────────────────────────────
    // A dynamic challenge solved with zero wrong submissions prior to the solve.
    // Suppressed unless the challenge has a real solver base (>= MIN_SOLVERS),
    // mirroring RSCTF's `challengeSolveCount >= 5` gate. The community-easy ratio
    // suppression needs game-wide per-challenge stats and is intentionally left
    // out here.
    if is_dynamic {
        if let Some(accept_time) = earliest_accept_here {
            // Distinct solvers of this challenge across the game.
            let solver_base = submission::Entity::find()
                .filter(submission::Column::GameId.eq(game_id))
                .filter(submission::Column::ChallengeId.eq(challenge_id))
                .filter(submission::Column::Status.eq(AnswerResult::Accepted))
                .all(db)
                .await?
                .iter()
                .map(|s| s.participation_id)
                .collect::<std::collections::BTreeSet<_>>()
                .len();

            if solver_base >= ZERO_WRONG_MIN_SOLVERS {
                let wrongs_before = subs
                    .iter()
                    .filter(|s| {
                        s.challenge_id == challenge_id
                            && s.status == AnswerResult::WrongAnswer
                            && s.submit_time_utc < accept_time
                    })
                    .count();
                if wrongs_before == 0 {
                    let evidence_key = challenge_evidence_key(challenge_id);
                    record_with_dedup(
                        db,
                        game_id,
                        participation_id,
                        Some(challenge_id),
                        SuspicionType::ZeroWrongAttempts,
                        &evidence_key,
                        &mut codes,
                    )
                    .await?;
                }
            }
        }
    }

    // ── Rule: HighWrongRate ──────────────────────────────────────────────────
    // Participation-wide brute forcing: a large absolute number of wrong answers
    // that dwarfs the number of solves.
    if total_wrong >= HIGH_WRONG_MIN
        && (total_wrong as i64) >= HIGH_WRONG_RATIO * (total_accepted.max(1) as i64)
    {
        let evidence_key = challenge_evidence_key(challenge_id);
        record_with_dedup(
            db,
            game_id,
            participation_id,
            Some(challenge_id),
            SuspicionType::HighWrongRate,
            &evidence_key,
            &mut codes,
        )
        .await?;
    }

    // ── Rule: Burst ──────────────────────────────────────────────────────────
    // >= BURST_MIN_SOLVES distinct challenges solved within BURST_WINDOW_SECS —
    // automated submission or shared flags entered in one go (RSCTF Check 8).
    {
        // First-accept time per distinct challenge for this participation.
        let mut first_accept: std::collections::BTreeMap<i32, chrono::DateTime<chrono::Utc>> =
            std::collections::BTreeMap::new();
        for s in subs.iter().filter(|s| s.status == AnswerResult::Accepted) {
            first_accept
                .entry(s.challenge_id)
                .and_modify(|t| {
                    if s.submit_time_utc < *t {
                        *t = s.submit_time_utc;
                    }
                })
                .or_insert(s.submit_time_utc);
        }
        let mut times: Vec<chrono::DateTime<chrono::Utc>> = first_accept.into_values().collect();
        times.sort();

        let mut burst = false;
        for i in 0..times.len() {
            let mut count = 1usize;
            for j in (i + 1)..times.len() {
                if (times[j] - times[i]).num_seconds() <= BURST_WINDOW_SECS {
                    count += 1;
                } else {
                    break;
                }
            }
            if count >= BURST_MIN_SOLVES {
                burst = true;
                break;
            }
        }
        if burst {
            record_with_dedup(
                db,
                game_id,
                participation_id,
                Some(challenge_id),
                SuspicionType::Burst,
                GLOBAL_EVIDENCE_KEY,
                &mut codes,
            )
            .await?;
        }
    }

    // ── Rule: WrongFlagLeakage ───────────────────────────────────────────────
    // A wrong answer from this participation equals another participation's valid
    // dynamic flag for THIS challenge — they held the flag but it was not theirs
    // (leakage; distinct from an accepted steal). This shares its flag-context /
    // instance join with StolenFlag branch 1, so the same answer would otherwise
    // trip BOTH rules and double-count one steal (StolenFlag 100 + WrongFlagLeakage
    // 80). RSCTF's two checks are mutually exclusive: a submission classified as a
    // stolen flag is marked `CheatDetected` and is never re-examined by the
    // WrongFlagLeakage (Check B) branch. rsctf's judge doesn't stamp
    // `CheatDetected`, so we reproduce that exclusion here by skipping this branch
    // whenever StolenFlag already fired for this submission — one steal ⇒ exactly
    // one StolenFlag(100).
    if is_dynamic && !codes.contains(&SuspicionType::StolenFlag.kind()) {
        let submitted_wrong_here = subs.iter().any(|s| {
            s.challenge_id == challenge_id
                && s.answer.as_str() == answer
                && s.status == AnswerResult::WrongAnswer
        });
        if submitted_wrong_here {
            let matching_flag_ids: Vec<i32> = flag_context::Entity::find()
                .filter(flag_context::Column::Flag.eq(answer))
                .all(db)
                .await?
                .into_iter()
                .map(|f| f.id)
                .collect();
            if !matching_flag_ids.is_empty() {
                let cross = game_instance::Entity::find()
                    .filter(game_instance::Column::ChallengeId.eq(challenge_id))
                    .filter(game_instance::Column::ParticipationId.ne(participation_id))
                    .filter(game_instance::Column::FlagId.is_in(matching_flag_ids))
                    .one(db)
                    .await?;
                if cross.is_some() {
                    let evidence_key = challenge_evidence_key(challenge_id);
                    record_with_dedup(
                        db,
                        game_id,
                        participation_id,
                        Some(challenge_id),
                        SuspicionType::WrongFlagLeakage,
                        &evidence_key,
                        &mut codes,
                    )
                    .await?;
                }
            }
        }
    }

    // ── Rule: Hoarding ───────────────────────────────────────────────────────
    // A container challenge solved well after its instance's last container
    // operation. In the fire case the instance has no live container, so that
    // last operation is a destroy (or a never-started creation): the team held
    // the flag and submitted it long after the environment went away (RSCTF Check
    // 6). rsctf does not log attachment downloads, so the download-requirement
    // variant (RSCTF NoDownload / FastSolve-Download) stays a TODO.
    let requires_container = challenge.challenge_type.is_container();
    if requires_container {
        if let Some(accept_time) = earliest_accept_here {
            let inst = game_instance::Entity::find()
                .filter(game_instance::Column::ChallengeId.eq(challenge_id))
                .filter(game_instance::Column::ParticipationId.eq(participation_id))
                .one(db)
                .await?;
            if let Some(inst) = inst {
                let hoarded = inst.container_id.is_none()
                    && !inst.is_loaded
                    && (accept_time - inst.last_container_operation).num_seconds()
                        > HOARDING_MIN_GAP_SECS;
                if hoarded {
                    let evidence_key = challenge_evidence_key(challenge_id);
                    record_with_dedup(
                        db,
                        game_id,
                        participation_id,
                        Some(challenge_id),
                        SuspicionType::Hoarding,
                        &evidence_key,
                        &mut codes,
                    )
                    .await?;
                }
            }
        }
    }

    // IP / session / honeypot / traffic-dependent rules (SharedIP, CrossTeamIP,
    // IpChurn, SessionConcurrency, Honeypot*, FlagEgress, …) need request-context
    // capture rsctf does not model here — see the note above; they stay // TODO.

    Ok(codes)
}

/// Correlate browser fingerprints captured at login to surface account sharing.
///
/// The login flow persists one [`log_entry`](crate::models::data::log_entry) row
/// per sign-in with `logger = "fingerprint"`, `message = <fingerprint>`, and
/// `user_name = <name>`. This groups those rows by fingerprint and returns every
/// fingerprint observed for **2+ distinct user names** — the account-sharing
/// signal behind [`SuspicionType::SharedFingerprint`]. The returned vec is sorted
/// by fingerprint, and each user-name list is distinct and sorted, for
/// deterministic output.
///
/// As a side effect, when such a shared fingerprint maps to users whose
/// participations in `game_id` span **two or more distinct teams** (cross-team
/// multi-accounting — as opposed to two members of one team sharing a machine, a
/// benign case), a `SharedFingerprint` [`suspicion_event`] is persisted for each
/// involved participation and its running score bumped, reusing the exact insert
/// path from [`evaluate_submission`]. Participations that already carry a
/// `SharedFingerprint` event are not double-recorded.
///
/// IP / session heuristics would need per-request IP capture and are out of
/// scope here (see the note in [`evaluate_submission`]).
pub async fn correlate_fingerprints(
    db: &DatabaseConnection,
    game_id: i32,
) -> AppResult<Vec<(String, Vec<String>)>> {
    use crate::models::data::{log_entry, team_member, user};
    use sea_orm::{ColumnTrait, QueryFilter};
    use std::collections::{BTreeMap, BTreeSet};

    // 1. Pull every login-fingerprint log row and group by fingerprint, keeping
    //    the set of DISTINCT user names seen for each.
    let rows = log_entry::Entity::find()
        .filter(log_entry::Column::Logger.eq("fingerprint"))
        .all(db)
        .await?;

    let mut by_fp: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in rows {
        if let Some(name) = row.user_name {
            if name.is_empty() || row.message.is_empty() {
                continue;
            }
            by_fp.entry(row.message).or_default().insert(name);
        }
    }

    // 2. Keep only fingerprints shared by 2+ distinct users.
    let shared: Vec<(String, Vec<String>)> = by_fp
        .into_iter()
        .filter(|(_, users)| users.len() >= 2)
        .map(|(fp, users)| (fp, users.into_iter().collect()))
        .collect();

    // 3. Side effect: escalate the cross-team subset through the same atomic
    // evidence + score path as every other detector.
    let mut codes = Vec::new();
    for (_, user_names) in &shared {
        // Resolve each shared user name to its game-`game_id` participations,
        // grouped by team. A fingerprint shared only within a single team is the
        // expected shared-machine case; only cross-team sharing is escalated.
        let mut parts_by_team: BTreeMap<i32, BTreeSet<i32>> = BTreeMap::new();
        for name in user_names {
            let account = user::Entity::find()
                .filter(user::Column::UserName.eq(name.clone()))
                .one(db)
                .await?;
            let Some(account) = account else { continue };

            let team_ids: Vec<i32> = team_member::Entity::find()
                .filter(team_member::Column::UserId.eq(account.id))
                .all(db)
                .await?
                .into_iter()
                .map(|m| m.team_id)
                .collect();
            if team_ids.is_empty() {
                continue;
            }

            let parts = participation::Entity::find()
                .filter(participation::Column::GameId.eq(game_id))
                .filter(participation::Column::TeamId.is_in(team_ids))
                .all(db)
                .await?;
            for p in parts {
                parts_by_team.entry(p.team_id).or_default().insert(p.id);
            }
        }

        // Cross-team only: users span 2+ distinct participating teams.
        if parts_by_team.len() < 2 {
            continue;
        }

        let participation_ids: BTreeSet<i32> = parts_by_team.values().flatten().copied().collect();

        for pid in participation_ids {
            record_with_dedup(
                db,
                game_id,
                pid,
                None,
                SuspicionType::SharedFingerprint,
                GLOBAL_EVIDENCE_KEY,
                &mut codes,
            )
            .await?;
        }
    }

    Ok(shared)
}

#[cfg(test)]
mod tests {
    use super::{
        persist_suspicion_event_with_weight, valid_evidence_key, SuspicionType,
        INSERT_SUSPICION_EVENT_SQL, MAX_EVIDENCE_KEY_BYTES,
    };
    use sqlx::postgres::PgPoolOptions;

    #[test]
    fn suspicion_write_is_conflict_gated_and_score_coupled() {
        assert!(INSERT_SUSPICION_EVENT_SQL
            .contains("ON CONFLICT (game_id, participation_id, kind, evidence_key) DO NOTHING"));
        assert!(INSERT_SUSPICION_EVENT_SQL.contains("WITH participant AS MATERIALIZED"));
        assert!(INSERT_SUSPICION_EVENT_SQL.contains("FOR UPDATE"));
        assert!(INSERT_SUSPICION_EVENT_SQL.contains("AND EXISTS (SELECT 1 FROM inserted)"));
        assert!(INSERT_SUSPICION_EVENT_SQL
            .contains("suspicion_score = participation.suspicion_score + $6"));
    }

    #[test]
    fn evidence_keys_are_nonempty_and_bounded() {
        assert!(!valid_evidence_key(""));
        assert!(!valid_evidence_key("   "));
        assert!(valid_evidence_key("submission:500"));
        assert!(valid_evidence_key(&"x".repeat(MAX_EVIDENCE_KEY_BYTES)));
        assert!(!valid_evidence_key(&"x".repeat(MAX_EVIDENCE_KEY_BYTES + 1)));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn concurrent_rule_retries_insert_and_score_exactly_once() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect test database");
        let schema = format!("suspicion_write_{}", uuid::Uuid::new_v4().simple());
        let setup = format!(
            r#"
            CREATE SCHEMA "{schema}";
            CREATE TABLE "{schema}"."Participations" (
                id INTEGER PRIMARY KEY,
                game_id INTEGER NOT NULL,
                suspicion_score INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE "{schema}"."SuspicionEvents" (
                id BIGSERIAL PRIMARY KEY,
                game_id INTEGER NOT NULL,
                participation_id INTEGER NOT NULL,
                challenge_id INTEGER,
                kind SMALLINT NOT NULL,
                evidence_key TEXT NOT NULL,
                score_delta INTEGER,
                created_at TIMESTAMPTZ NOT NULL
            );
            CREATE UNIQUE INDEX ux_suspicionevents_incident
              ON "{schema}"."SuspicionEvents"
                 (game_id, participation_id, kind, evidence_key);
            "#
        );
        sqlx::raw_sql(&setup)
            .execute(&admin)
            .await
            .expect("create isolated suspicion schema");

        let search_path_schema = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(16)
            .after_connect(move |connection, _metadata| {
                let statement = format!(r#"SET search_path TO "{search_path_schema}""#);
                Box::pin(async move {
                    sqlx::query(&statement).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await
            .expect("connect isolated suspicion pool");
        sqlx::query(
            r#"INSERT INTO "Participations" (id, game_id, suspicion_score)
               VALUES (10, 1, 0)"#,
        )
        .execute(&pool)
        .await
        .expect("insert participation");

        let tasks = (0..64)
            .map(|_| {
                let pool = pool.clone();
                tokio::spawn(async move {
                    persist_suspicion_event_with_weight(
                        &pool,
                        1,
                        10,
                        Some(20),
                        SuspicionType::StolenFlag,
                        "submission:500",
                        100,
                        "concurrent test",
                    )
                    .await
                    .expect("persist concurrent suspicion event")
                })
            })
            .collect::<Vec<_>>();

        let mut inserted = 0usize;
        for task in tasks {
            inserted += usize::from(task.await.expect("join suspicion writer"));
        }
        assert_eq!(inserted, 1);

        let second_incident = persist_suspicion_event_with_weight(
            &pool,
            1,
            10,
            Some(20),
            SuspicionType::StolenFlag,
            "submission:501",
            100,
            "distinct incident",
        )
        .await
        .expect("persist distinct suspicion event");
        assert!(second_incident);

        let event_count: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "SuspicionEvents""#)
            .fetch_one(&pool)
            .await
            .expect("count suspicion events");
        let score: i32 =
            sqlx::query_scalar(r#"SELECT suspicion_score FROM "Participations" WHERE id = 10"#)
                .fetch_one(&pool)
                .await
                .expect("read suspicion score");
        let deltas: Vec<i32> = sqlx::query_scalar(
            r#"SELECT score_delta FROM "SuspicionEvents" ORDER BY evidence_key"#,
        )
        .fetch_all(&pool)
        .await
        .expect("read persisted score deltas");
        assert_eq!(event_count, 2);
        assert_eq!(deltas, vec![100, 100]);
        assert_eq!(score, 200);

        pool.close().await;
        let teardown = format!(r#"DROP SCHEMA "{schema}" CASCADE"#);
        sqlx::query(&teardown)
            .execute(&admin)
            .await
            .expect("drop isolated suspicion schema");
    }
}
