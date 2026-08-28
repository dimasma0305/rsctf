//! Source projections for the lazy suspicion-evidence review endpoint.

use std::collections::BTreeSet;

use super::*;

const MAX_EVIDENCE_SOURCE_ROWS: usize = 2_048;

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
    let access: (i64, Option<DateTime<Utc>>, i64, Option<bool>) = sqlx::query_as(
        r#"SELECT COUNT(*)::bigint,
                  MIN(access.connected_at_utc),
                  COUNT(*) FILTER (WHERE access.accessing_user_id = submission.user_id)::bigint,
                  BOOL_OR(access.remote_ip_hash = submission.submit_remote_ip_hash)
             FROM "Submissions" submission
        LEFT JOIN "ContainerAccessEvents" access
               ON access.game_id = submission.game_id
              AND access.challenge_id = submission.challenge_id
              AND access.container_owner_participation_id = submission.participation_id
              AND access.container_id = submission.container_id
              AND access.is_monitor = FALSE
              AND access.connected_at_utc <= submission.submit_time_utc
            WHERE submission.id = $1
         GROUP BY submission.user_id, submission.submit_remote_ip_hash"#,
    )
    .bind(row.submission_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .unwrap_or((0, None, 0, None));
    let baseline: (i64, Option<bool>, Option<String>) = sqlx::query_as(
        r#"SELECT COUNT(DISTINCT observation.value_hash)::bigint,
                  BOOL_OR(observation.value_hash = submission.submit_remote_ip_hash),
                  STRING_AGG(DISTINCT observation.value_hint, ', ')
             FROM "Submissions" submission
        LEFT JOIN "IdentityObservations" observation
               ON observation.game_id = submission.game_id
              AND observation.participation_id = submission.participation_id
              AND observation.kind = 'Ip'
              AND observation.observed_at_utc <= submission.submit_time_utc
            WHERE submission.id = $1
         GROUP BY submission.submit_remote_ip_hash"#,
    )
    .bind(row.submission_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .unwrap_or((0, None, None));

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
            "Known game IP identities before submit",
            baseline.0.to_string(),
        ),
        fact(
            "Submission IP matched prior game identity",
            baseline.1.map_or(
                "not comparable",
                |matched| if matched { "yes" } else { "no" },
            ),
        ),
    ];
    if let Some(hints) = baseline.2.filter(|hints| !hints.is_empty()) {
        facts.push(fact("Prior masked IP hints", hints));
    }
    if let Some(container_id) = row.container_id {
        facts.push(fact("Container generation", container_id.to_string()));
        facts.push(fact("Matching access records", access.0.to_string()));
        facts.push(fact("Submitter access records", access.2.to_string()));
        facts.push(fact(
            "Submission IP matched an access IP",
            access.3.map_or(
                "not comparable",
                |matched| if matched { "yes" } else { "no" },
            ),
        ));
    }
    for (label, value) in [
        ("First challenge open", row.first_open_at),
        ("First attachment download", row.first_download_at),
        ("First container start", row.first_container_start_at),
        ("First proxy access", access.1),
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
    let solves = sqlx::query_as::<_, SolveSourceRow>(
        r#"SELECT submission.id AS submission_id,
                  submission.participation_id,
                  team.name AS team_name,
                  challenge.title AS challenge_title,
                  submission.submit_time_utc AS submitted_at,
                  game.start_time_utc AS game_start,
                  COUNT(*) OVER ()::bigint AS solver_count,
                  (SELECT COUNT(*)::bigint
                     FROM "Submissions" wrong
                    WHERE wrong.game_id = submission.game_id
                      AND wrong.participation_id = submission.participation_id
                      AND wrong.challenge_id = submission.challenge_id
                      AND wrong.status = $3
                      AND wrong.submit_time_utc < submission.submit_time_utc) AS wrong_before
             FROM "FirstSolves" first_solve
             JOIN "Submissions" submission
               ON submission.id = first_solve.submission_id
              AND submission.participation_id = first_solve.participation_id
              AND submission.challenge_id = first_solve.challenge_id
             JOIN "Participations" participation
               ON participation.id = submission.participation_id
              AND participation.game_id = submission.game_id
             JOIN "Teams" team ON team.id = participation.team_id
             JOIN "GameChallenges" challenge
               ON challenge.id = submission.challenge_id
              AND challenge.game_id = submission.game_id
             JOIN "Games" game ON game.id = submission.game_id
            WHERE submission.game_id = $1
              AND submission.challenge_id = $2
              AND submission.status = $4
              AND submission.submit_time_utc >= game.start_time_utc
              AND submission.submit_time_utc < game.end_time_utc
            ORDER BY submission.submit_time_utc, submission.id
            LIMIT $5"#,
    )
    .bind(event.game_id)
    .bind(challenge_id)
    .bind(AnswerResult::WrongAnswer as i16)
    .bind(AnswerResult::Accepted as i16)
    .bind(MAX_EVIDENCE_SOURCE_ROWS as i64 + 1)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let mut solves = solves;
    let solver_count = solves
        .first()
        .map(|solve| solve.solver_count.max(0) as usize)
        .unwrap_or(0);
    let solver_history_truncated = solves.len() > MAX_EVIDENCE_SOURCE_ROWS;
    solves.truncate(MAX_EVIDENCE_SOURCE_ROWS);
    if solver_history_truncated {
        review.limitations.push(format!(
            "Canonical solver history is capped at the first {MAX_EVIDENCE_SOURCE_ROWS} rows; population counts remain exact, but later team detail is omitted."
        ));
    }
    let wrong_pattern = matches!(
        ty,
        SuspicionType::HighWrongRate | SuspicionType::AutomatedPattern
    );
    if solves.is_empty() && !wrong_pattern {
        return Ok(());
    }

    let participant = solves
        .iter()
        .find(|solve| solve.participation_id == event.participation_id);
    let mut offsets = solves
        .iter()
        .map(|solve| (solve.submitted_at - solve.game_start).num_milliseconds() as f64)
        .collect::<Vec<_>>();
    let mut facts = vec![
        fact(
            "Challenge",
            event
                .challenge_title
                .clone()
                .or_else(|| solves.first().map(|solve| solve.challenge_title.clone()))
                .unwrap_or_else(|| format!("#{challenge_id}")),
        ),
        fact("Canonical solver count", solver_count.to_string()),
    ];
    if !offsets.is_empty() && !solver_history_truncated {
        offsets.sort_by(|a, b| a.total_cmp(b));
        let median_ms = if offsets.len().is_multiple_of(2) {
            (offsets[offsets.len() / 2 - 1] + offsets[offsets.len() / 2]) / 2.0
        } else {
            offsets[offsets.len() / 2]
        };
        facts.push(fact("First solver team", &solves[0].team_name));
        facts.push(fact(
            "Community median solve offset",
            duration_text(median_ms.round() as i64),
        ));
    }
    if let Some(participant) = participant {
        facts.extend([
            fact("Team solve", format_time(participant.submitted_at)),
            fact(
                "Team solve offset",
                duration_text(
                    (participant.submitted_at - participant.game_start).num_milliseconds(),
                ),
            ),
            fact(
                "Wrong attempts before solve",
                participant.wrong_before.to_string(),
            ),
            fact(
                "Canonical solve submission",
                format!("#{}", participant.submission_id),
            ),
        ]);
    }
    if solves.len() >= 2 {
        facts.push(fact(
            "First-to-second solve gap",
            duration_text((solves[1].submitted_at - solves[0].submitted_at).num_milliseconds()),
        ));
    }

    if wrong_pattern {
        let (wrong_count, rolling): (i64, i64) = sqlx::query_as(
            r#"SELECT COUNT(*)::bigint,
                      COUNT(*) FILTER (
                          WHERE submit_time_utc >= $5 - INTERVAL '60 seconds'
                      )::bigint
                 FROM "Submissions"
                WHERE game_id = $1
                  AND participation_id = $2
                  AND challenge_id = $3
                  AND status = $4
                  AND submit_time_utc <= $5"#,
        )
        .bind(event.game_id)
        .bind(event.participation_id)
        .bind(challenge_id)
        .bind(AnswerResult::WrongAnswer as i16)
        .bind(event.created_at)
        .fetch_one(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        let wrong_times: Vec<DateTime<Utc>> = sqlx::query_scalar(
            r#"SELECT sampled.submit_time_utc
                 FROM (
                       SELECT submit_time_utc, id
                         FROM "Submissions"
                        WHERE game_id = $1
                          AND participation_id = $2
                          AND challenge_id = $3
                          AND status = $4
                          AND submit_time_utc <= $5
                        ORDER BY submit_time_utc DESC, id DESC
                        LIMIT $6
                 ) sampled
                ORDER BY sampled.submit_time_utc, sampled.id"#,
        )
        .bind(event.game_id)
        .bind(event.participation_id)
        .bind(challenge_id)
        .bind(AnswerResult::WrongAnswer as i16)
        .bind(event.created_at)
        .bind(MAX_EVIDENCE_SOURCE_ROWS as i64)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if wrong_count > MAX_EVIDENCE_SOURCE_ROWS as i64 {
            review.limitations.push(format!(
                "Wrong-attempt timing detail is capped at the most recent {MAX_EVIDENCE_SOURCE_ROWS} rows; exact total and prior-60-second counts come from bounded aggregate output."
            ));
        }
        let mut fastest_run = 0usize;
        let mut run = 0usize;
        for pair in wrong_times.windows(2) {
            let delta = (pair[1] - pair[0]).num_milliseconds();
            if (0..2_000).contains(&delta) {
                run += 1;
                fastest_run = fastest_run.max(run);
            } else {
                run = 0;
            }
        }
        facts.push(fact(
            "Wrong attempts through event",
            wrong_count.to_string(),
        ));
        facts.push(fact(
            "Wrong attempts in prior 60 seconds",
            rolling.to_string(),
        ));
        facts.push(fact(
            "Consecutive sub-2-second intervals",
            fastest_run.to_string(),
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
    let row = sqlx::query_as::<_, BurstSourceRow>(
        r#"WITH solves AS (
               SELECT challenge.title AS first_title,
                      submission.submit_time_utc AS first_at,
                      submission.id,
                      LEAD(challenge.title, 1) OVER solve_order AS second_title,
                      LEAD(submission.submit_time_utc, 1) OVER solve_order AS second_at,
                      LEAD(challenge.title, 2) OVER solve_order AS third_title,
                      LEAD(submission.submit_time_utc, 2) OVER solve_order AS third_at
                 FROM "FirstSolves" first_solve
                 JOIN "Submissions" submission
                   ON submission.id = first_solve.submission_id
                  AND submission.participation_id = first_solve.participation_id
                  AND submission.challenge_id = first_solve.challenge_id
                 JOIN "GameChallenges" challenge
                   ON challenge.id = submission.challenge_id
                  AND challenge.game_id = submission.game_id
                 JOIN "Games" game ON game.id = submission.game_id
                WHERE submission.game_id = $1
                  AND submission.participation_id = $2
                  AND submission.status = $3
                  AND submission.submit_time_utc >= game.start_time_utc
                  AND submission.submit_time_utc < game.end_time_utc
               WINDOW solve_order AS (
                   ORDER BY submission.submit_time_utc, submission.id
               )
         )
         SELECT first_title, first_at, second_title, second_at, third_title, third_at
           FROM solves
          WHERE third_at IS NOT NULL
          ORDER BY third_at - first_at, first_at
          LIMIT 1"#,
    )
    .bind(event.game_id)
    .bind(event.participation_id)
    .bind(AnswerResult::Accepted as i16)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(row) = row else {
        return Ok(());
    };
    review.sources.push(EvidenceSourceReview {
        source_type: "solveBurst".to_string(),
        title: "Fastest three-solve window".to_string(),
        source_id: Some("canonical-first-solves".to_string()),
        recorded_at: Some(row.third_at),
        immutable: true,
        summary:
            "The detector requires at least three distinct canonical solves inside 60 seconds."
                .to_string(),
        facts: vec![
            fact(
                "Window",
                duration_text((row.third_at - row.first_at).num_milliseconds()),
            ),
            fact(
                "Solves",
                [
                    (row.first_title, row.first_at),
                    (row.second_title, row.second_at),
                    (row.third_title, row.third_at),
                ]
                .into_iter()
                .map(|(title, time)| format!("{title} @ {}", format_time(time)))
                .collect::<Vec<_>>()
                .join("; "),
            ),
        ],
    });
    mark_supporting(review);
    Ok(())
}

pub(super) async fn add_identity_source(
    pool: &sqlx::PgPool,
    event: &EventEvidenceRow,
    ty: SuspicionType,
    review: &mut SuspicionEvidenceReview,
) -> AppResult<()> {
    let (hash, user_id, hash_column, same_team_only) = match ty {
        SuspicionType::SharedIp => (
            parse_hash_key(&event.evidence_key, "shared-ip:"),
            None,
            "value_hash",
            true,
        ),
        SuspicionType::SharedFingerprint => (
            parse_hash_key(&event.evidence_key, "shared-fingerprint:"),
            None,
            "value_hash",
            false,
        ),
        SuspicionType::CrossTeamIp => (
            parse_hash_key(&event.evidence_key, "cross-team-ip:"),
            None,
            "value_hash",
            false,
        ),
        SuspicionType::SubnetOverlap => (
            parse_hash_key(&event.evidence_key, "subnet-overlap:"),
            None,
            "subnet_group_hash",
            false,
        ),
        SuspicionType::FingerprintChurn => (
            None,
            parse_uuid_user_key(&event.evidence_key, "fingerprint-churn:"),
            "value_hash",
            false,
        ),
        SuspicionType::IpChurn => (
            None,
            parse_uuid_user_key(&event.evidence_key, "ip-churn:"),
            "value_hash",
            false,
        ),
        SuspicionType::SessionConcurrency => (
            None,
            parse_uuid_user_key(&event.evidence_key, "session-concurrency:"),
            "value_hash",
            false,
        ),
        _ => return Ok(()),
    };
    if hash.is_none() && user_id.is_none() {
        return Ok(());
    }
    let rows = sqlx::query_as::<_, IdentitySourceRow>(
        r#"SELECT observation.user_id,
                  account.user_name,
                  team.name AS team_name,
                  observation.kind,
                  observation.value_hint,
                  observation.source,
                  observation.observed_at_utc AS observed_at
             FROM "IdentityObservations" observation
             JOIN "Games" game ON game.id = observation.game_id
             JOIN "UserParticipations" roster
               ON roster.user_id = observation.user_id
              AND roster.game_id = observation.game_id
              AND roster.team_id = observation.team_id
              AND roster.participation_id = observation.participation_id
             JOIN "Participations" participation
               ON participation.id = roster.participation_id
              AND participation.game_id = roster.game_id
             JOIN "Teams" team ON team.id = roster.team_id
             JOIN "AspNetUsers" account ON account.id = observation.user_id
            WHERE observation.game_id = $1
              AND observation.observed_at_utc >= game.start_time_utc
              AND observation.observed_at_utc < game.end_time_utc
              AND participation.competitive_admitted_at_utc IS NOT NULL
              AND ($2::BYTEA IS NULL
                   OR ($4 = 'value_hash' AND observation.value_hash = $2)
                   OR ($4 = 'subnet_group_hash' AND observation.subnet_group_hash = $2))
              AND ($3::UUID IS NULL OR observation.user_id = $3)
              AND (NOT $5 OR observation.team_id = $6)
            ORDER BY observation.observed_at_utc, observation.id
            LIMIT 200"#,
    )
    .bind(event.game_id)
    .bind(hash)
    .bind(user_id)
    .bind(hash_column)
    .bind(same_team_only)
    .bind(event.team_id)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if rows.is_empty() {
        return Ok(());
    }

    let teams = rows
        .iter()
        .map(|row| row.team_name.clone())
        .collect::<BTreeSet<_>>();
    let users = rows
        .iter()
        .map(|row| format!("{} ({})", row.user_name, row.user_id))
        .collect::<BTreeSet<_>>();
    let hints = rows
        .iter()
        .map(|row| row.value_hint.clone())
        .collect::<BTreeSet<_>>();
    let sources = rows
        .iter()
        .map(|row| row.source.clone())
        .collect::<BTreeSet<_>>();
    let kinds = rows
        .iter()
        .map(|row| row.kind.clone())
        .collect::<BTreeSet<_>>();
    let first = rows
        .first()
        .map(|row| row.observed_at)
        .expect("non-empty rows");
    let last = rows
        .last()
        .map(|row| row.observed_at)
        .expect("non-empty rows");
    review.sources.push(EvidenceSourceReview {
        source_type: "identityObservations".to_string(),
        title: "Privacy-preserving identity observations".to_string(),
        source_id: Some(event.evidence_key.clone()),
        recorded_at: Some(event.created_at),
        immutable: true,
        summary: "Only masked hints and deployment-keyed hashes are used for equality; raw IP addresses and fingerprints are not exposed."
            .to_string(),
        facts: vec![
            fact("Observation kinds", kinds.into_iter().collect::<Vec<_>>().join(", ")),
            fact("Distinct identities", hints.len().to_string()),
            fact("Masked identity hints", hints.into_iter().take(12).collect::<Vec<_>>().join(", ")),
            fact("Teams", teams.into_iter().take(12).collect::<Vec<_>>().join(", ")),
            fact("Users", users.into_iter().take(12).collect::<Vec<_>>().join(", ")),
            fact("Admission sources", sources.into_iter().collect::<Vec<_>>().join(", ")),
            fact("First observed", format_time(first)),
            fact("Last observed", format_time(last)),
        ],
    });
    mark_supporting(review);
    Ok(())
}

pub(super) async fn add_pair_source(
    pool: &sqlx::PgPool,
    event: &EventEvidenceRow,
    ty: SuspicionType,
    review: &mut SuspicionEvidenceReview,
) -> AppResult<()> {
    let participants = if ty == SuspicionType::SequenceSimilarity {
        let Some(pair) = event.evidence_key.strip_prefix("pair:") else {
            return Ok(());
        };
        let mut parts = pair
            .split(':')
            .filter_map(|value| value.parse::<i32>().ok());
        let (Some(left), Some(right), None) = (parts.next(), parts.next(), parts.next()) else {
            return Ok(());
        };
        [left, right]
    } else {
        let Some(source) = parse_i32_key(&event.evidence_key, "source:") else {
            return Ok(());
        };
        [source, event.participation_id]
    };
    let rows = sqlx::query_as::<_, PairSourceRow>(
        r#"SELECT left_team.name AS left_team_name,
                  right_team.name AS right_team_name,
                  challenge.title AS challenge_title,
                  left_submission.submit_time_utc AS left_at,
                  right_submission.submit_time_utc AS right_at,
                  COUNT(*) OVER ()::bigint AS shared_count
             FROM "FirstSolves" left_solve
             JOIN "Submissions" left_submission
               ON left_submission.id = left_solve.submission_id
              AND left_submission.participation_id = left_solve.participation_id
              AND left_submission.challenge_id = left_solve.challenge_id
             JOIN "FirstSolves" right_solve
               ON right_solve.challenge_id = left_solve.challenge_id
              AND right_solve.participation_id = $3
             JOIN "Submissions" right_submission
               ON right_submission.id = right_solve.submission_id
              AND right_submission.participation_id = right_solve.participation_id
              AND right_submission.challenge_id = right_solve.challenge_id
              AND right_submission.game_id = left_submission.game_id
             JOIN "Participations" left_participation
               ON left_participation.id = left_submission.participation_id
              AND left_participation.game_id = left_submission.game_id
             JOIN "Teams" left_team ON left_team.id = left_participation.team_id
             JOIN "Participations" right_participation
               ON right_participation.id = right_submission.participation_id
              AND right_participation.game_id = right_submission.game_id
             JOIN "Teams" right_team ON right_team.id = right_participation.team_id
             JOIN "GameChallenges" challenge
               ON challenge.id = left_submission.challenge_id
              AND challenge.game_id = left_submission.game_id
             JOIN "Games" game ON game.id = left_submission.game_id
            WHERE left_submission.game_id = $1
              AND left_submission.participation_id = $2
              AND left_submission.status = $4
              AND right_submission.status = $4
              AND left_submission.submit_time_utc >= game.start_time_utc
              AND left_submission.submit_time_utc < game.end_time_utc
              AND right_submission.submit_time_utc >= game.start_time_utc
              AND right_submission.submit_time_utc < game.end_time_utc
            ORDER BY GREATEST(
                         left_submission.submit_time_utc,
                         right_submission.submit_time_utc
                     ),
                     left_submission.id,
                     right_submission.id
            LIMIT 12"#,
    )
    .bind(event.game_id)
    .bind(participants[0])
    .bind(participants[1])
    .bind(AnswerResult::Accepted as i16)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(first) = rows.first() else {
        return Ok(());
    };
    let shared_count = first.shared_count;
    let left_team_name = first.left_team_name.clone();
    let right_team_name = first.right_team_name.clone();
    let sample = rows
        .iter()
        .map(|row| {
            format!(
                "{title}: {}",
                duration_text((row.right_at - row.left_at).num_milliseconds()),
                title = row.challenge_title,
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    review.sources.push(EvidenceSourceReview {
        source_type: "canonicalSolvePair".to_string(),
        title: "Cross-team canonical solve comparison".to_string(),
        source_id: Some(event.evidence_key.clone()),
        recorded_at: Some(event.created_at),
        immutable: true,
        summary: "This review lists the immutable solve overlap. The detector applies additional final-snapshot prevalence and consistency filters before emitting."
            .to_string(),
        facts: vec![
            fact(
                "Teams",
                format!(
                    "{} ↔ {}",
                    left_team_name,
                    right_team_name,
                ),
            ),
            fact("Shared canonical solves", shared_count.to_string()),
            fact("Solve-gap sample", if sample.is_empty() { "none".to_string() } else { sample }),
        ],
    });
    mark_supporting(review);
    Ok(())
}
