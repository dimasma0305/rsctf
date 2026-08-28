//! Cheap, bounded admission and machine-readable outcomes for BYOC agent upgrades.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, Weak};

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(super) const BYOC_AGENT_STATE_HEADER: &str = "x-rsctf-byoc-state";
pub(super) const BYOC_AGENT_RETRY_AFTER_SECONDS: u64 = 5;
const MAX_CONCURRENT_AGENT_HANDSHAKES: usize = 128;
const MAX_AGENT_HANDSHAKE_PARTICIPATIONS: usize = 4_096;
const MAX_AGENT_HANDSHAKE_CAPABILITIES: usize = 4_096;

pub(super) static AGENT_HANDSHAKE_ADMISSION: LazyLock<AgentHandshakeAdmission> =
    LazyLock::new(|| AgentHandshakeAdmission::new(MAX_CONCURRENT_AGENT_HANDSHAKES));

pub(super) struct AgentHandshakeAdmission {
    global: Arc<Semaphore>,
    identities: Mutex<AgentHandshakeGates>,
}

#[derive(Default)]
struct AgentHandshakeGates {
    participations: HashMap<i32, Weak<Semaphore>>,
    capabilities: HashMap<(i32, i32), Weak<Semaphore>>,
}

pub(super) struct AgentHandshakePermit {
    _global: OwnedSemaphorePermit,
    _participation: OwnedSemaphorePermit,
    _capability: OwnedSemaphorePermit,
}

impl AgentHandshakeAdmission {
    pub(super) fn new(global_limit: usize) -> Self {
        Self {
            global: Arc::new(Semaphore::new(global_limit)),
            identities: Mutex::new(AgentHandshakeGates::default()),
        }
    }

    pub(super) fn try_admit(
        &self,
        participation_id: i32,
        challenge_id: i32,
    ) -> Option<AgentHandshakePermit> {
        let global = self.global.clone().try_acquire_owned().ok()?;
        let (participation, capability) = {
            let mut identities = self
                .identities
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if identities.participations.len() >= MAX_AGENT_HANDSHAKE_PARTICIPATIONS {
                identities
                    .participations
                    .retain(|_, gate| gate.strong_count() > 0);
            }
            if identities.capabilities.len() >= MAX_AGENT_HANDSHAKE_CAPABILITIES {
                identities
                    .capabilities
                    .retain(|_, gate| gate.strong_count() > 0);
            }

            let participation = match identities
                .participations
                .get(&participation_id)
                .and_then(Weak::upgrade)
            {
                Some(gate) => gate,
                None if identities.participations.len() < MAX_AGENT_HANDSHAKE_PARTICIPATIONS => {
                    let gate = Arc::new(Semaphore::new(4));
                    identities
                        .participations
                        .insert(participation_id, Arc::downgrade(&gate));
                    gate
                }
                None => return None,
            };
            let capability = match identities
                .capabilities
                .get(&(participation_id, challenge_id))
                .and_then(Weak::upgrade)
            {
                Some(gate) => gate,
                None if identities.capabilities.len() < MAX_AGENT_HANDSHAKE_CAPABILITIES => {
                    let gate = Arc::new(Semaphore::new(1));
                    identities
                        .capabilities
                        .insert((participation_id, challenge_id), Arc::downgrade(&gate));
                    gate
                }
                None => return None,
            };
            (participation, capability)
        };
        Some(AgentHandshakePermit {
            _global: global,
            _participation: participation.try_acquire_owned().ok()?,
            _capability: capability.try_acquire_owned().ok()?,
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ByocAgentStateBody {
    title: &'static str,
    state: &'static str,
    terminal: bool,
    retry_after: Option<u64>,
}

pub(super) fn byoc_agent_state_response(
    status: StatusCode,
    title: &'static str,
    state: &'static str,
    terminal: bool,
    retry_after: Option<u64>,
) -> Response {
    let mut response = (
        status,
        axum::Json(ByocAgentStateBody {
            title,
            state,
            terminal,
            retry_after,
        }),
    )
        .into_response();
    response.headers_mut().insert(
        BYOC_AGENT_STATE_HEADER,
        axum::http::HeaderValue::from_static(state),
    );
    if let Some(retry_after) = retry_after {
        if let Ok(value) = axum::http::HeaderValue::from_str(&retry_after.to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
    }
    response
}
