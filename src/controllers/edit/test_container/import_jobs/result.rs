//! Size bounds for persisted import results and errors.

use super::ChallengeImportResult;
use crate::utils::error::AppError;

pub(super) const MAX_RESULT_MESSAGES: usize = 256;
pub(super) const MAX_RESULT_MESSAGE_BYTES: usize = 4 * 1024;
pub(super) const MAX_RESULT_MESSAGES_BYTES: usize = 64 * 1024;
const RESULT_SUMMARY_RESERVE_BYTES: usize = 128;

pub(super) fn bounded_error(error: &AppError) -> String {
    let mut message = match error {
        AppError::Database(_) | AppError::Internal(_) => {
            "Challenge import failed due to an internal error".to_string()
        }
        _ => error.to_string(),
    };
    truncate_utf8(&mut message, 16_000);
    message
}

fn truncate_utf8(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
}

pub(super) fn bounded_result(mut result: ChallengeImportResult) -> ChallengeImportResult {
    let original_count = result.messages.len();
    result.messages.truncate(MAX_RESULT_MESSAGES);
    let mut retained_bytes = 0usize;
    for message in &mut result.messages {
        truncate_utf8(message, MAX_RESULT_MESSAGE_BYTES);
        let remaining = MAX_RESULT_MESSAGES_BYTES
            .saturating_sub(RESULT_SUMMARY_RESERVE_BYTES)
            .saturating_sub(retained_bytes);
        truncate_utf8(message, remaining);
        retained_bytes = retained_bytes.saturating_add(message.len());
    }
    result.messages.retain(|message| !message.is_empty());
    if original_count > result.messages.len() {
        if result.messages.len() == MAX_RESULT_MESSAGES {
            result.messages.pop();
        }
        result.messages.push(format!(
            "{} additional import message(s) omitted",
            original_count - result.messages.len()
        ));
    }
    result
}
