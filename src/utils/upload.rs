//! Central upload limits.
//!
//! Axum's multipart extractor has a 2 MiB default body limit. Every multipart
//! route must therefore install a body limit that is large enough for its
//! documented payload while remaining bounded before the handler buffers it.
//! Size limits alone do not bound aggregate memory: many slow uploads can each
//! hold a nearly-complete field. Buffered handlers must reserve their maximum
//! payload here before polling the multipart stream.

use tokio::sync::{Semaphore, SemaphorePermit};
use uuid::Uuid;

use crate::utils::error::{AppError, AppResult};

pub const OPERATION_ID_HEADER: &str = "x-rsctf-operation-id";

/// Parse the stable identity required by replayable multipart mutations.
/// Keeping this in the upload boundary gives every named blob owner the same
/// strict contract before it buffers a request body or touches storage.
pub fn required_operation_id(headers: &axum::http::HeaderMap) -> AppResult<Uuid> {
    let raw = headers
        .get(OPERATION_ID_HEADER)
        .ok_or_else(|| AppError::bad_request("X-RSCTF-Operation-Id header is required"))?
        .to_str()
        .map_err(|_| AppError::bad_request("X-RSCTF-Operation-Id must be an ASCII UUID"))?;
    Uuid::parse_str(raw)
        .ok()
        .filter(|operation_id| !operation_id.is_nil())
        .ok_or_else(|| AppError::bad_request("X-RSCTF-Operation-Id must be a non-zero UUID"))
}

const MIB: usize = 1024 * 1024;
const MULTIPART_OVERHEAD_BYTES: usize = MIB;

/// Maximum aggregate multipart payload that handlers may buffer in one process.
/// Permits are measured in MiB and acquired for the route's full documented
/// maximum, so a misleading or absent Content-Length cannot evade admission.
const BUFFERED_UPLOAD_BUDGET_MIB: usize = 256;
static BUFFERED_UPLOAD_BUDGET: Semaphore = Semaphore::const_new(BUFFERED_UPLOAD_BUDGET_MIB);

/// Absolute wall-clock deadline for transferring any request body. The shared
/// memory admission is the primary bound; this deadline also releases permits
/// held by clients that trickle bytes indefinitely.
pub const REQUEST_BODY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(300);

pub const IMAGE_FILE_BYTES: usize = 3 * MIB;
pub const WRITEUP_FILE_BYTES: usize = 20 * MIB;
/// Generic challenge attachments may include signed desktop clients. Uploads
/// remain admin-only and bounded; the download path streams files this large.
pub const ASSET_FILE_BYTES: usize = 192 * MIB;
pub const ASSET_TOTAL_BYTES: usize = 192 * MIB;
/// Multipart field count is bounded independently from bytes so thousands of
/// tiny parts cannot turn one HTTP token into unbounded serial storage work.
pub const ASSET_FILE_COUNT: usize = 32;
/// Named single-file forms accept a few metadata fields but never an
/// attacker-controlled number of tiny multipart parts.
pub const SINGLE_FILE_FIELD_COUNT: usize = 4;
pub const ARCHIVE_FILE_BYTES: usize = 64 * MIB;
/// Repository-generated source ZIPs may add central-directory overhead to the
/// 64 MiB uncompressed source budget.
pub const SOURCE_ARCHIVE_BLOB_BYTES: usize = 72 * MIB;

pub const IMAGE_BODY_BYTES: usize = IMAGE_FILE_BYTES + MULTIPART_OVERHEAD_BYTES;
pub const WRITEUP_BODY_BYTES: usize = WRITEUP_FILE_BYTES + MULTIPART_OVERHEAD_BYTES;
pub const ASSET_BODY_BYTES: usize = ASSET_TOTAL_BYTES + MULTIPART_OVERHEAD_BYTES;
pub const ARCHIVE_BODY_BYTES: usize = ARCHIVE_FILE_BYTES + MULTIPART_OVERHEAD_BYTES;

/// Reserve enough of the process-wide budget for a handler's maximum buffered
/// payload. This is deliberately non-blocking: queued request bodies would still
/// occupy connections and could already be arriving at the reverse proxy.
pub fn reserve_buffered(max_buffer_bytes: usize) -> AppResult<SemaphorePermit<'static>> {
    let permits = max_buffer_bytes.div_ceil(MIB);
    let permits = u32::try_from(permits)
        .map_err(|_| AppError::internal("upload memory reservation overflow"))?;
    BUFFERED_UPLOAD_BUDGET
        .try_acquire_many(permits)
        .map_err(|_| AppError::unavailable("Upload capacity is busy; retry shortly"))
}

const _: () = {
    assert!(IMAGE_BODY_BYTES > IMAGE_FILE_BYTES);
    assert!(WRITEUP_BODY_BYTES > WRITEUP_FILE_BYTES);
    assert!(ASSET_BODY_BYTES > ASSET_TOTAL_BYTES);
    assert!(ARCHIVE_BODY_BYTES > ARCHIVE_FILE_BYTES);
    assert!(BUFFERED_UPLOAD_BUDGET_MIB * MIB >= ASSET_BODY_BYTES);
};

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    #[test]
    fn every_buffered_route_fits_the_shared_budget() {
        for limit in [
            IMAGE_BODY_BYTES,
            WRITEUP_BODY_BYTES,
            ASSET_BODY_BYTES,
            ARCHIVE_BODY_BYTES,
        ] {
            assert!(limit.div_ceil(MIB) <= BUFFERED_UPLOAD_BUDGET_MIB);
        }
    }

    #[test]
    fn multipart_operation_identity_is_required_and_strict() {
        let mut headers = HeaderMap::new();
        assert!(required_operation_id(&headers).is_err());
        headers.insert(OPERATION_ID_HEADER, HeaderValue::from_static("not-a-uuid"));
        assert!(required_operation_id(&headers).is_err());
        headers.insert(
            OPERATION_ID_HEADER,
            HeaderValue::from_static("00000000-0000-0000-0000-000000000000"),
        );
        assert!(required_operation_id(&headers).is_err());

        let expected = Uuid::new_v4();
        headers.insert(
            OPERATION_ID_HEADER,
            HeaderValue::from_str(&expected.to_string()).unwrap(),
        );
        assert_eq!(required_operation_id(&headers).unwrap(), expected);
    }
}
