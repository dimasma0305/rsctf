//! Deployment-wide admission for trusted solve-receipt issuance.

use sha2::{Digest, Sha256};

use super::{check_weighted_async, Policy};

/// Bound signing and retained-row growth across replicas. Issuer and
/// participation weights impose tighter budgets than the shared deployment
/// envelope without exposing verifier names in Redis keys.
pub(crate) async fn admit_solve_receipt_issuance(
    issuer: &str,
    participation_id: i32,
) -> Result<(), u64> {
    let issuer = hex::encode(Sha256::digest(issuer.as_bytes()));
    let global = check_weighted_async(Policy::SolveReceipt, "global".to_owned(), 1);
    let issuer = check_weighted_async(Policy::SolveReceipt, format!("issuer:{issuer}"), 4);
    let participation = check_weighted_async(
        Policy::SolveReceipt,
        format!("participation:{participation_id}"),
        8,
    );
    let (global, issuer, participation) = tokio::join!(global, issuer, participation);
    [global, issuer, participation]
        .into_iter()
        .filter_map(Result::err)
        .max()
        .map_or(Ok(()), Err)
}
