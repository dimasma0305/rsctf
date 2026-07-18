//! Fail-closed capture-owner shutdown sequencing.

use crate::app_state::SharedState;
use crate::services::capture_safety::{FULL_FAIL_CLOSED_WAIT, KERNEL_FAIL_CLOSED_WAIT};

use super::health::{self, OwnerHeartbeat, OwnerToken};
use super::owner::{release as release_owner, OwnerLease};
use super::CaptureRegistry;

const DURABLE_FENCE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const ROUTE_FENCE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

fn route_expiry_wait(durably_fenced: bool, kernel_fenced: bool) -> Option<std::time::Duration> {
    match (durably_fenced, kernel_fenced) {
        (true, true) => None,
        (true, false) => Some(KERNEL_FAIL_CLOSED_WAIT),
        (false, _) => Some(FULL_FAIL_CLOSED_WAIT),
    }
}

async fn begin_drain(owner: &mut OwnerLease, token: OwnerToken) -> Result<(), String> {
    tokio::time::timeout(
        DURABLE_FENCE_TIMEOUT,
        health::begin_drain(&mut **owner.connection_mut(), token),
    )
    .await
    .map_err(|_| "traffic capture durable drain timed out".to_string())?
    .map_err(|error| error.to_string())
}

async fn release_durable(owner: &mut OwnerLease, token: OwnerToken) -> Result<(), String> {
    tokio::time::timeout(
        DURABLE_FENCE_TIMEOUT,
        health::release(&mut **owner.connection_mut(), token),
    )
    .await
    .map_err(|_| "traffic capture durable release timed out".to_string())?
    .map_err(|error| error.to_string())
}

async fn sync_routes(state: &SharedState) -> Result<(), String> {
    tokio::time::timeout(
        ROUTE_FENCE_TIMEOUT,
        crate::services::ad_vpn::ensure_hub_and_sync(&state.db),
    )
    .await
    .map_err(|_| "traffic capture route fence timed out".to_string())?
    .map_err(|error| error.to_string())
}

pub(super) async fn drain_owner(
    state: &SharedState,
    owner_token: Option<OwnerToken>,
    heartbeat: Option<OwnerHeartbeat>,
    mut lease: Option<OwnerLease>,
    captures: &mut CaptureRegistry,
) {
    state.readiness.begin_capture_restore();
    if let Some(pulse) = heartbeat {
        pulse.stop().await;
    }

    if let (Some(token), Some(owner)) = (owner_token, lease.as_mut()) {
        let drain = begin_drain(owner, token).await;
        let mut durably_fenced = drain.is_ok();
        let graceful_fence = match drain {
            Ok(()) => sync_routes(state).await,
            Err(error) => Err(error),
        };
        let mut durable_released = false;

        if let Err(error) = graceful_fence {
            tracing::error!(%error, "traffic capture graceful fence failed");
            if !durably_fenced {
                match release_durable(owner, token).await {
                    Ok(()) => {
                        durably_fenced = true;
                        durable_released = true;
                    }
                    Err(release_error) => tracing::error!(
                        %release_error,
                        "traffic capture emergency durable fence failed"
                    ),
                }
            }

            let kernel_fenced = match crate::services::ad_vpn::fence_capture_routes().await {
                Ok(()) => true,
                Err(kernel_error) => {
                    tracing::error!(%kernel_error, "direct capture route fence failed");
                    false
                }
            };
            if let Some(wait) = route_expiry_wait(durably_fenced, kernel_fenced) {
                tracing::warn!(
                    wait_seconds = wait.as_secs(),
                    "keeping packet capture alive until route admission expires"
                );
                tokio::time::sleep(wait).await;
            }
        }

        captures.stop_all().await;
        if !durable_released {
            if let Err(error) = release_durable(owner, token).await {
                tracing::warn!(%error, "traffic capture durable ownership release failed");
            }
        }
    } else {
        captures.stop_all().await;
    }

    if let Some(connection) = lease {
        if let Err(error) = release_owner(connection).await {
            tracing::warn!(%error, "traffic capture advisory ownership release failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_expiry_wait_covers_repopulation_and_kernel_timeout() {
        assert_eq!(route_expiry_wait(true, true), None);
        assert_eq!(
            route_expiry_wait(true, false),
            Some(KERNEL_FAIL_CLOSED_WAIT)
        );
        assert_eq!(route_expiry_wait(false, true), Some(FULL_FAIL_CLOSED_WAIT));
        assert_eq!(route_expiry_wait(false, false), Some(FULL_FAIL_CLOSED_WAIT));
        assert!(FULL_FAIL_CLOSED_WAIT > KERNEL_FAIL_CLOSED_WAIT);
    }
}
