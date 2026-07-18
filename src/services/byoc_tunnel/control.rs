//! Cross-replica control messages for the process-local BYOC registry.

use crate::app_state::SharedState;

/// Listen for authorization mutations committed by another API replica and
/// apply them immediately to this process's live tunnel registry. PostgreSQL
/// remains authoritative and each tunnel independently revalidates on a short
/// lease, so a missed best-effort event can delay revocation but cannot preserve
/// an unauthorized session indefinitely.
pub fn start_control_listener(
    st: SharedState,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let mut events = st.events.subscribe();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                received = events.recv() => {
                    let event = match received {
                        Ok(event) => event,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(skipped, "BYOC control listener lagged; DB authorization leases remain authoritative");
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    };
                    if !crate::services::ad_vpn::owns_instance_lease() {
                        continue;
                    }
                    match event.target {
                        "InternalByocRevokeParticipation" => {
                            if let Ok(id) = event.payload.parse::<i32>() {
                                if let Err(error) = st.byoc.disconnect_participation_inner(&st.db, id, false).await {
                                    tracing::warn!(participation = id, %error, "cross-replica BYOC participation revocation failed");
                                }
                            }
                        }
                        "InternalByocRevokeChallenge" => {
                            if let Ok(id) = event.payload.parse::<i32>() {
                                if let Err(error) = st.byoc.disconnect_challenge_inner(&st.db, id, false).await {
                                    tracing::warn!(challenge = id, %error, "cross-replica BYOC challenge revocation failed");
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    })
}
