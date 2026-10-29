use k8s_openapi::api::node::v1::RuntimeClass;
use kube::Api;

use super::KubernetesContainerManager;
use crate::services::container::profiles::{Isolation, RuntimeProfile};
use crate::utils::error::{AppError, AppResult};

impl KubernetesContainerManager {
    pub(crate) async fn with_profile(mut self, profile: &RuntimeProfile) -> AppResult<Self> {
        self.scope = profile.scope(&self.scope);
        self.runtime_class = profile.runtime_class.clone();
        if profile.isolation == Isolation::Microvm && self.runtime_class.is_none() {
            return Err(AppError::bad_request(
                "microVM profile requires a Kata RuntimeClass",
            ));
        }
        self.validate_runtime_class().await?;
        Ok(self)
    }

    pub(super) fn profile_fingerprint(&self, fingerprint: String) -> String {
        match &self.runtime_class {
            None => fingerprint,
            Some(class) => crate::utils::codec::sha256_str(&format!("{fingerprint}\0kata:{class}")),
        }
    }

    pub(super) async fn validate_runtime_class(&self) -> AppResult<()> {
        let Some(name) = &self.runtime_class else {
            return Ok(());
        };
        let classes: Api<RuntimeClass> = Api::all(self.client.clone());
        let class = tokio::time::timeout(std::time::Duration::from_secs(5), classes.get(name))
            .await
            .map_err(|_| AppError::unavailable("Kata runtime validation timed out"))?
            .map_err(|_| {
                AppError::unavailable(format!(
                    "Kata RuntimeClass {name:?} is unavailable; no container fallback is allowed"
                ))
            })?;
        validate_class(&class)
    }
}

pub(super) fn validate_class(class: &RuntimeClass) -> AppResult<()> {
    if class.handler != "kata" && !class.handler.starts_with("kata-") {
        return Err(AppError::bad_request(
            "microVM profiles require an administrator-installed Kata handler",
        ));
    }
    let overhead = class.overhead.as_ref().and_then(|o| o.pod_fixed.as_ref());
    let memory = overhead
        .and_then(|o| o.get("memory"))
        .and_then(|q| super::metrics::parse_memory_bytes(&q.0));
    if memory.is_none_or(|bytes| bytes == 0) {
        return Err(AppError::bad_request("Kata RuntimeClass must declare positive memory overhead so VM memory is included in scheduling"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microvm_class_rejects_container_handlers_and_missing_vm_overhead() {
        for value in [
            serde_json::json!({"handler":"runc", "overhead":{"podFixed":{"memory":"256Mi"}}}),
            serde_json::json!({"handler":"kata-qemu"}),
            serde_json::json!({"handler":"kata-qemu", "overhead":{"podFixed":{"memory":"0"}}}),
            serde_json::json!({"handler":"kata-qemu", "overhead":{"podFixed":{"memory":"nonsense"}}}),
        ] {
            let class: RuntimeClass = serde_json::from_value(value).unwrap();
            assert!(validate_class(&class).is_err());
        }
        let class: RuntimeClass = serde_json::from_value(serde_json::json!({
            "handler":"kata-qemu", "overhead":{"podFixed":{"memory":"256Mi", "cpu":"100m"}}
        }))
        .unwrap();
        validate_class(&class).unwrap();
    }
}
