//! Admit configured service networks before granting an exact checker target.
use ipnet::IpNet;

pub(super) fn checker_allowed_networks() -> Result<Vec<IpNet>, String> {
    crate::services::ad_vpn::validate_checker_service_routes()?;
    let mut values = crate::services::ad_vpn::service_route_cidrs()?;
    values.extend(
        crate::services::container::profiles::config::checker_service_cidrs(
            &crate::services::container::profiles::config::from_env()
                .map_err(|error| error.to_string())?,
            &crate::services::ad_vpn::services_cidr(),
            crate::services::ad_vpn::kubernetes_services_cidr().as_deref(),
        )
        .map_err(|error| error.to_string())?,
    );
    values.push(crate::services::ad_vpn::client_cidr());
    values
        .into_iter()
        .map(|value| {
            value
                .parse::<IpNet>()
                .map(|network| network.trunc())
                .map_err(|error| format!("invalid checker target network {value:?}: {error}"))
        })
        .collect()
}

pub(super) fn checker_target_is_allowed(target: std::net::IpAddr) -> Result<bool, String> {
    Ok(checker_allowed_networks()?
        .into_iter()
        .any(|network| network.contains(&target)))
}
