use std::collections::HashMap;

use bollard::container::{InspectContainerOptions, ListContainersOptions, StopContainerOptions};
use bollard::network::InspectNetworkOptions;
use rsctf_worker_protocol::{CommandErrorCode, OperatingSystem, WorkloadFence};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::{
    docker_error, label, DockerRuntime, RuntimeError, LABEL_ASSIGNMENT, LABEL_GENERATION,
    LABEL_MANAGED, LABEL_SPEC_HASH, LABEL_WORKER, LABEL_WORKLOAD,
};

const ACL_POLICY_TYPE: &str = "ACL";
const ALLOW_WORKLOAD_PRIORITY: u16 = 100;
const DENY_EGRESS_PRIORITY: u16 = 200;
pub(super) const WINDOWS_DNS_SERVERS_OPTION: &str = "com.docker.network.windowsshim.dnsservers";
pub(super) const WINDOWS_HNS_ID_OPTION: &str = "com.docker.network.windowsshim.hnsid";

pub(super) fn workload_network_driver(operating_system: OperatingSystem) -> &'static str {
    match operating_system {
        OperatingSystem::Linux => "bridge",
        OperatingSystem::Windows => "nat",
    }
}

pub(super) fn workload_network_options(
    operating_system: OperatingSystem,
) -> HashMap<String, String> {
    if operating_system == OperatingSystem::Linux {
        return HashMap::new();
    }
    // Keep Docker's internal service resolver, but give it no external
    // upstream. Otherwise DNS becomes an egress bypass around the HCN ACL.
    HashMap::from([(
        WINDOWS_DNS_SERVERS_OPTION.to_string(),
        "127.0.0.1".to_string(),
    )])
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EndpointSecurity {
    Secure,
    NeedsRepair,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
struct EndpointPolicy {
    #[serde(rename = "Type")]
    kind: String,
    settings: Value,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
#[cfg_attr(not(windows), allow(dead_code))]
struct HcnEndpoint {
    #[serde(rename = "ID")]
    id: String,
    host_compute_network: String,
    #[serde(default)]
    policies: Vec<EndpointPolicy>,
    #[serde(default)]
    ip_configurations: Vec<IpConfiguration>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
#[cfg_attr(not(windows), allow(dead_code))]
struct IpConfiguration {
    ip_address: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
struct AclSettings {
    action: String,
    direction: String,
    remote_addresses: String,
    rule_type: String,
    priority: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    protocols: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    local_addresses: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    local_ports: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote_ports: Option<String>,
}

impl DockerRuntime {
    pub(super) async fn secure_new_windows_container(
        &self,
        container_id: &str,
        network_name: &str,
    ) -> Result<(), RuntimeError> {
        if let Err(error) = self
            .repair_windows_endpoint(container_id, network_name)
            .await
        {
            let _ = self
                .docker
                .remove_container(
                    container_id,
                    Some(bollard::container::RemoveContainerOptions {
                        force: true,
                        v: true,
                        ..Default::default()
                    }),
                )
                .await;
            return Err(error);
        }
        Ok(())
    }

    pub(super) async fn repair_windows_endpoint(
        &self,
        container_id: &str,
        network_name: &str,
    ) -> Result<(), RuntimeError> {
        let (network_id, address, subnet) = self
            .windows_endpoint_identity(container_id, network_name)
            .await?;
        repair_endpoint(network_id, address, subnet).await
    }

    pub(super) async fn verify_started_windows_container(
        &self,
        container_id: &str,
        network_name: &str,
    ) -> Result<(), RuntimeError> {
        let result = match self
            .windows_endpoint_security(container_id, network_name)
            .await
        {
            Ok(EndpointSecurity::Secure) => return Ok(()),
            Ok(EndpointSecurity::NeedsRepair) => Err(RuntimeError::new(
                CommandErrorCode::RuntimeUnavailable,
                "Windows workload endpoint ACL changed during container start",
            )),
            Err(error) => Err(error),
        };
        let _ = self
            .docker
            .remove_container(
                container_id,
                Some(bollard::container::RemoveContainerOptions {
                    force: true,
                    v: true,
                    ..Default::default()
                }),
            )
            .await;
        result
    }

    async fn windows_endpoint_security(
        &self,
        container_id: &str,
        network_name: &str,
    ) -> Result<EndpointSecurity, RuntimeError> {
        let (network_id, address, subnet) = self
            .windows_endpoint_identity(container_id, network_name)
            .await?;
        inspect_endpoint(network_id, address, subnet).await
    }

    async fn windows_endpoint_identity(
        &self,
        container_id: &str,
        network_name: &str,
    ) -> Result<(Uuid, String, String), RuntimeError> {
        let container = self
            .docker
            .inspect_container(container_id, None::<InspectContainerOptions>)
            .await
            .map_err(|error| docker_error("inspect Windows workload endpoint", error))?;
        let labels = container
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref());
        let fence = WorkloadFence {
            workload_id: required_uuid_label(labels, LABEL_WORKLOAD)?,
            assignment_id: required_uuid_label(labels, LABEL_ASSIGNMENT)?,
            generation: label(labels, LABEL_GENERATION)
                .and_then(|value| value.parse().ok())
                .ok_or_else(|| invalid_windows_label(LABEL_GENERATION))?,
        };
        let spec_hash = label(labels, LABEL_SPEC_HASH)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid_windows_label(LABEL_SPEC_HASH))?
            .to_string();
        let endpoint = container
            .network_settings
            .and_then(|settings| settings.networks)
            .and_then(|networks| networks.get(network_name).cloned())
            .ok_or_else(|| {
                RuntimeError::new(
                    CommandErrorCode::RuntimeUnavailable,
                    "Windows container is not attached to its workload network",
                )
            })?;
        let address = endpoint
            .ip_address
            .filter(|value| value.parse::<std::net::Ipv4Addr>().is_ok())
            .ok_or_else(|| {
                RuntimeError::new(
                    CommandErrorCode::RuntimeUnavailable,
                    "Windows container endpoint has no valid IPv4 address",
                )
            })?;

        let network = self
            .docker
            .inspect_network(network_name, None::<InspectNetworkOptions<String>>)
            .await
            .map_err(|error| docker_error("inspect Windows workload network", error))?;
        super::validate_workload_network(
            &network,
            self.worker_id,
            fence,
            &spec_hash,
            OperatingSystem::Windows,
        )?;
        let options = network.options.as_ref().ok_or_else(|| {
            RuntimeError::new(
                CommandErrorCode::RuntimeUnavailable,
                "Windows workload network has no HCN identity",
            )
        })?;
        let network_id = options
            .get(WINDOWS_HNS_ID_OPTION)
            .and_then(|value| Uuid::parse_str(value).ok())
            .ok_or_else(|| {
                RuntimeError::new(
                    CommandErrorCode::RuntimeUnavailable,
                    "Windows workload network has an invalid HCN identity",
                )
            })?;
        let subnet = workload_ipv4_subnet(&network).ok_or_else(|| {
            RuntimeError::new(
                CommandErrorCode::RuntimeUnavailable,
                "Windows workload network has no bounded IPv4 subnet",
            )
        })?;
        Ok((network_id, address, subnet))
    }

    pub(super) async fn audit_windows_endpoints(&self) -> Result<(), RuntimeError> {
        if self.platform.operating_system != OperatingSystem::Windows {
            return Ok(());
        }
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            vec![
                format!("{LABEL_MANAGED}=true"),
                format!("{LABEL_WORKER}={}", self.worker_id),
            ],
        );
        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters,
                ..Default::default()
            }))
            .await
            .map_err(|error| docker_error("audit Windows workload endpoints", error))?;
        for container in containers {
            let Some(container_id) = container.id.as_deref() else {
                continue;
            };
            let inspected = self
                .docker
                .inspect_container(container_id, None::<InspectContainerOptions>)
                .await
                .map_err(|error| docker_error("inspect Windows workload network", error))?;
            let network_names = inspected
                .network_settings
                .and_then(|settings| settings.networks)
                .map(|networks| networks.into_keys().collect::<Vec<_>>())
                .unwrap_or_default();
            let [network_name] = network_names.as_slice() else {
                self.remove_untrusted_windows_container(&container).await?;
                tracing::warn!(
                    container_id,
                    "removed a managed Windows container without exactly one workload network"
                );
                continue;
            };
            match self
                .windows_endpoint_security(container_id, network_name)
                .await
            {
                Ok(EndpointSecurity::Secure) => {}
                Ok(EndpointSecurity::NeedsRepair) => {
                    let was_running = container.state.as_deref() == Some("running");
                    self.stop_untrusted_windows_container(&container).await?;
                    if let Err(error) = self
                        .repair_windows_endpoint(container_id, network_name)
                        .await
                    {
                        self.remove_untrusted_windows_container(&container).await?;
                        tracing::warn!(container_id, %error, "removed a Windows workload whose endpoint ACL could not be repaired");
                        continue;
                    }
                    if was_running {
                        self.docker
                            .start_container(
                                container_id,
                                None::<bollard::container::StartContainerOptions<String>>,
                            )
                            .await
                            .map_err(|error| {
                                docker_error("restart secured Windows workload", error)
                            })?;
                    }
                }
                Err(error) => {
                    self.remove_untrusted_windows_container(&container).await?;
                    tracing::warn!(container_id, %error, "removed a Windows workload whose endpoint ACL could not be verified");
                }
            }
        }
        Ok(())
    }

    async fn stop_untrusted_windows_container(
        &self,
        container: &bollard::models::ContainerSummary,
    ) -> Result<(), RuntimeError> {
        if container.state.as_deref() != Some("running") {
            return Ok(());
        }
        if let Some(id) = container.id.as_deref() {
            self.ready_containers.remove(id);
            self.docker
                .stop_container(id, Some(StopContainerOptions { t: 5 }))
                .await
                .map_err(|error| docker_error("stop unverified Windows workload", error))?;
        }
        Ok(())
    }

    async fn remove_untrusted_windows_container(
        &self,
        container: &bollard::models::ContainerSummary,
    ) -> Result<(), RuntimeError> {
        let Some(id) = container.id.as_deref() else {
            return Ok(());
        };
        self.ready_containers.remove(id);
        self.docker
            .remove_container(
                id,
                Some(bollard::container::RemoveContainerOptions {
                    force: true,
                    v: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|error| docker_error("remove unverified Windows workload", error))
    }
}

fn required_uuid_label(
    labels: Option<&HashMap<String, String>>,
    name: &str,
) -> Result<Uuid, RuntimeError> {
    label(labels, name)
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or_else(|| invalid_windows_label(name))
}

fn invalid_windows_label(name: &str) -> RuntimeError {
    RuntimeError::new(
        CommandErrorCode::RuntimeUnavailable,
        format!("Windows workload endpoint has an invalid {name} label"),
    )
}

fn workload_ipv4_subnet(network: &bollard::models::Network) -> Option<String> {
    let configs = network.ipam.as_ref()?.config.as_deref()?;
    if configs.len() != 1 {
        return None;
    }
    let subnet = configs[0].subnet.as_deref()?;
    super::parse_ipv4_cidr(subnet)?;
    Some(subnet.to_string())
}

fn required_policies(subnet: &str) -> Vec<EndpointPolicy> {
    [
        AclSettings {
            action: "Allow".to_string(),
            direction: "Out".to_string(),
            remote_addresses: subnet.to_string(),
            rule_type: "Switch".to_string(),
            priority: ALLOW_WORKLOAD_PRIORITY,
            protocols: None,
            local_addresses: None,
            local_ports: None,
            remote_ports: None,
        },
        AclSettings {
            action: "Block".to_string(),
            direction: "Out".to_string(),
            remote_addresses: "0.0.0.0/0".to_string(),
            rule_type: "Switch".to_string(),
            priority: DENY_EGRESS_PRIORITY,
            protocols: None,
            local_addresses: None,
            local_ports: None,
            remote_ports: None,
        },
    ]
    .into_iter()
    .map(|settings| EndpointPolicy {
        kind: ACL_POLICY_TYPE.to_string(),
        settings: serde_json::to_value(settings).expect("ACL settings serialize"),
    })
    .collect()
}

fn classify_policies(policies: &[EndpointPolicy], subnet: &str) -> EndpointSecurity {
    let actual = policies
        .iter()
        .filter(|policy| policy.kind.eq_ignore_ascii_case(ACL_POLICY_TYPE))
        .collect::<Vec<_>>();
    let required = required_policies(subnet);
    if actual.len() != required.len()
        || required.iter().any(|expected| {
            !actual
                .iter()
                .any(|policy| acl_policy_matches(policy, expected))
        })
    {
        EndpointSecurity::NeedsRepair
    } else {
        EndpointSecurity::Secure
    }
}

fn acl_policy_matches(actual: &EndpointPolicy, expected: &EndpointPolicy) -> bool {
    let Ok(mut actual) = serde_json::from_value::<AclSettings>(actual.settings.clone()) else {
        return false;
    };
    let Ok(expected) = serde_json::from_value::<AclSettings>(expected.settings.clone()) else {
        return false;
    };
    normalize_optional(&mut actual.protocols);
    normalize_optional(&mut actual.local_addresses);
    normalize_optional(&mut actual.local_ports);
    normalize_optional(&mut actual.remote_ports);
    actual == expected
}

fn normalize_optional(value: &mut Option<String>) {
    if value.as_deref().is_some_and(str::is_empty) {
        *value = None;
    }
}

async fn inspect_endpoint(
    network_id: Uuid,
    address: String,
    subnet: String,
) -> Result<EndpointSecurity, RuntimeError> {
    tokio::task::spawn_blocking(move || {
        let endpoint = platform::find_endpoint(network_id, &address)?;
        Ok(classify_policies(&endpoint.policies, &subnet))
    })
    .await
    .map_err(|error| {
        RuntimeError::new(
            CommandErrorCode::Internal,
            format!("Windows endpoint audit task failed: {error}"),
        )
    })?
}

async fn repair_endpoint(
    network_id: Uuid,
    address: String,
    subnet: String,
) -> Result<(), RuntimeError> {
    tokio::task::spawn_blocking(move || {
        let endpoint = platform::find_endpoint(network_id, &address)?;
        if classify_policies(&endpoint.policies, &subnet) == EndpointSecurity::Secure {
            return Ok(());
        }
        let old_acl = endpoint
            .policies
            .iter()
            .filter(|policy| policy.kind.eq_ignore_ascii_case(ACL_POLICY_TYPE))
            .cloned()
            .collect::<Vec<_>>();
        if !old_acl.is_empty() {
            platform::modify_endpoint(&endpoint.id, "Remove", &old_acl)?;
        }
        platform::modify_endpoint(&endpoint.id, "Add", &required_policies(&subnet))?;
        let verified = platform::find_endpoint(network_id, &address)?;
        if classify_policies(&verified.policies, &subnet) != EndpointSecurity::Secure {
            return Err(RuntimeError::new(
                CommandErrorCode::RuntimeUnavailable,
                "Windows HCN did not retain the required deny-egress policy",
            ));
        }
        Ok(())
    })
    .await
    .map_err(|error| {
        RuntimeError::new(
            CommandErrorCode::Internal,
            format!("Windows endpoint policy task failed: {error}"),
        )
    })?
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use std::ptr::null_mut;

    use windows_sys::core::{GUID, PWSTR};
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::System::HostComputeNetwork::{
        HcnCloseEndpoint, HcnEnumerateEndpoints, HcnModifyEndpoint, HcnOpenEndpoint,
        HcnQueryEndpointProperties,
    };

    use super::*;

    const MAX_HCN_ENDPOINTS: usize = 8_192;
    const MAX_HCN_WIDE_CHARS: usize = 8 * 1024 * 1024;

    pub(super) fn find_endpoint(
        network_id: Uuid,
        address: &str,
    ) -> Result<HcnEndpoint, RuntimeError> {
        let query = wide(r#"{"SchemaVersion":{"Major":2,"Minor":0}}"#)?;
        let mut output: PWSTR = null_mut();
        let mut error: PWSTR = null_mut();
        let status = unsafe { HcnEnumerateEndpoints(query.as_ptr(), &mut output, &mut error) };
        check_hresult("enumerate HCN endpoints", status, error)?;
        let json = unsafe { take_wide(output) }?;
        let ids: Vec<Uuid> = serde_json::from_str(&json).map_err(|error| {
            RuntimeError::new(
                CommandErrorCode::RuntimeUnavailable,
                format!("decode HCN endpoint inventory: {error}"),
            )
        })?;
        if ids.len() > MAX_HCN_ENDPOINTS {
            return Err(RuntimeError::new(
                CommandErrorCode::RuntimeUnavailable,
                "HCN endpoint inventory exceeds the safety limit",
            ));
        }
        let mut matches = Vec::new();
        for id in ids {
            let endpoint = query_endpoint(id)?;
            let same_network =
                Uuid::parse_str(&endpoint.host_compute_network).ok() == Some(network_id);
            let same_address = endpoint
                .ip_configurations
                .iter()
                .any(|config| config.ip_address == address);
            if same_network && same_address {
                matches.push(endpoint);
            }
        }
        if matches.len() != 1 {
            return Err(RuntimeError::new(
                CommandErrorCode::RuntimeUnavailable,
                "Windows workload endpoint could not be uniquely identified in HCN",
            ));
        }
        Ok(matches.remove(0))
    }

    pub(super) fn modify_endpoint(
        endpoint_id: &str,
        request_type: &str,
        policies: &[EndpointPolicy],
    ) -> Result<(), RuntimeError> {
        let id = Uuid::parse_str(endpoint_id).map_err(|_| {
            RuntimeError::new(
                CommandErrorCode::RuntimeUnavailable,
                "HCN returned an invalid endpoint identity",
            )
        })?;
        let handle = open_endpoint(id)?;
        let request = serde_json::json!({
            "ResourceType": "Policy",
            "RequestType": request_type,
            "Settings": {"Policies": policies},
        });
        let settings = wide(&request.to_string())?;
        let mut error: PWSTR = null_mut();
        let status = unsafe { HcnModifyEndpoint(handle.0, settings.as_ptr(), &mut error) };
        check_hresult("modify HCN endpoint policy", status, error)
    }

    fn query_endpoint(id: Uuid) -> Result<HcnEndpoint, RuntimeError> {
        let handle = open_endpoint(id)?;
        let query = wide(r#"{"SchemaVersion":{"Major":2,"Minor":0}}"#)?;
        let mut output: PWSTR = null_mut();
        let mut error: PWSTR = null_mut();
        let status = unsafe {
            HcnQueryEndpointProperties(handle.0, query.as_ptr(), &mut output, &mut error)
        };
        check_hresult("query HCN endpoint", status, error)?;
        let json = unsafe { take_wide(output) }?;
        serde_json::from_str(&json).map_err(|error| {
            RuntimeError::new(
                CommandErrorCode::RuntimeUnavailable,
                format!("decode HCN endpoint: {error}"),
            )
        })
    }

    fn open_endpoint(id: Uuid) -> Result<EndpointHandle, RuntimeError> {
        let guid = guid(id);
        let mut handle = null_mut();
        let mut error: PWSTR = null_mut();
        let status = unsafe { HcnOpenEndpoint(&guid, &mut handle, &mut error) };
        check_hresult("open HCN endpoint", status, error)?;
        if handle.is_null() {
            return Err(RuntimeError::new(
                CommandErrorCode::RuntimeUnavailable,
                "HCN returned an empty endpoint handle",
            ));
        }
        Ok(EndpointHandle(handle))
    }

    struct EndpointHandle(*mut c_void);

    impl Drop for EndpointHandle {
        fn drop(&mut self) {
            let _ = unsafe { HcnCloseEndpoint(self.0) };
        }
    }

    fn guid(id: Uuid) -> GUID {
        let (data1, data2, data3, data4) = id.as_fields();
        GUID {
            data1,
            data2,
            data3,
            data4: *data4,
        }
    }

    fn wide(value: &str) -> Result<Vec<u16>, RuntimeError> {
        if value.contains('\0') {
            return Err(RuntimeError::new(
                CommandErrorCode::Internal,
                "HCN request contains an invalid null character",
            ));
        }
        Ok(value.encode_utf16().chain([0]).collect())
    }

    fn check_hresult(operation: &str, status: i32, error: PWSTR) -> Result<(), RuntimeError> {
        let detail = unsafe { take_wide(error) }.unwrap_or_default();
        if status >= 0 {
            return Ok(());
        }
        let detail = detail.chars().take(2_048).collect::<String>();
        let suffix = if detail.trim().is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        };
        Err(RuntimeError::new(
            CommandErrorCode::RuntimeUnavailable,
            format!("{operation} failed (HRESULT 0x{status:08x}){suffix}"),
        ))
    }

    unsafe fn take_wide(pointer: PWSTR) -> Result<String, RuntimeError> {
        if pointer.is_null() {
            return Ok(String::new());
        }
        let mut length = 0_usize;
        while length < MAX_HCN_WIDE_CHARS && unsafe { *pointer.add(length) } != 0 {
            length += 1;
        }
        let result = if length == MAX_HCN_WIDE_CHARS {
            Err(RuntimeError::new(
                CommandErrorCode::RuntimeUnavailable,
                "HCN returned an unterminated oversized response",
            ))
        } else {
            String::from_utf16(unsafe { std::slice::from_raw_parts(pointer, length) }).map_err(
                |_| {
                    RuntimeError::new(
                        CommandErrorCode::RuntimeUnavailable,
                        "HCN returned invalid UTF-16",
                    )
                },
            )
        };
        unsafe { CoTaskMemFree(pointer.cast()) };
        result
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    pub(super) fn find_endpoint(
        _network_id: Uuid,
        _address: &str,
    ) -> Result<HcnEndpoint, RuntimeError> {
        Err(RuntimeError::unsupported(
            "Windows HCN endpoint policy enforcement requires the Windows worker binary",
        ))
    }

    pub(super) fn modify_endpoint(
        _endpoint_id: &str,
        _request_type: &str,
        _policies: &[EndpointPolicy],
    ) -> Result<(), RuntimeError> {
        Err(RuntimeError::unsupported(
            "Windows HCN endpoint policy enforcement requires the Windows worker binary",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUBNET: &str = "172.30.4.0/24";

    #[test]
    fn exact_fail_closed_acl_is_accepted() {
        assert_eq!(
            classify_policies(&required_policies(SUBNET), SUBNET),
            EndpointSecurity::Secure
        );
    }

    #[test]
    fn missing_or_extra_acl_requires_repair() {
        let mut policies = required_policies(SUBNET);
        policies.pop();
        assert_eq!(
            classify_policies(&policies, SUBNET),
            EndpointSecurity::NeedsRepair
        );

        let mut policies = required_policies(SUBNET);
        policies.push(EndpointPolicy {
            kind: ACL_POLICY_TYPE.to_string(),
            settings: serde_json::json!({
                "Action": "Allow",
                "Direction": "Out",
                "RemoteAddresses": "0.0.0.0/0",
                "RuleType": "Switch",
                "Priority": 50,
            }),
        });
        assert_eq!(
            classify_policies(&policies, SUBNET),
            EndpointSecurity::NeedsRepair
        );
    }

    #[test]
    fn hcn_default_empty_selectors_are_normalized() {
        let mut policies = required_policies(SUBNET);
        let object = policies[0].settings.as_object_mut().unwrap();
        object.insert("Protocols".to_string(), Value::String(String::new()));
        object.insert("LocalAddresses".to_string(), Value::String(String::new()));
        assert_eq!(
            classify_policies(&policies, SUBNET),
            EndpointSecurity::Secure
        );
    }
}
