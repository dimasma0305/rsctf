use std::collections::{BTreeMap, HashSet};
use std::fmt::Write;
use std::net::Ipv6Addr;
use std::ops::Deref;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

const MAX_SERVICES: usize = 32;
const MAX_PORTS_PER_SERVICE: usize = 32;
const MAX_REPLICAS_PER_SERVICE: u16 = 64;
const MAX_ENVIRONMENT_ENTRIES: usize = 128;
const MAX_ENVIRONMENT_KEY_BYTES: usize = 128;
const MAX_ENVIRONMENT_VALUE_BYTES: usize = 16 * 1024;
pub const MAX_ARCHITECTURE_BYTES: usize = 64;
pub const MAX_WINDOWS_BUILD_BYTES: usize = 128;
pub const MAX_REGISTRY_REPOSITORY_BYTES: usize = 255;
const MAX_FLAG_PATH_BYTES: usize = 1_024;
/// Keeps one status/inventory item comfortably below the control-frame limit.
pub const MAX_WORKLOAD_REPLICAS: usize = 512;
/// Leaves framing room for command identifiers, fences, and future metadata.
pub const MAX_WORKLOAD_SPEC_BYTES: usize = 192 * 1024;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GameKind {
    Jeopardy,
    AttackDefense,
    KingOfTheHill,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OperatingSystem {
    Linux,
    Windows,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Platform {
    pub operating_system: OperatingSystem,
    pub architecture: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows_build: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ImageIdentity {
    RegistryDigest { repository: String, digest: String },
    WorkerLocal { worker_id: Uuid, image_id: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceLimits {
    pub cpu_millis: u32,
    pub memory_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PortProtocol {
    Tcp,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServicePort {
    pub name: String,
    pub container_port: u16,
    pub protocol: PortProtocol,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceSpec {
    pub name: String,
    pub image: ImageIdentity,
    pub resources: ResourceLimits,
    pub replicas: u16,
    /// A replica count greater than one is accepted only when this is true.
    pub stateless: bool,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    pub ports: Vec<ServicePort>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EndpointRef {
    pub service: String,
    pub port: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FlagTarget {
    pub service: String,
    /// Guest path interpreted by the selected operating-system runtime.
    pub path: String,
}

/// Unvalidated workload input. Convert it to [`ValidatedWorkloadSpec`] before use.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkloadSpec {
    pub game_kind: GameKind,
    pub platform: Platform,
    pub services: Vec<ServiceSpec>,
    pub primary_endpoint: EndpointRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flag_target: Option<FlagTarget>,
}

/// A workload whose structural and game-mode invariants have been checked.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "WorkloadSpec", into = "WorkloadSpec")]
pub struct ValidatedWorkloadSpec(WorkloadSpec);

impl ValidatedWorkloadSpec {
    pub fn as_spec(&self) -> &WorkloadSpec {
        &self.0
    }

    pub fn into_inner(self) -> WorkloadSpec {
        self.0
    }

    /// Stable hash for an already-ordered workload specification.
    pub fn spec_hash(&self) -> Result<String, serde_json::Error> {
        let bytes = serde_json::to_vec(&self.0)?;
        let digest = Sha256::digest(bytes);
        let mut output = String::with_capacity(64);
        for byte in digest {
            let _ = write!(&mut output, "{byte:02x}");
        }
        Ok(output)
    }
}

impl Deref for ValidatedWorkloadSpec {
    type Target = WorkloadSpec;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<ValidatedWorkloadSpec> for WorkloadSpec {
    fn from(value: ValidatedWorkloadSpec) -> Self {
        value.0
    }
}

impl TryFrom<WorkloadSpec> for ValidatedWorkloadSpec {
    type Error = WorkloadValidationError;

    fn try_from(spec: WorkloadSpec) -> Result<Self, Self::Error> {
        validate_workload(&spec)?;
        Ok(Self(spec))
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum WorkloadValidationError {
    #[error("a workload must contain between 1 and {MAX_SERVICES} services")]
    InvalidServiceCount,
    #[error("service name `{0}` is invalid or duplicated")]
    InvalidServiceName(String),
    #[error("service `{service}` must request between 1 and {MAX_REPLICAS_PER_SERVICE} replicas")]
    InvalidReplicaCount { service: String },
    #[error("only explicitly stateless Jeopardy services can have replicas")]
    ReplicasNotAllowed,
    #[error("a workload cannot contain more than {MAX_WORKLOAD_REPLICAS} total replicas")]
    TooManyReplicas,
    #[error("Attack/Defense and King-of-the-Hill workloads must have exactly one service and one replica")]
    CompetitiveModeShape,
    #[error("service `{service}` has invalid resource limits")]
    InvalidResources { service: String },
    #[error("service `{service}` has too many environment entries")]
    TooManyEnvironmentEntries { service: String },
    #[error("environment key `{key}` in service `{service}` is invalid")]
    InvalidEnvironment { service: String, key: String },
    #[error("service `{service}` must contain between 1 and {MAX_PORTS_PER_SERVICE} ports")]
    InvalidPortCount { service: String },
    #[error("port `{port}` in service `{service}` is invalid or duplicated")]
    InvalidPort { service: String, port: String },
    #[error("primary endpoint does not select an existing service port")]
    InvalidPrimaryEndpoint,
    #[error("flag target does not select an existing service or has an invalid path")]
    InvalidFlagTarget,
    #[error("platform is invalid: {0}")]
    InvalidPlatform(String),
    #[error("image identity for service `{service}` is invalid")]
    InvalidImage { service: String },
    #[error("the workload specification exceeds {MAX_WORKLOAD_SPEC_BYTES} bytes")]
    SpecTooLarge,
}

fn validate_workload(spec: &WorkloadSpec) -> Result<(), WorkloadValidationError> {
    validate_platform(&spec.platform)?;
    if spec.services.is_empty() || spec.services.len() > MAX_SERVICES {
        return Err(WorkloadValidationError::InvalidServiceCount);
    }
    if spec.game_kind != GameKind::Jeopardy && spec.services.len() != 1 {
        return Err(WorkloadValidationError::CompetitiveModeShape);
    }

    let mut service_names = HashSet::new();
    let mut total_replicas = 0_usize;
    for service in &spec.services {
        if !valid_dns_label(&service.name) || !service_names.insert(service.name.as_str()) {
            return Err(WorkloadValidationError::InvalidServiceName(
                service.name.clone(),
            ));
        }
        validate_image(&service.image).map_err(|()| WorkloadValidationError::InvalidImage {
            service: service.name.clone(),
        })?;
        if service.resources.cpu_millis == 0 || service.resources.memory_bytes == 0 {
            return Err(WorkloadValidationError::InvalidResources {
                service: service.name.clone(),
            });
        }
        if service.replicas == 0 || service.replicas > MAX_REPLICAS_PER_SERVICE {
            return Err(WorkloadValidationError::InvalidReplicaCount {
                service: service.name.clone(),
            });
        }
        total_replicas = total_replicas.saturating_add(usize::from(service.replicas));
        if total_replicas > MAX_WORKLOAD_REPLICAS {
            return Err(WorkloadValidationError::TooManyReplicas);
        }
        if service.replicas > 1 && (spec.game_kind != GameKind::Jeopardy || !service.stateless) {
            return Err(WorkloadValidationError::ReplicasNotAllowed);
        }
        if spec.game_kind != GameKind::Jeopardy && service.replicas != 1 {
            return Err(WorkloadValidationError::CompetitiveModeShape);
        }
        if service.environment.len() > MAX_ENVIRONMENT_ENTRIES {
            return Err(WorkloadValidationError::TooManyEnvironmentEntries {
                service: service.name.clone(),
            });
        }
        for (key, value) in &service.environment {
            if !valid_environment_key(key)
                || value.len() > MAX_ENVIRONMENT_VALUE_BYTES
                || value.as_bytes().contains(&0)
            {
                return Err(WorkloadValidationError::InvalidEnvironment {
                    service: service.name.clone(),
                    key: key.clone(),
                });
            }
        }
        validate_ports(service)?;
    }

    let endpoint_valid = spec.services.iter().any(|service| {
        service.name == spec.primary_endpoint.service
            && service
                .ports
                .iter()
                .any(|port| port.name == spec.primary_endpoint.port)
    });
    if !endpoint_valid {
        return Err(WorkloadValidationError::InvalidPrimaryEndpoint);
    }

    if let Some(target) = &spec.flag_target {
        let service_exists = spec
            .services
            .iter()
            .any(|service| service.name == target.service);
        if !service_exists
            || target.path.trim().is_empty()
            || target.path.len() > MAX_FLAG_PATH_BYTES
            || target.path.as_bytes().contains(&0)
        {
            return Err(WorkloadValidationError::InvalidFlagTarget);
        }
    }

    if serde_json::to_vec(spec)
        .map(|encoded| encoded.len() > MAX_WORKLOAD_SPEC_BYTES)
        .unwrap_or(true)
    {
        return Err(WorkloadValidationError::SpecTooLarge);
    }

    Ok(())
}

fn validate_platform(platform: &Platform) -> Result<(), WorkloadValidationError> {
    if !is_valid_architecture(&platform.architecture) {
        return Err(WorkloadValidationError::InvalidPlatform(
            "architecture must be a bounded ASCII token".to_string(),
        ));
    }
    if platform
        .windows_build
        .as_ref()
        .is_some_and(|build| build.trim().is_empty() || build.len() > MAX_WINDOWS_BUILD_BYTES)
    {
        return Err(WorkloadValidationError::InvalidPlatform(
            "Windows build is empty or too long".to_string(),
        ));
    }
    match platform.operating_system {
        OperatingSystem::Linux if platform.windows_build.is_some() => {
            Err(WorkloadValidationError::InvalidPlatform(
                "a Linux platform cannot declare a Windows build".to_string(),
            ))
        }
        _ => Ok(()),
    }
}

fn validate_ports(service: &ServiceSpec) -> Result<(), WorkloadValidationError> {
    if service.ports.is_empty() || service.ports.len() > MAX_PORTS_PER_SERVICE {
        return Err(WorkloadValidationError::InvalidPortCount {
            service: service.name.clone(),
        });
    }
    let mut names = HashSet::new();
    let mut numbers = HashSet::new();
    for port in &service.ports {
        if !valid_dns_label(&port.name)
            || port.container_port == 0
            || !names.insert(port.name.as_str())
            || !numbers.insert((port.container_port, port.protocol))
        {
            return Err(WorkloadValidationError::InvalidPort {
                service: service.name.clone(),
                port: port.name.clone(),
            });
        }
    }
    Ok(())
}

fn validate_image(image: &ImageIdentity) -> Result<(), ()> {
    let (prefix, digest) = match image {
        ImageIdentity::RegistryDigest { repository, digest } => {
            if !is_valid_registry_repository(repository) {
                return Err(());
            }
            ("sha256:", digest.as_str())
        }
        ImageIdentity::WorkerLocal { image_id, .. } => ("sha256:", image_id.as_str()),
    };
    let Some(value) = digest.strip_prefix(prefix) else {
        return Err(());
    };
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(());
    }
    Ok(())
}

/// Architecture token accepted identically for worker identities and workload
/// placement. Keeping this predicate in the wire crate prevents persisted
/// workloads from requesting a value that a worker hello could never report.
pub fn is_valid_architecture(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ARCHITECTURE_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Validate the repository portion of an immutable registry reference. This
/// deliberately excludes tags, digests, URL schemes, controls, and traversal
/// components; the digest travels in its own protocol field.
pub fn is_valid_registry_repository(value: &str) -> bool {
    if value.is_empty()
        || value.len() > MAX_REGISTRY_REPOSITORY_BYTES
        || !value.is_ascii()
        || value.contains('@')
    {
        return false;
    }

    let components = value.split('/').collect::<Vec<_>>();
    if components.iter().any(|component| component.is_empty()) {
        return false;
    }
    let path_start = usize::from(
        components.len() > 1
            && (components[0] == "localhost"
                || components[0].contains('.')
                || components[0].contains(':')
                || components[0].starts_with('[')),
    );
    if path_start == 1 && !valid_registry_domain(components[0]) {
        return false;
    }
    components[path_start..]
        .iter()
        .all(|component| valid_repository_component(component))
}

fn valid_registry_domain(value: &str) -> bool {
    if let Some(address) = value.strip_prefix('[') {
        let Some((address, suffix)) = address.split_once(']') else {
            return false;
        };
        return address.parse::<Ipv6Addr>().is_ok()
            && (suffix.is_empty() || valid_registry_port(suffix.strip_prefix(':')));
    }

    let (host, port) = value
        .rsplit_once(':')
        .map_or((value, None), |(host, port)| (host, Some(port)));
    if host.contains(':') || port.is_some_and(|port| !valid_registry_port(Some(port))) {
        return false;
    }
    !host.is_empty()
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
}

fn valid_registry_port(port: Option<&str>) -> bool {
    port.is_some_and(|port| {
        !port.is_empty()
            && port.bytes().all(|byte| byte.is_ascii_digit())
            && port.parse::<u16>().is_ok_and(|port| port != 0)
    })
}

fn valid_repository_component(value: &str) -> bool {
    let bytes = value.as_bytes();
    if !bytes
        .first()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return false;
    }
    let mut index = 0;
    while index < bytes.len() {
        while index < bytes.len()
            && (bytes[index].is_ascii_lowercase() || bytes[index].is_ascii_digit())
        {
            index += 1;
        }
        if index == bytes.len() {
            return true;
        }
        match bytes[index] {
            b'.' => index += 1,
            b'_' => {
                index += 1;
                if bytes.get(index) == Some(&b'_') {
                    index += 1;
                }
            }
            b'-' => {
                while bytes.get(index) == Some(&b'-') {
                    index += 1;
                }
            }
            _ => return false,
        }
        if !bytes
            .get(index)
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        {
            return false;
        }
    }
    true
}

fn valid_dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && value.as_bytes().first() != Some(&b'-')
        && value.as_bytes().last() != Some(&b'-')
}

fn valid_environment_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ENVIRONMENT_KEY_BYTES
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_spec(kind: GameKind) -> WorkloadSpec {
        WorkloadSpec {
            game_kind: kind,
            platform: Platform {
                operating_system: OperatingSystem::Linux,
                architecture: "amd64".to_string(),
                windows_build: None,
            },
            services: vec![ServiceSpec {
                name: "challenge".to_string(),
                image: ImageIdentity::RegistryDigest {
                    repository: "registry.example/ctf/challenge".to_string(),
                    digest: format!("sha256:{}", "a".repeat(64)),
                },
                resources: ResourceLimits {
                    cpu_millis: 500,
                    memory_bytes: 256 * 1024 * 1024,
                },
                replicas: 1,
                stateless: false,
                environment: BTreeMap::new(),
                ports: vec![ServicePort {
                    name: "service".to_string(),
                    container_port: 31337,
                    protocol: PortProtocol::Tcp,
                }],
            }],
            primary_endpoint: EndpointRef {
                service: "challenge".to_string(),
                port: "service".to_string(),
            },
            flag_target: Some(FlagTarget {
                service: "challenge".to_string(),
                path: "/flag".to_string(),
            }),
        }
    }

    #[test]
    fn accepts_stateless_jeopardy_replicas() {
        let mut spec = valid_spec(GameKind::Jeopardy);
        spec.services[0].replicas = 3;
        spec.services[0].stateless = true;
        assert!(ValidatedWorkloadSpec::try_from(spec).is_ok());
    }

    #[test]
    fn rejects_attack_defense_replicas() {
        let mut spec = valid_spec(GameKind::AttackDefense);
        spec.services[0].replicas = 2;
        spec.services[0].stateless = true;
        assert_eq!(
            ValidatedWorkloadSpec::try_from(spec).unwrap_err(),
            WorkloadValidationError::ReplicasNotAllowed
        );
    }

    #[test]
    fn rejects_multi_service_koth() {
        let mut spec = valid_spec(GameKind::KingOfTheHill);
        spec.services.push(spec.services[0].clone());
        spec.services[1].name = "database".to_string();
        assert_eq!(
            ValidatedWorkloadSpec::try_from(spec).unwrap_err(),
            WorkloadValidationError::CompetitiveModeShape
        );
    }

    #[test]
    fn deserialize_revalidates_spec() {
        let mut spec = valid_spec(GameKind::AttackDefense);
        spec.services[0].replicas = 2;
        let wire = serde_json::to_string(&spec).unwrap();
        assert!(serde_json::from_str::<ValidatedWorkloadSpec>(&wire).is_err());
    }

    #[test]
    fn worker_local_image_uses_camel_case_fields_on_wire() {
        let image = ImageIdentity::WorkerLocal {
            worker_id: Uuid::nil(),
            image_id: format!("sha256:{}", "b".repeat(64)),
        };
        let wire = serde_json::to_value(&image).unwrap();

        assert_eq!(
            wire,
            serde_json::json!({
                "type": "workerLocal",
                "workerId": Uuid::nil(),
                "imageId": format!("sha256:{}", "b".repeat(64)),
            })
        );
        assert_eq!(
            serde_json::from_value::<ImageIdentity>(wire).unwrap(),
            image
        );
    }

    #[test]
    fn repository_validation_accepts_runtime_safe_registry_names() {
        for repository in [
            "ubuntu",
            "library/ubuntu",
            "registry.example/ctf/challenge",
            "localhost:5000/ctf/challenge_image",
            "[2001:db8::1]:5000/ctf/challenge",
        ] {
            assert!(
                is_valid_registry_repository(repository),
                "safe repository {repository:?} was rejected"
            );
        }
    }

    #[test]
    fn repository_validation_rejects_ambiguous_or_unsafe_values_on_wire() {
        for repository in [
            "registry.example/ctf/app@sha256:deadbeef",
            "registry.example/ctf/app\0forged",
            "registry.example/ctf/app\nforged",
            "https://registry.example/ctf/app",
            "registry.example/ctf/../app",
            "registry.example/ctf/App",
            "registry.example/ctf/app:latest",
            "registry.example//ctf/app",
        ] {
            assert!(
                !is_valid_registry_repository(repository),
                "unsafe repository {repository:?} was accepted"
            );
            let mut wire = serde_json::to_value(valid_spec(GameKind::Jeopardy)).unwrap();
            wire["services"][0]["image"]["repository"] = serde_json::json!(repository);
            assert!(
                serde_json::from_value::<ValidatedWorkloadSpec>(wire).is_err(),
                "unsafe repository {repository:?} survived wire validation"
            );
        }
    }

    #[test]
    fn workload_architecture_uses_the_worker_identity_grammar() {
        for architecture in ["amd64", "arm64", "x86_64", "arm64.v8", "AMD64"] {
            assert!(is_valid_architecture(architecture));
            let mut spec = valid_spec(GameKind::Jeopardy);
            spec.platform.architecture = architecture.to_string();
            assert!(ValidatedWorkloadSpec::try_from(spec).is_ok());
        }

        let invalid = [
            String::new(),
            " amd64".into(),
            "amd64/variant".into(),
            "amd64\0forged".into(),
            "arm64\nforged".into(),
            "架構".into(),
            "a".repeat(MAX_ARCHITECTURE_BYTES + 1),
        ];
        for architecture in invalid {
            assert!(!is_valid_architecture(&architecture));
            let mut wire = serde_json::to_value(valid_spec(GameKind::Jeopardy)).unwrap();
            wire["platform"]["architecture"] = serde_json::json!(architecture);
            assert!(serde_json::from_value::<ValidatedWorkloadSpec>(wire).is_err());
        }
    }

    #[test]
    fn rejects_unknown_top_level_fields_during_deserialization() {
        let mut wire = serde_json::to_value(valid_spec(GameKind::Jeopardy)).unwrap();
        wire.as_object_mut()
            .unwrap()
            .insert("gameKnd".to_string(), serde_json::json!("jeopardy"));

        assert!(serde_json::from_value::<ValidatedWorkloadSpec>(wire).is_err());
    }

    #[test]
    fn rejects_unknown_nested_fields_during_deserialization() {
        let base = serde_json::to_value(valid_spec(GameKind::Jeopardy)).unwrap();
        for location in [
            "platform",
            "service",
            "image",
            "resources",
            "port",
            "endpoint",
            "flag",
        ] {
            let mut wire = base.clone();
            let object = match location {
                "platform" => wire["platform"].as_object_mut().unwrap(),
                "service" => wire["services"][0].as_object_mut().unwrap(),
                "image" => wire["services"][0]["image"].as_object_mut().unwrap(),
                "resources" => wire["services"][0]["resources"].as_object_mut().unwrap(),
                "port" => wire["services"][0]["ports"][0].as_object_mut().unwrap(),
                "endpoint" => wire["primaryEndpoint"].as_object_mut().unwrap(),
                "flag" => wire["flagTarget"].as_object_mut().unwrap(),
                _ => unreachable!(),
            };
            object.insert("cpuMilis".to_string(), serde_json::json!(500));

            assert!(
                serde_json::from_value::<ValidatedWorkloadSpec>(wire).is_err(),
                "nested typo under {location} was accepted"
            );
        }

        let mut worker_local = base;
        worker_local["services"][0]["image"] = serde_json::json!({
            "type": "workerLocal",
            "workerId": Uuid::nil(),
            "imageId": format!("sha256:{}", "b".repeat(64)),
            "repository": "unexpected"
        });
        assert!(serde_json::from_value::<ValidatedWorkloadSpec>(worker_local).is_err());
    }

    #[test]
    fn rejects_environment_keys_that_change_docker_parsing() {
        let mut spec = valid_spec(GameKind::Jeopardy);
        spec.services[0]
            .environment
            .insert("BAD=KEY".to_string(), "value".to_string());
        assert!(matches!(
            ValidatedWorkloadSpec::try_from(spec),
            Err(WorkloadValidationError::InvalidEnvironment { .. })
        ));
    }

    #[test]
    fn rejects_shapes_that_cannot_fit_status_or_command_frames() {
        let mut replicas = valid_spec(GameKind::Jeopardy);
        replicas.services[0].replicas = 64;
        replicas.services[0].stateless = true;
        for index in 1..9 {
            let mut service = replicas.services[0].clone();
            service.name = format!("service-{index}");
            service.ports[0].name = format!("port-{index}");
            service.ports[0].container_port += index as u16;
            replicas.services.push(service);
        }
        assert_eq!(
            ValidatedWorkloadSpec::try_from(replicas).unwrap_err(),
            WorkloadValidationError::TooManyReplicas
        );

        let mut oversized = valid_spec(GameKind::Jeopardy);
        for index in 0..13 {
            oversized.services[0].environment.insert(
                format!("VALUE_{index}"),
                "x".repeat(MAX_ENVIRONMENT_VALUE_BYTES),
            );
        }
        assert_eq!(
            ValidatedWorkloadSpec::try_from(oversized).unwrap_err(),
            WorkloadValidationError::SpecTooLarge
        );
    }
}
