//! Per-replica admission for monitor spreadsheet exports.
//!
//! A small operation semaphore bounds PostgreSQL snapshots and blocking XLSX
//! tasks. A second weighted semaphore limits the combined row-shaped working
//! set, so two small exports can proceed without admitting two maximum-sized
//! submission exports on the same replica.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::utils::enums::AnswerResult;
use crate::utils::error::AppError;

const MAX_CONCURRENT_EXPORTS: usize = 2;
const MAX_WEIGHT_UNITS: usize = 64;
const ROWS_PER_WEIGHT_UNIT: usize = 1_000;
const BYTES_PER_WEIGHT_UNIT: usize = 1024 * 1024;
pub(crate) const MAX_SUBMISSION_EXPORT_ROWS: usize = 50_000;
const MAX_SUBMISSION_EXPORT_SNAPSHOT_BYTES: usize = 48 * 1024 * 1024;
const SUBMISSION_EXPORT_PAGE_ROWS: usize = 1_000;

#[derive(Clone)]
pub(crate) struct MonitorExportAdmission {
    slots: Arc<Semaphore>,
    weight: Arc<Semaphore>,
}

pub(crate) struct MonitorExportPermit {
    _slot: OwnedSemaphorePermit,
    weight: Arc<Semaphore>,
    _weighted: Option<OwnedSemaphorePermit>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MonitorExportAdmissionError {
    Busy,
    WeightedCapacity,
}

impl MonitorExportAdmission {
    pub(crate) fn new() -> Self {
        Self::with_limits(MAX_CONCURRENT_EXPORTS, MAX_WEIGHT_UNITS)
    }

    fn with_limits(slots: usize, weight_units: usize) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(slots)),
            weight: Arc::new(Semaphore::new(weight_units)),
        }
    }

    /// Reserve one export task before opening a PostgreSQL snapshot or doing
    /// any blocking workbook work. Admission is deliberately immediate: HTTP
    /// callers receive a retryable overload response instead of forming a queue.
    pub(crate) fn try_begin(&self) -> Result<MonitorExportPermit, MonitorExportAdmissionError> {
        let slot = Arc::clone(&self.slots)
            .try_acquire_owned()
            .map_err(|_| MonitorExportAdmissionError::Busy)?;
        Ok(MonitorExportPermit {
            _slot: slot,
            weight: Arc::clone(&self.weight),
            _weighted: None,
        })
    }
}

impl Default for MonitorExportAdmission {
    fn default() -> Self {
        Self::new()
    }
}

impl MonitorExportPermit {
    /// Charge the larger of one unit per thousand rows or one unit per MiB,
    /// rounded up. The permit stays owned through XLSX generation.
    pub(crate) fn try_reserve_work(
        &mut self,
        rows: usize,
        bytes: usize,
    ) -> Result<(), MonitorExportAdmissionError> {
        debug_assert!(self._weighted.is_none(), "export weight reserved twice");
        let row_units = rows.max(1).div_ceil(ROWS_PER_WEIGHT_UNIT);
        let byte_units = bytes.div_ceil(BYTES_PER_WEIGHT_UNIT);
        let units = row_units.max(byte_units);
        let units =
            u32::try_from(units).map_err(|_| MonitorExportAdmissionError::WeightedCapacity)?;
        let permit = Arc::clone(&self.weight)
            .try_acquire_many_owned(units)
            .map_err(|_| MonitorExportAdmissionError::WeightedCapacity)?;
        self._weighted = Some(permit);
        Ok(())
    }
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct SubmissionExportRow {
    pub(crate) id: i32,
    pub(crate) submit_time_utc: DateTime<Utc>,
    pub(crate) team_name: Option<String>,
    pub(crate) user_name: Option<String>,
    pub(crate) challenge_title: Option<String>,
    pub(crate) answer: String,
    pub(crate) status: i16,
}

#[derive(Debug)]
pub(crate) enum SubmissionSnapshotError {
    Application(AppError),
    Overloaded(MonitorExportAdmissionError),
}

impl From<AppError> for SubmissionSnapshotError {
    fn from(error: AppError) -> Self {
        Self::Application(error)
    }
}

/// Read one transactionally stable, explicitly bounded submission snapshot.
/// Keyset pages avoid retaining both complete ORM entities and a second
/// projected vector, while the repeatable-read transaction keeps the count and
/// every page on the same PostgreSQL snapshot.
pub(crate) async fn load_submission_export_snapshot(
    pool: &sqlx::PgPool,
    game_id: i32,
    permit: &mut MonitorExportPermit,
) -> Result<Vec<SubmissionExportRow>, SubmissionSnapshotError> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query("SET LOCAL statement_timeout = '20s'")
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    let (bounded_count, bounded_bytes, valid_statuses): (i64, i64, bool) = sqlx::query_as(
        r#"WITH bounded AS (
               SELECT submission.id,
                      submission.answer,
                      submission.status,
                      team.name AS team_name,
                      account.user_name,
                      challenge.title AS challenge_title
                 FROM "Submissions" submission
                 LEFT JOIN "Teams" team ON team.id = submission.team_id
                 LEFT JOIN "AspNetUsers" account ON account.id = submission.user_id
                 LEFT JOIN "GameChallenges" challenge ON challenge.id = submission.challenge_id
                WHERE submission.game_id = $1
                ORDER BY submission.submit_time_utc DESC, submission.id DESC
                LIMIT $2
             )
             SELECT COUNT(*)::bigint,
                    COALESCE(SUM(
                      octet_length(answer)
                      + octet_length(COALESCE(team_name, ''))
                      + octet_length(COALESCE(user_name, ''))
                      + octet_length(COALESCE(challenge_title, ''))
                    ), 0)::bigint,
                    COALESCE(bool_and(status BETWEEN $3 AND $4), TRUE)
               FROM bounded"#,
    )
    .bind(game_id)
    .bind(i64::try_from(MAX_SUBMISSION_EXPORT_ROWS + 1).unwrap_or(i64::MAX))
    .bind(AnswerResult::NotFound as i16)
    .bind(AnswerResult::CheatDetected as i16)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if bounded_count > MAX_SUBMISSION_EXPORT_ROWS as i64 {
        return Err(AppError::payload_too_large(format!(
            "Submission export is limited to {MAX_SUBMISSION_EXPORT_ROWS} rows"
        ))
        .into());
    }
    if !valid_statuses {
        return Err(
            AppError::internal("submission export contains an invalid answer status").into(),
        );
    }
    let row_count = usize::try_from(bounded_count)
        .map_err(|_| AppError::internal("invalid submission export row count"))?;
    let snapshot_bytes = usize::try_from(bounded_bytes)
        .map_err(|_| AppError::internal("invalid submission export snapshot size"))?;
    if snapshot_bytes > MAX_SUBMISSION_EXPORT_SNAPSHOT_BYTES {
        return Err(AppError::payload_too_large(format!(
            "Submission export snapshot is limited to {} MiB",
            MAX_SUBMISSION_EXPORT_SNAPSHOT_BYTES / 1024 / 1024
        ))
        .into());
    }
    permit
        .try_reserve_work(row_count, snapshot_bytes)
        .map_err(SubmissionSnapshotError::Overloaded)?;

    let mut rows = Vec::with_capacity(row_count);
    let mut cursor: Option<(DateTime<Utc>, i32)> = None;
    while rows.len() < row_count {
        let remaining = row_count - rows.len();
        let page_size = remaining.min(SUBMISSION_EXPORT_PAGE_ROWS);
        let (cursor_time, cursor_id) = cursor.unzip();
        let page = sqlx::query_as::<_, SubmissionExportRow>(
            r#"SELECT submission.id,
                      submission.submit_time_utc,
                      team.name AS team_name,
                      account.user_name,
                      challenge.title AS challenge_title,
                      submission.answer,
                      submission.status
                 FROM "Submissions" submission
                 LEFT JOIN "Teams" team ON team.id = submission.team_id
                 LEFT JOIN "AspNetUsers" account ON account.id = submission.user_id
                 LEFT JOIN "GameChallenges" challenge ON challenge.id = submission.challenge_id
                WHERE submission.game_id = $1
                  AND ($2::timestamptz IS NULL OR
                       (submission.submit_time_utc, submission.id) < ($2, $3))
                ORDER BY submission.submit_time_utc DESC, submission.id DESC
                LIMIT $4"#,
        )
        .bind(game_id)
        .bind(cursor_time)
        .bind(cursor_id)
        .bind(i64::try_from(page_size).unwrap_or(i64::MAX))
        .fetch_all(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if page.is_empty() {
            break;
        }
        cursor = page.last().map(|row| (row.submit_time_utc, row.id));
        rows.extend(page);
    }
    if rows.len() != row_count {
        return Err(AppError::internal(
            "submission export snapshot row count changed unexpectedly",
        )
        .into());
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_slots_reject_without_queueing_and_release_on_drop() {
        let admission = MonitorExportAdmission::with_limits(1, 8);
        let first = admission.try_begin().expect("first export admitted");
        assert!(matches!(
            admission.try_begin(),
            Err(MonitorExportAdmissionError::Busy)
        ));
        drop(first);
        assert!(admission.try_begin().is_ok());
    }

    #[test]
    fn row_weight_is_rounded_up_shared_and_released() {
        let admission = MonitorExportAdmission::with_limits(3, 3);
        let mut first = admission.try_begin().unwrap();
        first.try_reserve_work(2_001, 1).unwrap();

        let mut blocked = admission.try_begin().unwrap();
        assert_eq!(
            blocked.try_reserve_work(1, 1),
            Err(MonitorExportAdmissionError::WeightedCapacity)
        );

        drop(first);
        blocked.try_reserve_work(1, 1).unwrap();
    }

    #[test]
    fn byte_weight_can_dominate_row_weight() {
        let admission = MonitorExportAdmission::with_limits(2, 2);
        let mut first = admission.try_begin().unwrap();
        first
            .try_reserve_work(1, BYTES_PER_WEIGHT_UNIT + 1)
            .unwrap();
        let mut second = admission.try_begin().unwrap();
        assert_eq!(
            second.try_reserve_work(1, 1),
            Err(MonitorExportAdmissionError::WeightedCapacity)
        );
    }
}
