//! Deterministic identities for endpoint-scoped blob staging operations.

use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Keep the staging table globally unique without making operation IDs from
/// unrelated endpoints conflict with each other.
pub(crate) fn scoped_operation_id(root: Uuid, scope: &str, ordinal: u64) -> Uuid {
    let mut digest = Sha256::new();
    digest.update(scope.as_bytes());
    digest.update(root.as_bytes());
    digest.update(ordinal.to_be_bytes());
    let digest = digest.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}
