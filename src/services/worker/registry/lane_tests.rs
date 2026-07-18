use rsctf_worker_protocol::SessionFence;
use uuid::Uuid;

use super::{RegistryConfig, SessionContext, WorkerRegistry};
use crate::services::worker::WorkerError;

fn fence(epoch: u64) -> SessionFence {
    SessionFence {
        session_id: Uuid::new_v4(),
        session_epoch: epoch,
    }
}

#[tokio::test]
async fn negotiated_data_lane_limit_is_enforced_per_session() {
    let registry = WorkerRegistry::new(RegistryConfig {
        max_data_lanes_per_worker: 4,
        ..RegistryConfig::default()
    });
    let worker = Uuid::new_v4();
    let session_fence = fence(1);
    registry
        .register_authenticated_control(
            SessionContext {
                worker_id: worker,
                boot_id: Uuid::new_v4(),
                certificate_fingerprint_sha256: [7; 32],
                fence: session_fence,
            },
            1,
        )
        .await
        .unwrap();

    assert!(registry
        .lane_session(worker, &session_fence, 0)
        .await
        .is_ok());
    assert!(matches!(
        registry.lane_session(worker, &session_fence, 1).await,
        Err(WorkerError::Protocol("data lane number exceeds limit"))
    ));
}

#[tokio::test]
async fn replacement_session_uses_its_own_negotiated_lane_limit() {
    let registry = WorkerRegistry::new(RegistryConfig {
        max_data_lanes_per_worker: 4,
        ..RegistryConfig::default()
    });
    let worker = Uuid::new_v4();
    let old_fence = fence(1);
    registry
        .register_authenticated_control(
            SessionContext {
                worker_id: worker,
                boot_id: Uuid::new_v4(),
                certificate_fingerprint_sha256: [7; 32],
                fence: old_fence,
            },
            4,
        )
        .await
        .unwrap();
    let new_fence = fence(2);
    registry
        .register_authenticated_control(
            SessionContext {
                worker_id: worker,
                boot_id: Uuid::new_v4(),
                certificate_fingerprint_sha256: [7; 32],
                fence: new_fence,
            },
            1,
        )
        .await
        .unwrap();

    assert!(matches!(
        registry.lane_session(worker, &old_fence, 0).await,
        Err(WorkerError::StaleSession)
    ));
    assert!(matches!(
        registry.lane_session(worker, &new_fence, 1).await,
        Err(WorkerError::Protocol("data lane number exceeds limit"))
    ));
}
