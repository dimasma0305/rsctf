use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Public-HTTPS enrollment request. Deliberately does not implement `Debug` so
/// ordinary error logging cannot expose the one-time token.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollmentRequest {
    pub token: String,
    pub csr_pem: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollmentResponse {
    pub worker_id: Uuid,
    pub control_address: String,
    pub data_address: String,
    pub server_name: String,
    pub certificate_pem: String,
    pub ca_pem: String,
}
