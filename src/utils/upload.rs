//! Central upload limits.
//!
//! Axum's multipart extractor has a 2 MiB default body limit. Every multipart
//! route must therefore install a body limit that is large enough for its
//! documented payload while remaining bounded before the handler buffers it.

const MIB: usize = 1024 * 1024;
const MULTIPART_OVERHEAD_BYTES: usize = MIB;

pub const IMAGE_FILE_BYTES: usize = 3 * MIB;
pub const WRITEUP_FILE_BYTES: usize = 20 * MIB;
pub const ASSET_FILE_BYTES: usize = 32 * MIB;
pub const ASSET_TOTAL_BYTES: usize = 64 * MIB;
pub const ARCHIVE_FILE_BYTES: usize = 64 * MIB;
/// Repository-generated source ZIPs may add central-directory overhead to the
/// 64 MiB uncompressed source budget.
pub const SOURCE_ARCHIVE_BLOB_BYTES: usize = 72 * MIB;

pub const IMAGE_BODY_BYTES: usize = IMAGE_FILE_BYTES + MULTIPART_OVERHEAD_BYTES;
pub const WRITEUP_BODY_BYTES: usize = WRITEUP_FILE_BYTES + MULTIPART_OVERHEAD_BYTES;
pub const ASSET_BODY_BYTES: usize = ASSET_TOTAL_BYTES + MULTIPART_OVERHEAD_BYTES;
pub const ARCHIVE_BODY_BYTES: usize = ARCHIVE_FILE_BYTES + MULTIPART_OVERHEAD_BYTES;

const _: () = {
    assert!(IMAGE_BODY_BYTES > IMAGE_FILE_BYTES);
    assert!(WRITEUP_BODY_BYTES > WRITEUP_FILE_BYTES);
    assert!(ASSET_BODY_BYTES > ASSET_TOTAL_BYTES);
    assert!(ARCHIVE_BODY_BYTES > ARCHIVE_FILE_BYTES);
};
