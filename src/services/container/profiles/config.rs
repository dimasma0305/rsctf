use serde::{Deserialize, Serialize};

use crate::utils::error::{AppError, AppResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProfileBackend {
    Docker,
    Kubernetes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Isolation {
    Container,
    Microvm,
}

/// Administrator-owned deployment policy. Challenge authors select only its ID.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeProfile {
    pub id: String,
    pub name: String,
    pub backend: ProfileBackend,
    pub isolation: Isolation,
    /// Disabled profiles retain cleanup ownership but reject new instances.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_class: Option<String>,
}

fn enabled_by_default() -> bool {
    true
}

/// Checker admission must cover retained runtimes as well as new placement.
/// The checker firewall still grants only one exact target IP/port per process.
pub(crate) fn checker_service_cidrs(
    profiles: &[RuntimeProfile],
    docker: &str,
    kubernetes: Option<&str>,
) -> AppResult<Vec<String>> {
    let mut networks = std::collections::BTreeSet::new();
    for profile in profiles {
        let value = match profile.backend {
            ProfileBackend::Docker => docker,
            ProfileBackend::Kubernetes => kubernetes.ok_or_else(|| AppError::bad_request(
                "Kubernetes runtime profiles require RSCTF_K8S_AD_SERVICE_CIDR for checker target isolation",
            ))?,
        };
        let network = value.parse::<ipnet::Ipv4Net>().map_err(|_| {
            AppError::bad_request("Runtime profiles require valid IPv4 service CIDRs")
        })?;
        networks.insert(network.trunc().to_string());
    }
    Ok(networks.into_iter().collect())
}

impl RuntimeProfile {
    pub fn identity(&self) -> String {
        // Display names are editable; placement and isolation are immutable for
        // an existing handle. Changing either requires a new profile ID.
        crate::utils::codec::sha256_hex(
            format!(
                "{:?}\0{:?}\0{}",
                self.backend,
                self.isolation,
                self.runtime_class.as_deref().unwrap_or("")
            )
            .as_bytes(),
        )[..16]
            .to_string()
    }

    pub(crate) fn scope(&self, base: &str) -> String {
        // Shared by Docker and Kubernetes: label values must fit within 63 bytes.
        crate::utils::codec::sha256_str(&format!("{base}\0{}\0{}", self.id, self.identity()))[..32]
            .to_string()
    }
}

pub fn parse(value: &str) -> AppResult<Vec<RuntimeProfile>> {
    if value.len() > 16 * 1024 {
        return Err(AppError::bad_request(
            "runtime profile configuration exceeds 16 KiB",
        ));
    }
    let profiles: Vec<RuntimeProfile> = serde_json::from_str(value)
        .map_err(|error| AppError::bad_request(format!("invalid runtime profiles: {error}")))?;
    if profiles.len() > 16 {
        return Err(AppError::bad_request(
            "at most 16 runtime profiles are supported",
        ));
    }
    let mut ids = std::collections::HashSet::new();
    for profile in &profiles {
        if profile.id.is_empty()
            || profile.id.len() > 40
            || !profile
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || profile.id == "default"
            || !ids.insert(&profile.id)
            || profile.name.trim().is_empty()
            || profile.name.len() > 80
        {
            return Err(AppError::bad_request(
                "runtime profiles require unique lowercase IDs and bounded display names",
            ));
        }
        match (profile.backend, profile.isolation, profile.runtime_class.as_deref()) {
            (ProfileBackend::Docker, Isolation::Container, None)
            | (ProfileBackend::Kubernetes, Isolation::Container, None) => {}
            (ProfileBackend::Kubernetes, Isolation::Microvm, Some(class))
                if !class.is_empty() && class.len() <= 63
                    && class.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                    && !class.starts_with('-') && !class.ends_with('-') => {}
            _ => return Err(AppError::bad_request("microVM profiles require Kubernetes and an explicit Kata RuntimeClass; container profiles must not set runtimeClass")),
        }
    }
    Ok(profiles)
}

pub fn from_env() -> AppResult<Vec<RuntimeProfile>> {
    match std::env::var("RSCTF_RUNTIME_PROFILES") {
        Ok(value) => parse(&value),
        Err(std::env::VarError::NotPresent) => Ok(Vec::new()),
        Err(_) => Err(AppError::bad_request(
            "RSCTF_RUNTIME_PROFILES must be UTF-8 JSON",
        )),
    }
}

pub(crate) fn docker_scopes(base: &str) -> AppResult<Vec<String>> {
    let mut scopes = vec![base.to_owned()];
    scopes.extend(
        from_env()?
            .iter()
            .filter(|p| p.backend == ProfileBackend::Docker)
            .map(|p| p.scope(base)),
    );
    Ok(scopes)
}
