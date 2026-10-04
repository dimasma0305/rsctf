use sha2::{Digest, Sha256};

use super::*;

const ATTEMPT_REUSE_ERROR: &str =
    "This submission attempt ID was already used with different content";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SubmissionReplay {
    pub(super) submission_id: i32,
    pub(super) status: i16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AttemptReservation {
    Fresh,
    Replay(SubmissionReplay),
}

/// Bind an opaque retry key to the semantic request without retaining another
/// copy of the submitted flag or one-use receipt. The user identity is part of
/// the fingerprint so a teammate cannot use a guessed attempt UUID to recover
/// another user's submission ID.
pub(super) fn submission_request_fingerprint(
    user_id: Uuid,
    answer: &str,
    proof: Option<&str>,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"rsctf:normal-submit-attempt:v1\0");
    digest.update(user_id.as_bytes());
    digest.update((answer.len() as u64).to_be_bytes());
    digest.update(answer.as_bytes());
    match proof {
        Some(proof) => {
            digest.update([1]);
            digest.update((proof.len() as u64).to_be_bytes());
            digest.update(proof.as_bytes());
        }
        None => digest.update([0]),
    }
    digest.finalize().into()
}

fn fingerprints_match(stored: &[u8], expected: &[u8; 32]) -> bool {
    if stored.len() != expected.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (left, right) in stored.iter().zip(expected) {
        difference |= left ^ right;
    }
    difference == 0
}

fn completed_replay(
    fingerprint: Vec<u8>,
    submission_id: Option<i32>,
    status: Option<i16>,
    expected: &[u8; 32],
) -> AppResult<SubmissionReplay> {
    if !fingerprints_match(&fingerprint, expected) {
        return Err(AppError::conflict(ATTEMPT_REUSE_ERROR));
    }
    match (submission_id, status) {
        (Some(submission_id), Some(status)) => Ok(SubmissionReplay {
            submission_id,
            status,
        }),
        _ => Err(AppError::internal(
            "committed submission attempt is missing its result",
        )),
    }
}

/// Fast recovery path for an already committed request. This deliberately runs
/// before mutable challenge policy checks: recovering one's own durable result
/// after a lost response must not grade again merely because the event ended or
/// an organizer disabled the challenge in the meantime.
pub(super) async fn find_completed_attempt(
    pool: &sqlx::PgPool,
    participation_id: i32,
    challenge_id: i32,
    attempt_id: Uuid,
    fingerprint: &[u8; 32],
) -> AppResult<Option<SubmissionReplay>> {
    let row: Option<(Vec<u8>, Option<i32>, Option<i16>)> = sqlx::query_as(
        r#"SELECT attempt.request_fingerprint,
                  attempt.submission_id,
                  submission.status
             FROM "SubmissionAttempts" attempt
        LEFT JOIN "Submissions" submission
               ON submission.id = attempt.submission_id
              AND submission.participation_id = attempt.participation_id
              AND submission.challenge_id = attempt.challenge_id
            WHERE attempt.participation_id = $1
              AND attempt.challenge_id = $2
              AND attempt.attempt_id = $3"#,
    )
    .bind(participation_id)
    .bind(challenge_id)
    .bind(attempt_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    row.map(|(stored, submission_id, status)| {
        completed_replay(stored, submission_id, status, fingerprint)
    })
    .transpose()
}

/// Atomically reserve the client attempt inside the grading transaction. An
/// exact concurrent duplicate waits on PostgreSQL's unique index, observes the
/// first committed submission, and returns it without entering any side effect.
pub(super) async fn reserve_attempt(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    participation_id: i32,
    challenge_id: i32,
    attempt_id: Uuid,
    fingerprint: &[u8; 32],
) -> AppResult<AttemptReservation> {
    let inserted = sqlx::query_scalar::<_, i32>(
        r#"INSERT INTO "SubmissionAttempts"
                 (participation_id, challenge_id, attempt_id, request_fingerprint)
           VALUES ($1, $2, $3, $4)
           ON CONFLICT (participation_id, challenge_id, attempt_id) DO NOTHING
           RETURNING participation_id"#,
    )
    .bind(participation_id)
    .bind(challenge_id)
    .bind(attempt_id)
    .bind(fingerprint.as_slice())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if inserted.is_some() {
        return Ok(AttemptReservation::Fresh);
    }

    let row: (Vec<u8>, Option<i32>, Option<i16>) = sqlx::query_as(
        r#"SELECT attempt.request_fingerprint,
                  attempt.submission_id,
                  submission.status
             FROM "SubmissionAttempts" attempt
        LEFT JOIN "Submissions" submission
               ON submission.id = attempt.submission_id
              AND submission.participation_id = attempt.participation_id
              AND submission.challenge_id = attempt.challenge_id
            WHERE attempt.participation_id = $1
              AND attempt.challenge_id = $2
              AND attempt.attempt_id = $3
            FOR UPDATE OF attempt"#,
    )
    .bind(participation_id)
    .bind(challenge_id)
    .bind(attempt_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    completed_replay(row.0, row.1, row.2, fingerprint).map(AttemptReservation::Replay)
}

/// Complete the reservation before commit. A missing/previously completed row
/// is an integrity failure rather than permission to publish duplicate effects.
pub(super) async fn complete_attempt(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    participation_id: i32,
    challenge_id: i32,
    attempt_id: Uuid,
    fingerprint: &[u8; 32],
    submission_id: i32,
) -> AppResult<()> {
    let updated = sqlx::query(
        r#"UPDATE "SubmissionAttempts"
              SET submission_id = $5
            WHERE participation_id = $1
              AND challenge_id = $2
              AND attempt_id = $3
              AND request_fingerprint = $4
              AND submission_id IS NULL"#,
    )
    .bind(participation_id)
    .bind(challenge_id)
    .bind(attempt_id)
    .bind(fingerprint.as_slice())
    .bind(submission_id)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if updated.rows_affected() != 1 {
        return Err(AppError::internal(
            "submission attempt reservation could not be completed",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_and_binds_user_answer_and_proof_boundaries() {
        let user = Uuid::from_u128(7);
        let exact = submission_request_fingerprint(user, "flag{ok}", Some("receipt"));
        assert_eq!(
            exact,
            submission_request_fingerprint(user, "flag{ok}", Some("receipt"))
        );
        assert_ne!(
            exact,
            submission_request_fingerprint(Uuid::from_u128(8), "flag{ok}", Some("receipt"))
        );
        assert_ne!(
            exact,
            submission_request_fingerprint(user, "flag{other}", Some("receipt"))
        );
        assert_ne!(
            exact,
            submission_request_fingerprint(user, "flag{ok}", Some("other"))
        );
        assert_ne!(
            submission_request_fingerprint(user, "ab", Some("c")),
            submission_request_fingerprint(user, "a", Some("bc"))
        );
        assert_ne!(
            submission_request_fingerprint(user, "flag{ok}", None),
            submission_request_fingerprint(user, "flag{ok}", Some(""))
        );
    }

    #[test]
    fn replay_requires_both_the_original_submission_and_result() {
        let fingerprint = submission_request_fingerprint(Uuid::from_u128(7), "flag", None);
        assert_eq!(
            completed_replay(fingerprint.to_vec(), Some(41), Some(2), &fingerprint).unwrap(),
            SubmissionReplay {
                submission_id: 41,
                status: 2
            }
        );
        assert!(completed_replay(fingerprint.to_vec(), None, None, &fingerprint).is_err());

        let other = submission_request_fingerprint(Uuid::from_u128(7), "other", None);
        let mismatch = completed_replay(fingerprint.to_vec(), Some(41), Some(2), &other)
            .expect_err("attempt reuse with a different payload must fail");
        assert_eq!(mismatch.status(), axum::http::StatusCode::CONFLICT);
    }
}

#[cfg(test)]
#[path = "submit_idempotency_tests.rs"]
mod integration_tests;
