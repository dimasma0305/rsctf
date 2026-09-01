//! Source projections for the lazy suspicion-evidence review endpoint.

use super::*;

pub(super) const MAX_PRIOR_IDENTITY_HINTS: i64 = 12;
pub(super) const MAX_IDENTITY_SAMPLE_ROWS: usize = 200;
pub(super) const MAX_PAIR_SAMPLE_ROWS: i64 = 12;
pub(super) const MAX_WRONG_INTERVAL_ATTEMPTS: i64 = 256;
pub(super) const MAX_CHALLENGE_SOLVER_ROWS: i64 = 256;
pub(super) const MAX_SUBMISSION_CONTEXT_ROWS: i64 = 64;
pub(super) const BURST_SOLVE_ROWS: i64 = 3;
const CHALLENGE_LEADING_SOLVES: i64 = 2;

#[path = "cheat_evidence_correlations.rs"]
mod correlations;
pub(super) use correlations::{add_identity_source, add_pair_source};

fn capped_count_text(count: i64, maximum: i64, truncated: bool) -> String {
    if truncated {
        format!("at least {}", maximum.saturating_add(1))
    } else {
        count.to_string()
    }
}

fn challenge_snapshot_at(event: &EventEvidenceRow, ty: SuspicionType) -> Option<DateTime<Utc>> {
    if matches!(
        ty,
        SuspicionType::ZeroWrongAttempts
            | SuspicionType::AdaptiveFastSolve
            | SuspicionType::FirstBloodAnomaly
    ) {
        // These rules are emitted only by the barrier-backed final sweep and
        // intentionally use the complete competitive-window population.
        None
    } else {
        Some(event.created_at)
    }
}

pub(super) fn add_synthetic_preview(
    event: &EventEvidenceRow,
    ty: SuspicionType,
    review: &mut SuspicionEvidenceReview,
) {
    let fields = match ty {
        SuspicionType::StolenFlag => {
            "submission ID; submitting team/user; canonical flag-owning team; challenge; immutable grading timestamp"
        }
        SuspicionType::CrossTeamContainerAccess => {
            "access row and evaluation job IDs; accessing and owner teams; authenticated user; exact container generation; masked network identity"
        }
        SuspicionType::SharedIp
        | SuspicionType::SharedFingerprint
        | SuspicionType::FingerprintChurn
        | SuspicionType::IpChurn
        | SuspicionType::CrossTeamIp
        | SuspicionType::SubnetOverlap
        | SuspicionType::SessionConcurrency => {
            "masked identity hints; affected users and teams; distinct identity count; admission sources; first and last observation times"
        }
        SuspicionType::SequenceSimilarity | SuspicionType::SolutionRelay => {
            "team pair; shared canonical solves; per-challenge solve gaps; final-snapshot detector identity"
        }
        SuspicionType::Burst => {
            "three canonical challenge solves; exact solve timestamps; shortest three-solve window"
        }
        _ => {
            "submission ID and result; submitter; immutable interaction snapshots; wrong-attempt count; solver population; timing thresholds"
        }
    };
    review.sources.push(EvidenceSourceReview {
        source_type: "syntheticPreview".to_string(),
        title: "Synthetic evidence-preview schema".to_string(),
        source_id: Some(format!("demo:event:{}", event.event_id)),
        recorded_at: Some(event.created_at),
        immutable: false,
        summary: "This card lists the fields a real incident review would load. It contains no participant evidence and cannot support a sanction."
            .to_string(),
        facts: vec![
            fact("Real source would show", fields),
            fact("Detector", ty.code()),
            fact(
                "Proof status",
                "synthetic preview only — no source record was observed",
            ),
        ],
    });
}

pub(super) async fn add_cross_team_access_source(
    pool: &sqlx::PgPool,
    event: &EventEvidenceRow,
    review: &mut SuspicionEvidenceReview,
) -> AppResult<()> {
    let row = sqlx::query_as::<_, CrossTeamAccessSourceRow>(
        r#"SELECT access.id AS access_id,
                  job.id AS job_id,
                  access.container_id,
                  access.accessing_user_name,
                  access.accessing_participation_id,
                  accessing_team.name AS accessing_team_name,
                  access.container_owner_participation_id AS owner_participation_id,
                  owner_team.name AS owner_team_name,
                  challenge.title AS challenge_title,
                  access.connected_at_utc AS connected_at,
                  access.remote_ip_hash,
                  job.completed_at_utc AS completed_at
             FROM "SuspicionEvaluationOutbox" job
             JOIN "ContainerAccessEvents" access
               ON access.id = job.source_id
              AND access.game_id = job.game_id
              AND access.challenge_id = job.challenge_id
              AND access.accessing_participation_id = job.participation_id
              AND access.connected_at_utc = job.observed_at_utc
              AND access.is_monitor = FALSE
             JOIN "Participations" accessing_participation
               ON accessing_participation.id = access.accessing_participation_id
              AND accessing_participation.game_id = access.game_id
             JOIN "Teams" accessing_team ON accessing_team.id = accessing_participation.team_id
             JOIN "Participations" owner_participation
               ON owner_participation.id = access.container_owner_participation_id
              AND owner_participation.game_id = access.game_id
             JOIN "Teams" owner_team ON owner_team.id = owner_participation.team_id
             JOIN "GameChallenges" challenge
               ON challenge.id = access.challenge_id
              AND challenge.game_id = access.game_id
            WHERE job.game_id = $1
              AND job.participation_id = $2
              AND job.challenge_id = $3
              AND job.rule_kind = $4
              AND job.evidence_key = $5
              AND job.observed_at_utc = $6
              AND access.container_owner_participation_id <> access.accessing_participation_id
            ORDER BY job.id
            LIMIT 1"#,
    )
    .bind(event.game_id)
    .bind(event.participation_id)
    .bind(event.challenge_id)
    .bind(SuspicionType::CrossTeamContainerAccess.kind())
    .bind(&event.evidence_key)
    .bind(event.created_at)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(row) = row else {
        return Ok(());
    };

    review.source_status = EvidenceSourceStatus::Verified;
    review.is_direct_proof = true;
    review.sources.push(EvidenceSourceReview {
        source_type: "containerAccess".to_string(),
        title: "Cross-team proxy access".to_string(),
        source_id: Some(format!("access:{} / job:{}", row.access_id, row.job_id)),
        recorded_at: Some(row.connected_at),
        immutable: true,
        summary: "A non-monitor account opened the proxy for a container owned by another participation; the access and evaluation intent committed atomically."
            .to_string(),
        facts: vec![
            fact(
                "Accessing team",
                format!("{} (participation {})", row.accessing_team_name, row.accessing_participation_id),
            ),
            fact(
                "Container owner",
                format!("{} (participation {})", row.owner_team_name, row.owner_participation_id),
            ),
            fact(
                "Authenticated user",
                row.accessing_user_name.unwrap_or_else(|| "not captured".to_string()),
            ),
            fact("Challenge", row.challenge_title),
            fact("Container generation", row.container_id.to_string()),
            fact("Network identity", hash_hint(row.remote_ip_hash.as_deref())),
            fact(
                "Evaluation job",
                if row.completed_at.is_some() { "completed" } else { "pending" },
            ),
        ],
    });
    Ok(())
}

async fn submission_row(
    pool: &sqlx::PgPool,
    event: &EventEvidenceRow,
) -> AppResult<Option<SubmissionSourceRow>> {
    let by_id = parse_i32_key(&event.evidence_key, "submission:");
    sqlx::query_as::<_, SubmissionSourceRow>(
        r#"SELECT submission.id AS submission_id,
                  challenge.title AS challenge_title,
                  account.user_name AS submitter_name,
                  submission.status,
                  submission.submit_time_utc AS submitted_at,
                  submission.submit_remote_ip_hash AS remote_ip_hash,
                  submission.container_id,
                  submission.container_last_operation_at_submit AS container_last_operation,
                  submission.container_was_loaded_at_submit AS container_was_loaded,
                  submission.first_open_at_submit AS first_open_at,
                  submission.first_download_at_submit AS first_download_at,
                  submission.first_container_start_at_submit AS first_container_start_at
             FROM "Submissions" submission
             JOIN "GameChallenges" challenge
               ON challenge.id = submission.challenge_id
              AND challenge.game_id = submission.game_id
        LEFT JOIN "AspNetUsers" account ON account.id = submission.user_id
        LEFT JOIN "FirstSolves" first_solve
               ON first_solve.submission_id = submission.id
              AND first_solve.participation_id = submission.participation_id
              AND first_solve.challenge_id = submission.challenge_id
            WHERE submission.game_id = $1
              AND submission.participation_id = $2
              AND ($3::INTEGER IS NULL OR submission.challenge_id = $3)
              AND (($4::INTEGER IS NOT NULL AND submission.id = $4)
                   OR ($4::INTEGER IS NULL AND first_solve.submission_id IS NOT NULL))
            ORDER BY submission.submit_time_utc, submission.id
            LIMIT 1"#,
    )
    .bind(event.game_id)
    .bind(event.participation_id)
    .bind(event.challenge_id)
    .bind(by_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

fn answer_status(value: i16) -> &'static str {
    match value {
        -1 => "NotFound",
        0 => "FlagSubmitted",
        1 => "Accepted",
        2 => "WrongAnswer",
        3 => "CheatDetected",
        _ => "Unknown",
    }
}

pub(super) async fn add_submission_source(
    pool: &sqlx::PgPool,
    event: &EventEvidenceRow,
    review: &mut SuspicionEvidenceReview,
) -> AppResult<()> {
    let Some(row) = submission_row(pool, event).await? else {
        return Ok(());
    };
    let access: (i64, Option<DateTime<Utc>>, i64, Option<bool>, bool) = sqlx::query_as(
        r#"WITH target AS MATERIALIZED (
               SELECT game_id, challenge_id, participation_id, container_id,
                      user_id, submit_remote_ip_hash, submit_time_utc
                 FROM "Submissions" WHERE id = $1
           ), sampled AS MATERIALIZED (
               SELECT access.connected_at_utc, access.id, access.accessing_user_id,
                      access.remote_ip_hash, target.user_id, target.submit_remote_ip_hash
                 FROM target JOIN "ContainerAccessEvents" access
                   ON access.game_id = target.game_id
                  AND access.challenge_id = target.challenge_id
                  AND access.container_owner_participation_id = target.participation_id
                  AND access.container_id = target.container_id
                  AND access.is_monitor = FALSE
                  AND access.connected_at_utc <= target.submit_time_utc
                ORDER BY access.connected_at_utc DESC, access.id DESC LIMIT $2
           ), bounded AS MATERIALIZED (
               SELECT * FROM sampled ORDER BY connected_at_utc DESC, id DESC LIMIT $3
           )
           SELECT COUNT(*)::bigint, MIN(connected_at_utc),
                  COUNT(*) FILTER (WHERE accessing_user_id = user_id)::bigint,
                  BOOL_OR(remote_ip_hash = submit_remote_ip_hash),
                  (SELECT COUNT(*) FROM sampled) > $3
             FROM bounded"#,
    )
    .bind(row.submission_id)
    .bind(MAX_SUBMISSION_CONTEXT_ROWS.saturating_add(1))
    .bind(MAX_SUBMISSION_CONTEXT_ROWS)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .unwrap_or((0, None, 0, None, false));
    let baseline: (i64, Option<bool>, Vec<String>, bool) = sqlx::query_as(
        r#"WITH target AS MATERIALIZED (
               SELECT game_id, participation_id, submit_time_utc, submit_remote_ip_hash
                 FROM "Submissions"
                WHERE id = $1
            ), sampled AS MATERIALIZED (
               SELECT observation.id, observation.observed_at_utc,
                      observation.value_hash, observation.value_hint,
                      target.submit_remote_ip_hash
                 FROM target JOIN "IdentityObservations" observation ON
                      observation.game_id = target.game_id
                  AND observation.participation_id = target.participation_id
                  AND observation.kind = 'Ip'
                  AND observation.observed_at_utc <= target.submit_time_utc
                ORDER BY observation.observed_at_utc DESC, observation.id DESC LIMIT $2
            ), bounded AS MATERIALIZED (
               SELECT * FROM sampled ORDER BY observed_at_utc DESC, id DESC LIMIT $3
            )
            SELECT COUNT(DISTINCT value_hash)::bigint,
                   BOOL_OR(value_hash = submit_remote_ip_hash),
                   ARRAY(
                       SELECT DISTINCT hint.value_hint FROM bounded hint
                        ORDER BY hint.value_hint LIMIT $4
                   ), (SELECT COUNT(*) FROM sampled) > $3
              FROM bounded"#,
    )
    .bind(row.submission_id)
    .bind(MAX_SUBMISSION_CONTEXT_ROWS.saturating_add(1))
    .bind(MAX_SUBMISSION_CONTEXT_ROWS)
    .bind(MAX_PRIOR_IDENTITY_HINTS)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .unwrap_or((0, None, Vec::new(), false));

    if access.4 {
        review.limitations.push(format!(
            "Submission access context is capped at the latest {MAX_SUBMISSION_CONTEXT_ROWS} matching records; counts and matches below describe that bounded sample."
        ));
    }
    if baseline.3 {
        review.limitations.push(format!(
            "Pre-submit identity context is capped at the latest {MAX_SUBMISSION_CONTEXT_ROWS} observations; identity counts and matches below describe that bounded sample."
        ));
    }

    let mut facts = vec![
        fact("Submission", format!("#{}", row.submission_id)),
        fact("Challenge", &row.challenge_title),
        fact("Result", answer_status(row.status)),
        fact("Submitted at", format_time(row.submitted_at)),
        fact(
            "Submitter",
            row.submitter_name
                .unwrap_or_else(|| "not captured".to_string()),
        ),
        fact(
            "Submission network identity",
            hash_hint(row.remote_ip_hash.as_deref()),
        ),
        fact(
            if baseline.3 {
                "Known IP identities in bounded pre-submit sample"
            } else {
                "Known game IP identities before submit"
            },
            baseline.0.to_string(),
        ),
        fact(
            "Submission IP matched prior game identity",
            baseline.1.map_or("not comparable", |matched| {
                if matched {
                    "yes"
                } else if baseline.3 {
                    "not found in bounded sample"
                } else {
                    "no"
                }
            }),
        ),
    ];
    if !baseline.2.is_empty() {
        facts.push(fact("Prior masked IP hints", baseline.2.join(", ")));
    }
    if let Some(container_id) = row.container_id {
        facts.push(fact("Container generation", container_id.to_string()));
        facts.push(fact(
            if access.4 {
                "Matching access records (lower bound)"
            } else {
                "Matching access records"
            },
            capped_count_text(access.0, MAX_SUBMISSION_CONTEXT_ROWS, access.4),
        ));
        facts.push(fact(
            if access.4 {
                "Submitter accesses in bounded sample"
            } else {
                "Submitter access records"
            },
            access.2.to_string(),
        ));
        facts.push(fact(
            "Submission IP matched an access IP",
            access.3.map_or("not comparable", |matched| {
                if matched {
                    "yes"
                } else if access.4 {
                    "not found in bounded sample"
                } else {
                    "no"
                }
            }),
        ));
        if let Some(first) = access.1 {
            facts.push(fact(
                if access.4 {
                    "Earliest access in bounded sample"
                } else {
                    "First proxy access"
                },
                format_time(first),
            ));
        }
    }
    for (label, value) in [
        ("First challenge open", row.first_open_at),
        ("First attachment download", row.first_download_at),
        ("First container start", row.first_container_start_at),
        ("Last container operation", row.container_last_operation),
    ] {
        if let Some(value) = value {
            facts.push(fact(label, format_time(value)));
        }
    }
    if let Some(loaded) = row.container_was_loaded {
        facts.push(fact(
            "Container loaded at submit",
            if loaded { "yes" } else { "no" },
        ));
    }

    review.sources.push(EvidenceSourceReview {
        source_type: "submissionSnapshot".to_string(),
        title: "Immutable grading-time submission snapshot".to_string(),
        source_id: Some(format!("submission:{}", row.submission_id)),
        recorded_at: Some(row.submitted_at),
        immutable: true,
        summary: "The submission identity, result, time, user, network hash, and container-interaction snapshots were frozen when grading completed. The answer itself is redacted."
            .to_string(),
        facts,
    });
    mark_supporting(review);
    Ok(())
}

pub(super) async fn add_challenge_source(
    pool: &sqlx::PgPool,
    event: &EventEvidenceRow,
    ty: SuspicionType,
    review: &mut SuspicionEvidenceReview,
) -> AppResult<()> {
    let Some(challenge_id) = event.challenge_id else {
        return Ok(());
    };
    let snapshot_at = challenge_snapshot_at(event, ty);
    let summary: (i64, Option<f64>, bool) = sqlx::query_as(
        r#"WITH candidate AS MATERIALIZED (
               SELECT submission.submit_time_utc, submission.id, game.start_time_utc
                 FROM "FirstSolves" first_solve
                 JOIN "Submissions" submission ON submission.id = first_solve.submission_id
                  AND submission.participation_id = first_solve.participation_id
                  AND submission.challenge_id = first_solve.challenge_id
                 JOIN "Participations" participation ON participation.id = submission.participation_id
                  AND participation.game_id = submission.game_id
                 JOIN "Games" game ON game.id = submission.game_id
                WHERE submission.game_id = $1 AND submission.challenge_id = $2
                  AND submission.status = $3
                  AND submission.submit_time_utc >= game.start_time_utc
                  AND submission.submit_time_utc < game.end_time_utc
                  AND participation.competitive_admitted_at_utc IS NOT NULL
                  AND participation.competitive_admitted_at_utc < game.end_time_utc
                  AND ($4::TIMESTAMPTZ IS NULL OR submission.submit_time_utc <= $4)
                ORDER BY submission.submit_time_utc, submission.id
                LIMIT $5
           ), bounded AS MATERIALIZED (
               SELECT * FROM candidate ORDER BY submit_time_utc, id LIMIT $6
           )
           SELECT COUNT(*)::bigint,
                  PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY (EXTRACT(EPOCH FROM
                      (submit_time_utc - start_time_utc)) * 1000)::double precision)::double precision,
                  (SELECT COUNT(*) FROM candidate) > $6
             FROM bounded"#,
    )
    .bind(event.game_id)
    .bind(challenge_id)
    .bind(AnswerResult::Accepted as i16)
    .bind(snapshot_at)
    .bind(MAX_CHALLENGE_SOLVER_ROWS.saturating_add(1))
    .bind(MAX_CHALLENGE_SOLVER_ROWS)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let selected_solves = sqlx::query_as::<_, SolveSourceRow>(
        r#"WITH canonical AS NOT MATERIALIZED (
               SELECT submission.id AS submission_id, submission.participation_id,
                      team.name AS team_name, challenge.title AS challenge_title,
                      submission.submit_time_utc AS submitted_at, game.start_time_utc AS game_start,
                      submission.game_id, submission.challenge_id
                 FROM "FirstSolves" first_solve
                 JOIN "Submissions" submission ON submission.id = first_solve.submission_id
                  AND submission.participation_id = first_solve.participation_id
                  AND submission.challenge_id = first_solve.challenge_id
                 JOIN "Participations" participation ON participation.id = submission.participation_id
                  AND participation.game_id = submission.game_id
                 JOIN "Teams" team ON team.id = participation.team_id
                 JOIN "GameChallenges" challenge ON challenge.id = submission.challenge_id
                  AND challenge.game_id = submission.game_id
                 JOIN "Games" game ON game.id = submission.game_id
                WHERE submission.game_id = $1
                  AND submission.challenge_id = $2
                  AND submission.status = $4
                  AND submission.submit_time_utc >= game.start_time_utc AND submission.submit_time_utc < game.end_time_utc
                  AND participation.competitive_admitted_at_utc IS NOT NULL AND participation.competitive_admitted_at_utc < game.end_time_utc
                  AND ($5::TIMESTAMPTZ IS NULL OR submission.submit_time_utc <= $5)
            ), selected AS (
               SELECT head_solve.*, 0 AS selection_group
                 FROM (
                     SELECT * FROM canonical ORDER BY submitted_at, submission_id LIMIT $6
                 ) head_solve
                UNION ALL
               SELECT participant.*, 1 AS selection_group
                 FROM (
                     SELECT * FROM canonical WHERE participation_id = $7
                      ORDER BY submitted_at, submission_id LIMIT 1
                 ) participant
                ORDER BY selection_group, submitted_at, submission_id
            )
            SELECT selected.submission_id, selected.participation_id,
                   selected.team_name, selected.challenge_title, selected.submitted_at, selected.game_start,
                   (SELECT COUNT(*)::bigint FROM (
                        SELECT 1 FROM "Submissions" wrong
                         WHERE wrong.game_id = selected.game_id
                           AND wrong.participation_id = selected.participation_id
                           AND wrong.challenge_id = selected.challenge_id
                           AND wrong.status = $3
                           AND wrong.submit_time_utc >= selected.game_start
                           AND wrong.submit_time_utc < selected.submitted_at
                         ORDER BY wrong.submit_time_utc DESC, wrong.id DESC LIMIT $8
                    ) bounded_wrong) AS wrong_before
              FROM selected
             ORDER BY selection_group, submitted_at, submission_id"#,
    )
    .bind(event.game_id)
    .bind(challenge_id)
    .bind(AnswerResult::WrongAnswer as i16)
    .bind(AnswerResult::Accepted as i16)
    .bind(snapshot_at)
    .bind(CHALLENGE_LEADING_SOLVES)
    .bind(event.participation_id)
    .bind(MAX_WRONG_INTERVAL_ATTEMPTS.saturating_add(1))
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let leading_count = usize::try_from(summary.0.min(CHALLENGE_LEADING_SOLVES)).unwrap_or(0);
    let leading_solves = &selected_solves[..leading_count];
    let participant = selected_solves
        .iter()
        .find(|solve| solve.participation_id == event.participation_id);

    let wrong_pattern = matches!(
        ty,
        SuspicionType::HighWrongRate | SuspicionType::AutomatedPattern
    );
    if summary.0 == 0 && !wrong_pattern {
        return Ok(());
    }

    if summary.2 {
        review.limitations.push(format!(
            "Challenge reconstruction is capped at the earliest {MAX_CHALLENGE_SOLVER_ROWS} canonical solves; solver counts and the median below describe that bounded sample."
        ));
    }
    let mut facts = vec![
        fact(
            "Challenge",
            event
                .challenge_title
                .clone()
                .or_else(|| {
                    leading_solves
                        .first()
                        .map(|solve| solve.challenge_title.clone())
                })
                .or_else(|| participant.map(|solve| solve.challenge_title.clone()))
                .unwrap_or_else(|| format!("#{challenge_id}")),
        ),
        fact(
            if summary.2 {
                "Canonical solver count (lower bound)"
            } else {
                "Canonical solver count"
            },
            capped_count_text(summary.0, MAX_CHALLENGE_SOLVER_ROWS, summary.2),
        ),
    ];
    if let (Some(first), Some(median_ms)) = (leading_solves.first(), summary.1) {
        facts.push(fact("First solver team", &first.team_name));
        facts.push(fact(
            if summary.2 {
                "Bounded solver-sample median offset"
            } else {
                "Community median solve offset"
            },
            duration_text(median_ms.round() as i64),
        ));
    }
    if let Some(participant) = participant {
        let wrong_before_truncated = participant.wrong_before > MAX_WRONG_INTERVAL_ATTEMPTS;
        if wrong_before_truncated {
            review.limitations.push(format!(
                "Wrong attempts before the selected solve are capped at {MAX_WRONG_INTERVAL_ATTEMPTS}; the displayed count is a lower bound."
            ));
        }
        facts.extend([
            fact("Team solve", format_time(participant.submitted_at)),
            fact(
                "Team solve offset",
                duration_text(
                    (participant.submitted_at - participant.game_start).num_milliseconds(),
                ),
            ),
            fact(
                if wrong_before_truncated {
                    "Wrong attempts before solve (lower bound)"
                } else {
                    "Wrong attempts before solve"
                },
                capped_count_text(
                    participant.wrong_before.min(MAX_WRONG_INTERVAL_ATTEMPTS),
                    MAX_WRONG_INTERVAL_ATTEMPTS,
                    wrong_before_truncated,
                ),
            ),
            fact(
                "Canonical solve submission",
                format!("#{}", participant.submission_id),
            ),
        ]);
    }
    if leading_solves.len() == usize::try_from(CHALLENGE_LEADING_SOLVES).unwrap_or(2) {
        facts.push(fact(
            "First-to-second solve gap",
            duration_text(
                (leading_solves[1].submitted_at - leading_solves[0].submitted_at)
                    .num_milliseconds(),
            ),
        ));
    }

    if wrong_pattern {
        let wrong_summary: (i64, i64, i64, bool) = sqlx::query_as(
            r#"WITH sampled AS MATERIALIZED (
                   SELECT submission.submit_time_utc, submission.id
                     FROM "Submissions" submission
                     JOIN "Games" game ON game.id = submission.game_id
                    WHERE submission.game_id = $1
                      AND submission.participation_id = $2 AND submission.challenge_id = $3
                      AND submission.status = $4
                      AND submission.submit_time_utc >= game.start_time_utc AND submission.submit_time_utc < game.end_time_utc
                      AND submission.submit_time_utc <= $5
                    ORDER BY submission.submit_time_utc DESC, submission.id DESC
                    LIMIT $6
                ), bounded AS MATERIALIZED (
                   SELECT submit_time_utc, id FROM sampled
                    ORDER BY submit_time_utc DESC, id DESC LIMIT $7
                ), counts AS MATERIALIZED (
                   SELECT COUNT(*)::bigint AS total, COUNT(*) FILTER (WHERE
                              submit_time_utc >= $5 - INTERVAL '60 seconds')::bigint AS recent
                     FROM bounded
                ), ordered AS (
                   SELECT submit_time_utc, id,
                          LAG(submit_time_utc) OVER (ORDER BY submit_time_utc, id) AS previous_at
                     FROM bounded
                ), intervals AS (
                   SELECT submit_time_utc, id,
                          previous_at IS NOT NULL AND submit_time_utc >= previous_at
                              AND submit_time_utc - previous_at < INTERVAL '2 seconds'
                              AS is_fast
                     FROM ordered
                ), runs AS (
                   SELECT is_fast, SUM(CASE WHEN is_fast THEN 0 ELSE 1 END)
                              OVER (ORDER BY submit_time_utc, id) AS run_id
                     FROM intervals
                ), fast_runs AS (
                   SELECT run_id, COUNT(*)::bigint AS interval_count FROM runs WHERE is_fast
                    GROUP BY run_id
                )
                SELECT counts.total, counts.recent, COALESCE(MAX(interval_count), 0)::bigint,
                       (SELECT COUNT(*) FROM sampled) > $7
                  FROM counts LEFT JOIN fast_runs ON TRUE
              GROUP BY counts.total, counts.recent"#,
        )
        .bind(event.game_id)
        .bind(event.participation_id)
        .bind(challenge_id)
        .bind(AnswerResult::WrongAnswer as i16)
        .bind(event.created_at)
        .bind(MAX_WRONG_INTERVAL_ATTEMPTS.saturating_add(1))
        .bind(MAX_WRONG_INTERVAL_ATTEMPTS)
        .fetch_one(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if wrong_summary.3 {
            review.limitations.push(format!(
                "Wrong-attempt reconstruction reads only the latest {MAX_WRONG_INTERVAL_ATTEMPTS} wrong attempts through the event; total, recent-window, and interval facts below are bounded lower-bound/sample values."
            ));
        }
        facts.push(fact(
            if wrong_summary.3 {
                "Wrong attempts through event (lower bound)"
            } else {
                "Wrong attempts through event"
            },
            capped_count_text(
                wrong_summary.0,
                MAX_WRONG_INTERVAL_ATTEMPTS,
                wrong_summary.3,
            ),
        ));
        facts.push(fact(
            if wrong_summary.3 {
                "Wrong attempts in prior 60 seconds (bounded sample)"
            } else {
                "Wrong attempts in prior 60 seconds"
            },
            wrong_summary.1.to_string(),
        ));
        facts.push(fact(
            if wrong_summary.3 {
                "Consecutive sub-2-second intervals (bounded sample)"
            } else {
                "Consecutive sub-2-second intervals"
            },
            wrong_summary.2.to_string(),
        ));
    }

    review.sources.push(EvidenceSourceReview {
        source_type: "challengeOutcomeSnapshot".to_string(),
        title: "Canonical challenge outcome measurements".to_string(),
        source_id: Some(format!("challenge:{challenge_id}")),
        recorded_at: Some(event.created_at),
        immutable: true,
        summary: "Measurements are reconstructed from immutable first-solve and submission rows inside the configured competitive window."
            .to_string(),
        facts,
    });
    mark_supporting(review);
    Ok(())
}

pub(super) async fn add_burst_source(
    pool: &sqlx::PgPool,
    event: &EventEvidenceRow,
    review: &mut SuspicionEvidenceReview,
) -> AppResult<()> {
    // The latest three solves through the earliest qualifying completion recreate its trigger.
    let mut rows: Vec<(String, DateTime<Utc>)> = sqlx::query_as(
        r#"SELECT challenge.title, submission.submit_time_utc
             FROM "FirstSolves" first_solve
             JOIN "Submissions" submission
               ON submission.id = first_solve.submission_id
              AND submission.participation_id = first_solve.participation_id
              AND submission.challenge_id = first_solve.challenge_id
             JOIN "GameChallenges" challenge
               ON challenge.id = submission.challenge_id
              AND challenge.game_id = submission.game_id
             JOIN "Participations" participation
               ON participation.id = submission.participation_id
              AND participation.game_id = submission.game_id
             JOIN "Games" game ON game.id = submission.game_id
            WHERE submission.game_id = $1
              AND submission.participation_id = $2
              AND submission.status = $3
              AND submission.submit_time_utc >= game.start_time_utc AND submission.submit_time_utc < game.end_time_utc
              AND submission.submit_time_utc <= $4
              AND participation.competitive_admitted_at_utc IS NOT NULL AND participation.competitive_admitted_at_utc < game.end_time_utc
            ORDER BY submission.submit_time_utc DESC, submission.id DESC
            LIMIT $5"#,
    )
    .bind(event.game_id)
    .bind(event.participation_id)
    .bind(AnswerResult::Accepted as i16)
    .bind(event.created_at)
    .bind(BURST_SOLVE_ROWS)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if rows.len() != usize::try_from(BURST_SOLVE_ROWS).unwrap_or(3) {
        return Ok(());
    }
    rows.reverse();
    if rows[2].1 - rows[0].1 > chrono::Duration::seconds(60) {
        return Ok(());
    }
    review.sources.push(EvidenceSourceReview {
        source_type: "solveBurst".to_string(),
        title: "Fastest three-solve window".to_string(),
        source_id: Some("canonical-first-solves".to_string()),
        recorded_at: Some(rows[2].1),
        immutable: true,
        summary:
            "The detector requires at least three distinct canonical solves inside 60 seconds."
                .to_string(),
        facts: vec![
            fact(
                "Window",
                duration_text((rows[2].1 - rows[0].1).num_milliseconds()),
            ),
            fact(
                "Solves",
                rows.iter()
                    .map(|(title, time)| format!("{title} @ {}", format_time(*time)))
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
        ],
    });
    mark_supporting(review);
    Ok(())
}
