//! Deployment-wide admission for anonymous public cryptographic work.

use super::{check_async, Policy};

/// Public work whose source-scoped route budget also needs one aggregate
/// decision shared by every replica.
#[derive(Clone, Copy, Debug)]
pub(crate) enum PublicSecurityWork {
    TeamSignature,
    PowChallenge,
}

pub(crate) async fn admit_public_security(
    work: PublicSecurityWork,
) -> crate::utils::error::AppResult<()> {
    let policy = match work {
        PublicSecurityWork::TeamSignature => Policy::TeamSignatureAggregate,
        PublicSecurityWork::PowChallenge => Policy::PowChallengeAggregate,
    };
    check_async(policy, "deployment".to_owned())
        .await
        .map_err(crate::utils::error::AppError::too_many_requests)
}
