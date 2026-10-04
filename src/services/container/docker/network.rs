//! Managed Docker bridge networks for A&D services and isolated workloads.

use std::collections::HashMap;

use bollard::models::{Ipam, IpamConfig, Network};
use bollard::network::{CreateNetworkOptions, InspectNetworkOptions};
use ipnet::Ipv4Net;

use super::super::{scoped_managed_labels, DockerContainerManager, MANAGED_LABEL, SCOPE_LABEL};
use crate::services::docker_admission::docker_admission;
use crate::utils::error::{AppError, AppResult};

/// Legacy Compose-created bridges did not carry an rsctf scope label. Continue
/// to accept those after checking their exact name/subnet/internal shape, but a
/// bridge that declares ownership must belong to this installation.
pub(in crate::services::container) fn network_scope_matches(
    existing: &Network,
    scope: &str,
) -> bool {
    existing
        .labels
        .as_ref()
        .and_then(|labels| labels.get(SCOPE_LABEL))
        .is_none_or(|actual| actual == scope)
}

pub(in crate::services::container) fn bridge_network_matches(
    existing: &Network,
    subnet: Option<&str>,
    internal: bool,
    disable_icc: bool,
) -> bool {
    let managed = existing
        .labels
        .as_ref()
        .and_then(|labels| labels.get(MANAGED_LABEL))
        .is_some();
    let subnet_matches = subnet.is_none_or(|expected| {
        let Ok(expected) = expected.parse::<Ipv4Net>() else {
            return false;
        };
        let actual: Vec<Ipv4Net> = existing
            .ipam
            .as_ref()
            .and_then(|ipam| ipam.config.as_ref())
            .into_iter()
            .flatten()
            .filter_map(|config| config.subnet.as_deref()?.parse::<Ipv4Net>().ok())
            .collect();
        actual.len() == 1 && actual[0] == expected
    });
    let icc_matches = !disable_icc
        || existing.options.as_ref().is_some_and(|options| {
            options
                .get("com.docker.network.bridge.enable_icc")
                .is_some_and(|value| value.eq_ignore_ascii_case("false"))
        });
    existing.driver.as_deref() == Some("bridge")
        && existing.internal == Some(internal)
        && (internal || managed)
        && subnet_matches
        && icc_matches
}

impl DockerContainerManager {
    async fn inspect_network(&self, name: &str) -> AppResult<Option<Network>> {
        let docker = self.client()?;
        Ok(docker_admission()
            .read(
                "inspect_network",
                docker.inspect_network(name, None::<InspectNetworkOptions<String>>),
            )
            .await?
            .ok())
    }

    pub(in crate::services::container) async fn ensure_bridge_network(
        &self,
        name: &str,
        subnet: Option<&str>,
        internal: bool,
        disable_icc: bool,
    ) -> AppResult<()> {
        let docker = self.client()?;
        if let Some(existing) = self.inspect_network(name).await? {
            if bridge_network_matches(&existing, subnet, internal, disable_icc)
                && network_scope_matches(&existing, &self.scope)
            {
                return Ok(());
            }
            return Err(AppError::internal(format!(
                "Docker network {name} does not match the required bridge/Internal={internal}/subnet={subnet:?} configuration; recreate it before launching A&D services",
            )));
        }

        let ipam = match subnet {
            Some(subnet) => Ipam {
                config: Some(vec![IpamConfig {
                    subnet: Some(subnet.to_string()),
                    ..Default::default()
                }]),
                ..Default::default()
            },
            None => Ipam::default(),
        };
        let options = if disable_icc {
            HashMap::from([(
                "com.docker.network.bridge.enable_icc".to_string(),
                "false".to_string(),
            )])
        } else {
            HashMap::new()
        };
        let opts = CreateNetworkOptions {
            name: name.to_string(),
            check_duplicate: true,
            driver: "bridge".to_string(),
            internal,
            ipam,
            labels: scoped_managed_labels(&self.scope),
            options,
            ..Default::default()
        };
        let created = docker_admission()
            .lifecycle("create_network", docker.create_network(opts))
            .await?;
        match created {
            Ok(_) => Ok(()),
            Err(create_error) => {
                // A concurrent provision may have won the create race.
                match self.inspect_network(name).await? {
                    Some(existing)
                        if bridge_network_matches(&existing, subnet, internal, disable_icc)
                            && network_scope_matches(&existing, &self.scope) =>
                    {
                        Ok(())
                    }
                    _ => Err(AppError::internal(format!(
                        "failed to create Docker network {name}: {create_error}"
                    ))),
                }
            }
        }
    }
}
