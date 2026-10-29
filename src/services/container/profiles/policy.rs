//! Compatibility shared by editor, repository imports, and launch paths.
use super::super::ContainerBackendKind;
use crate::{
    app_state::SharedState,
    utils::{
        enums::ChallengeType,
        error::{AppError, AppResult},
    },
};

pub fn validate_selection(
    st: &SharedState,
    profile: Option<&str>,
    kind: ChallengeType,
    byoc: bool,
    aggregate: bool,
    capture: bool,
) -> AppResult<()> {
    let backend = st.containers.profile_backend_kind(profile)?;
    if profile.is_none() {
        return Ok(());
    }
    if !kind.is_container() || byoc || aggregate {
        return Err(AppError::bad_request("Runtime profiles require a platform-hosted single-container challenge; attachments, BYOC, and aggregate worker definitions use their own configuration"));
    }
    if kind.uses_ad_engine()
        && crate::services::ad_vpn::enabled()
        && backend != st.containers.backend_kind()
    {
        return Err(AppError::bad_request(
            "The selected runtime must match this deployment's A&D VPN routing backend",
        ));
    }
    if capture
        && (backend != ContainerBackendKind::Docker
            || st.containers.backend_kind() != ContainerBackendKind::Docker)
    {
        return Err(AppError::bad_request("Host packet capture requires a Docker runtime and Docker network owner; disable traffic capture before switching"));
    }
    Ok(())
}
