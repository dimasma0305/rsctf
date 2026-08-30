//! Bounded identity and solve-pair evidence projections.

use std::collections::{BTreeMap, BTreeSet};

use super::*;

pub(in crate::controllers::game) async fn add_identity_source(
    pool: &sqlx::PgPool,
    event: &EventEvidenceRow,
    ty: SuspicionType,
    review: &mut SuspicionEvidenceReview,
) -> AppResult<()> {
    let (hash, user_id, identity_kind, hash_column, same_team_only) = match ty {
        SuspicionType::SharedIp => (
            parse_hash_key(&event.evidence_key, "shared-ip:"),
            None,
            "Ip",
            "value_hash",
            true,
        ),
        SuspicionType::SharedFingerprint => (
            parse_hash_key(&event.evidence_key, "shared-fingerprint:"),
            None,
            "Fingerprint",
            "value_hash",
            false,
        ),
        SuspicionType::CrossTeamIp => (
            parse_hash_key(&event.evidence_key, "cross-team-ip:"),
            None,
            "Ip",
            "value_hash",
            false,
        ),
        SuspicionType::SubnetOverlap => (
            parse_hash_key(&event.evidence_key, "subnet-overlap:"),
            None,
            "Ip",
            "subnet_group_hash",
            false,
        ),
        SuspicionType::FingerprintChurn => (
            None,
            parse_uuid_user_key(&event.evidence_key, "fingerprint-churn:"),
            "Fingerprint",
            "value_hash",
            false,
        ),
        SuspicionType::IpChurn => (
            None,
            parse_uuid_user_key(&event.evidence_key, "ip-churn:"),
            "Ip",
            "value_hash",
            false,
        ),
        SuspicionType::SessionConcurrency => (
            None,
            parse_uuid_user_key(&event.evidence_key, "session-concurrency:"),
            "Ip",
            "value_hash",
            false,
        ),
        _ => return Ok(()),
    };
    if hash.is_none() && user_id.is_none() {
        return Ok(());
    }
    let sample_limit = i64::try_from(MAX_IDENTITY_SAMPLE_ROWS)
        .map_err(|_| AppError::internal("identity sample limit exceeds i64"))?
        .saturating_add(1);
    let mut rows = sqlx::query_as::<_, IdentitySourceRow>(
        r#"SELECT observation.user_id, account.user_name, team.name AS team_name,
                  observation.kind, observation.value_hint, observation.source,
                  observation.observed_at_utc AS observed_at
             FROM "IdentityObservations" observation
             JOIN "Games" game ON game.id = observation.game_id
             JOIN "UserParticipations" roster ON roster.user_id = observation.user_id
              AND roster.game_id = observation.game_id AND roster.team_id = observation.team_id
              AND roster.participation_id = observation.participation_id
             JOIN "Participations" participation ON participation.id = roster.participation_id
              AND participation.game_id = roster.game_id
             JOIN "Teams" team ON team.id = roster.team_id
             JOIN "AspNetUsers" account ON account.id = observation.user_id
            WHERE observation.game_id = $1
              AND observation.observed_at_utc >= game.start_time_utc
              AND observation.observed_at_utc < game.end_time_utc
              AND observation.observed_at_utc <= $8
              AND participation.competitive_admitted_at_utc IS NOT NULL
              AND observation.kind = $4
              AND ($2::BYTEA IS NULL
                   OR ($5 = 'value_hash' AND observation.value_hash = $2)
                   OR ($5 = 'subnet_group_hash' AND observation.subnet_group_hash = $2))
              AND ($3::UUID IS NULL OR observation.user_id = $3)
              AND (NOT $6 OR observation.team_id = $7)
            ORDER BY observation.observed_at_utc DESC, observation.id DESC
            LIMIT $9"#,
    )
    .bind(event.game_id)
    .bind(hash.as_deref())
    .bind(user_id)
    .bind(identity_kind)
    .bind(hash_column)
    .bind(same_team_only)
    .bind(event.team_id)
    .bind(event.created_at)
    .bind(sample_limit)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let truncated = rows.len() > MAX_IDENTITY_SAMPLE_ROWS;
    rows.truncate(MAX_IDENTITY_SAMPLE_ROWS);
    rows.reverse();
    let (Some(first), Some(last)) = (rows.first(), rows.last()) else {
        return Ok(());
    };
    if truncated {
        review.limitations.push(format!(
            "Identity reconstruction reads only the latest {MAX_IDENTITY_SAMPLE_ROWS} observations through the event; all counts and time bounds below describe that bounded sample."
        ));
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
    let count = if truncated {
        format!("at least {}", MAX_IDENTITY_SAMPLE_ROWS + 1)
    } else {
        rows.len().to_string()
    };
    review.sources.push(EvidenceSourceReview {
        source_type: "identityObservations".to_string(),
        title: "Privacy-preserving identity observations".to_string(),
        source_id: Some(event.evidence_key.clone()),
        recorded_at: Some(event.created_at),
        immutable: true,
        summary: "Only masked hints and deployment-keyed hashes are used for equality; raw IP addresses and fingerprints are not exposed.".to_string(),
        facts: vec![
            fact("Observations through event", count),
            fact("Observation kinds", kinds.into_iter().collect::<Vec<_>>().join(", ")),
            fact("Distinct identities in bounded sample", hints.len().to_string()),
            fact("Masked identity hints", hints.into_iter().take(12).collect::<Vec<_>>().join(", ")),
            fact("Teams", teams.into_iter().take(12).collect::<Vec<_>>().join(", ")),
            fact("Users", users.into_iter().take(12).collect::<Vec<_>>().join(", ")),
            fact("Admission sources", sources.into_iter().collect::<Vec<_>>().join(", ")),
            fact("Bounded sample first observed", format_time(first.observed_at)),
            fact("Bounded sample last observed", format_time(last.observed_at)),
        ],
    });
    mark_supporting(review);
    Ok(())
}

pub(in crate::controllers::game) async fn add_pair_source(
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
    if participants[0] == participants[1] {
        return Ok(());
    }
    let team_rows: Vec<(i32, String)> = sqlx::query_as(
        r#"SELECT participation.id, team.name
             FROM "Participations" participation
             JOIN "Teams" team ON team.id = participation.team_id
             JOIN "Games" game ON game.id = participation.game_id
            WHERE participation.game_id = $1 AND participation.id = ANY($2::INTEGER[])
              AND participation.competitive_admitted_at_utc IS NOT NULL
              AND participation.competitive_admitted_at_utc < game.end_time_utc
            ORDER BY participation.id LIMIT 2"#,
    )
    .bind(event.game_id)
    .bind(&participants[..])
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if team_rows.len() != 2 {
        return Ok(());
    }
    let team_names = team_rows.into_iter().collect::<BTreeMap<_, _>>();
    let input_limit = MAX_PAIR_SAMPLE_ROWS.saturating_add(1);
    let left: Vec<(i32, String, DateTime<Utc>)> = sqlx::query_as(
        r#"SELECT first_solve.challenge_id, challenge.title, submission.submit_time_utc
             FROM "FirstSolves" first_solve
             JOIN "Submissions" submission ON submission.id = first_solve.submission_id
              AND submission.participation_id = first_solve.participation_id
              AND submission.challenge_id = first_solve.challenge_id
             JOIN "GameChallenges" challenge ON challenge.id = first_solve.challenge_id
              AND challenge.game_id = submission.game_id
             JOIN "Games" game ON game.id = submission.game_id
            WHERE submission.game_id = $1 AND first_solve.participation_id = $2
              AND submission.status = $3
              AND submission.submit_time_utc >= game.start_time_utc
              AND submission.submit_time_utc < game.end_time_utc
              AND submission.submit_time_utc <= $4
            ORDER BY submission.submit_time_utc DESC, submission.id DESC LIMIT $5"#,
    )
    .bind(event.game_id)
    .bind(participants[0])
    .bind(AnswerResult::Accepted as i16)
    .bind(event.created_at)
    .bind(input_limit)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let truncated = left.len() > MAX_PAIR_SAMPLE_ROWS as usize;
    let challenge_ids = left.iter().map(|row| row.0).collect::<Vec<_>>();
    let right: Vec<(i32, DateTime<Utc>)> = sqlx::query_as(
        r#"SELECT first_solve.challenge_id, submission.submit_time_utc
             FROM "FirstSolves" first_solve
             JOIN "Submissions" submission ON submission.id = first_solve.submission_id
              AND submission.participation_id = first_solve.participation_id
              AND submission.challenge_id = first_solve.challenge_id
             JOIN "Games" game ON game.id = submission.game_id
            WHERE submission.game_id = $1 AND first_solve.participation_id = $2
              AND first_solve.challenge_id = ANY($3::INTEGER[])
              AND submission.status = $4
              AND submission.submit_time_utc >= game.start_time_utc
              AND submission.submit_time_utc < game.end_time_utc
              AND submission.submit_time_utc <= $5
            ORDER BY submission.submit_time_utc DESC, submission.id DESC LIMIT $6"#,
    )
    .bind(event.game_id)
    .bind(participants[1])
    .bind(&challenge_ids)
    .bind(AnswerResult::Accepted as i16)
    .bind(event.created_at)
    .bind(input_limit)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let right_times = right.into_iter().collect::<BTreeMap<_, _>>();
    let mut shared = left
        .into_iter()
        .filter_map(|(challenge_id, title, left_time)| {
            right_times
                .get(&challenge_id)
                .map(|right_time| (title, left_time, *right_time))
        })
        .collect::<Vec<_>>();
    shared.sort_by_key(|row| row.1.max(row.2));
    let observed_shared = shared.len();
    shared.truncate(MAX_PAIR_SAMPLE_ROWS as usize);
    if truncated {
        review.limitations.push(format!(
            "Pair reconstruction compares only the latest {MAX_PAIR_SAMPLE_ROWS} canonical solves from the first participation; overlap totals below are lower bounds."
        ));
    }
    let shared_count = if truncated {
        format!("at least {observed_shared} in bounded sample")
    } else {
        shared.len().to_string()
    };
    let sample = shared
        .iter()
        .map(|(title, left, right)| {
            format!(
                "{title}: {}",
                duration_text((*right - *left).num_milliseconds())
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
        summary: "This review lists a bounded immutable solve-overlap sample. The detector applies additional final-snapshot prevalence and consistency filters before emitting.".to_string(),
        facts: vec![
            fact("Teams", format!("{} ↔ {}",
                team_names.get(&participants[0]).cloned().unwrap_or_else(|| participants[0].to_string()),
                team_names.get(&participants[1]).cloned().unwrap_or_else(|| participants[1].to_string()))),
            fact("Shared canonical solves", shared_count),
            fact("Solve-gap sample", if sample.is_empty() { "none".to_string() } else { sample }),
        ],
    });
    mark_supporting(review);
    Ok(())
}
