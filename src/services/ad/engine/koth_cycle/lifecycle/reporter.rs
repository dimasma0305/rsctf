use crate::app_state::SharedState;
use crate::services::ad::koth_reporter::{self, TargetReporterRoute, TargetReporterRuntime};
use crate::utils::error::AppResult;

use super::CycleRow;

pub(super) async fn ensure(
    st: &SharedState,
    cycle: &CycleRow,
) -> AppResult<Option<TargetReporterRuntime>> {
    let base_url = st.config.koth_reporter_base_url.as_deref();
    let backend_identity = if base_url.is_some() {
        st.containers.managed_callback_routing_identity()?
    } else {
        None
    };
    koth_reporter::ensure_for_cycle(
        st.pg(),
        TargetReporterRoute {
            base_url,
            bind_addr: &st.config.bind_addr,
            backend_kind: st.containers.backend_kind(),
            backend_identity: backend_identity.as_deref(),
        },
        cycle.id,
        cycle.game_id,
        cycle.challenge_id,
        cycle.reset_attempt,
    )
    .await
}
