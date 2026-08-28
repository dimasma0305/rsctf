//! services/k8s.rs — Kubernetes [`ContainerManager`] backend.
//!
//! Port of RSCTF's `Services/Container/Manager/KubernetesManager.cs` (+ the
//! relevant slice of `Provider/KubernetesProvider.cs`) onto the Rust `kube`
//! (0.95) + `k8s-openapi` (0.23) crates. It schedules one **Pod + Service** per
//! challenge instance and maps the result back onto the runtime-agnostic
//! [`ContainerInfo`] / [`ContainerStatus`] types shared with the Docker backend.
//!
//! ## Lifecycle (mirrors `KubernetesManager`)
//!
//! 1. **connect** — [`KubernetesContainerManager::connect`] builds a
//!    [`kube::Client`] via [`kube::Client::try_default`], which honours
//!    `$KUBECONFIG` / `~/.kube/config` locally and the mounted service-account
//!    token when running in-cluster.
//! 2. **create** — build a single-container [`Pod`] from the [`ContainerSpec`]
//!    (image, `cpu`/`memory` limits, `RSCTF_FLAG` + caller env, the exposed
//!    `containerPort`, and an `app=rsctf-<uid>` label), create it in the
//!    configured namespace, then create a [`Service`] selecting the pod. Direct
//!    challenges use NodePort; PlatformProxy and A&D remain ClusterIP-only (not
//!    publicly node-published, but reachable by permitted cluster peers).
//!    On service failure the pod is deleted before its isolating policy so a
//!    half-created instance cannot become reachable during rollback.
//! 3. **destroy** — delete the Service, then the Pod, and only remove its
//!    NetworkPolicy after the Pod is absent; a `404` is the desired end state.
//! 4. **query** — get the Pod and map its `status.phase` to a coarse
//!    [`ContainerStatus`].
//!
//! A live cluster is NOT available in this environment, so this module is
//! compile-verified against the real crate APIs but cannot be exercised at
//! runtime; genuinely-unrunnable glue is marked `// TODO`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use k8s_openapi::api::core::v1::{
    Capabilities, Container, ContainerPort, EnvVar, Pod, PodSpec, ResourceRequirements,
    SeccompProfile, SecurityContext, Service, ServicePort, ServiceSpec,
};
use k8s_openapi::api::networking::v1::NetworkPolicy;
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::api::{Api, DeleteParams, PostParams};
use kube::core::{ApiResource, DynamicObject, ErrorResponse};
use kube::{Client, Config};

use crate::services::container::{
    ContainerBackendKind, ContainerExecAdmission, ContainerExecError, ContainerInfo,
    ContainerLiveness, ContainerManager, ContainerSpec, ContainerStatus,
};
use crate::utils::codec::random_hex;
use crate::utils::error::{AppError, AppResult};

mod exec;
mod metrics;
mod network;
mod orphans;
use metrics::{parse_cpu_cores, parse_memory_bytes};
use network::{
    ad_network_config, ad_network_policy, isolated_network_policy, isolated_proxy_network_policy,
    network_policy_required, proxy_network_policy, rollback_created_policy, service_ip_is_routed,
    validate_policy_enforcement_acknowledgement,
};
use orphans::APP_LABEL;

/// Flag variables injected into rsctf-managed challenge pods.
const FLAG_ENV: &str = "RSCTF_FLAG";
const FLAG_FILE_ENV: &str = "RSCTF_FLAG_FILE";
const FLAG_FILE_PATH: &str = "/flag";

/// Default namespace challenge pods are scheduled into when unset.
const DEFAULT_NAMESPACE: &str = "rsctf-challenges";

/// Env var overriding the target namespace.
const NAMESPACE_ENV: &str = "RSCTF_K8S_NAMESPACE";

/// Env var advertising the routable node/host IP for NodePort services
/// (RSCTF `PublicEntry`). When set, [`ContainerInfo::ip`] is this value.
const PUBLIC_ENTRY_ENV: &str = "RSCTF_K8S_PUBLIC_ENTRY";

/// Kubernetes-backed container manager.
///
/// Wraps a live [`kube::Client`] plus the namespace / public-entry configuration
/// read from the environment. The Rust equivalent of RSCTF's `KubernetesManager`
/// (which uses the C# `k8s` client).
#[derive(Clone)]
pub struct KubernetesContainerManager {
    /// Live Kubernetes API client.
    client: Client,
    /// Namespace all challenge pods/services are created in.
    namespace: String,
    /// Stable installation scope shared by split roles using the same control
    /// and challenge namespaces.
    scope: String,
    /// Advertised node/host IP for NodePort services (RSCTF `PublicEntry`).
    /// When `None`, [`create`](Self::create) falls back to the scheduled pod's
    /// `status.hostIP`.
    public_entry: Option<String>,
}

impl KubernetesContainerManager {
    /// Connect using the ambient kubeconfig / in-cluster service account
    /// (`kube::Client::try_default`), reading namespace + public entry from the
    /// environment.
    pub async fn connect() -> AppResult<Self> {
        validate_policy_enforcement_acknowledgement()?;
        let client = Client::try_default()
            .await
            .map_err(|e| AppError::internal(format!("failed to build kubernetes client: {e}")))?;
        Ok(Self::with_client(client))
    }

    /// Build a manager around an already-constructed client, pulling namespace
    /// and public-entry configuration from the environment.
    fn with_client(client: Client) -> Self {
        let namespace = std::env::var(NAMESPACE_ENV)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_NAMESPACE.to_string());
        let public_entry = std::env::var(PUBLIC_ENTRY_ENV)
            .ok()
            .filter(|s| !s.trim().is_empty());
        let control_namespace = network::configured_control_namespace();
        let scope = orphans::workload_scope(&namespace, control_namespace.as_deref());
        Self {
            client,
            namespace,
            scope,
            public_entry,
        }
    }

    /// Typed `Api` handle for pods in the configured namespace.
    fn pods(&self) -> Api<Pod> {
        Api::namespaced(self.client.clone(), &self.namespace)
    }

    /// Typed `Api` handle for services in the configured namespace.
    fn services(&self) -> Api<Service> {
        Api::namespaced(self.client.clone(), &self.namespace)
    }

    fn network_policies(&self) -> Api<NetworkPolicy> {
        Api::namespaced(self.client.clone(), &self.namespace)
    }

    /// Fetch live CPU / memory usage for a pod from the `metrics.k8s.io`
    /// aggregated API (served by metrics-server), summed across the pod's
    /// containers.
    ///
    /// `PodMetrics` is not a compiled-in `k8s-openapi` type, so it is queried as
    /// a dynamic resource (`GET
    /// /apis/metrics.k8s.io/v1beta1/namespaces/{ns}/pods/{name}`) and the
    /// `containers[].usage.{cpu,memory}` quantities are parsed out of the raw
    /// JSON. Returns `(memory_bytes, cpu_cores)`; either component is `None` when
    /// no container reported that metric. The whole call degrades to
    /// `(None, None)` on any transport / parse error (metrics-server absent,
    /// RBAC denied, metrics not yet scraped, …) so `query` never fails on it.
    async fn fetch_pod_metrics(&self, id: &str) -> (Option<u64>, Option<f64>) {
        // Type-erased handle onto the metrics.k8s.io PodMetrics resource. The
        // plural is `pods`, giving the standard `.../namespaces/{ns}/pods/{name}`
        // path that metrics-server serves.
        let ar = ApiResource {
            group: "metrics.k8s.io".to_string(),
            version: "v1beta1".to_string(),
            api_version: "metrics.k8s.io/v1beta1".to_string(),
            kind: "PodMetrics".to_string(),
            plural: "pods".to_string(),
        };
        let api: Api<DynamicObject> =
            Api::namespaced_with(self.client.clone(), &self.namespace, &ar);

        let obj = match api.get(id).await {
            Ok(obj) => obj,
            Err(_) => return (None, None),
        };

        // `DynamicObject::data` holds every top-level field other than
        // `metadata`; for PodMetrics that includes the `containers` array.
        let containers = match obj.data.get("containers").and_then(|c| c.as_array()) {
            Some(c) => c,
            None => return (None, None),
        };

        let mut total_mem: u64 = 0;
        let mut total_cpu: f64 = 0.0;
        let mut have_mem = false;
        let mut have_cpu = false;
        for c in containers {
            let usage = match c.get("usage") {
                Some(u) => u,
                None => continue,
            };
            if let Some(bytes) = usage
                .get("memory")
                .and_then(|v| v.as_str())
                .and_then(parse_memory_bytes)
            {
                total_mem = total_mem.saturating_add(bytes);
                have_mem = true;
            }
            if let Some(cores) = usage
                .get("cpu")
                .and_then(|v| v.as_str())
                .and_then(parse_cpu_cores)
            {
                total_cpu += cores;
                have_cpu = true;
            }
        }

        (have_mem.then_some(total_mem), have_cpu.then_some(total_cpu))
    }

    async fn rollback_new_pod(
        &self,
        pods: &Api<Pod>,
        name: &str,
        policy_created: bool,
        pod_adopted: bool,
    ) {
        if pod_adopted {
            return;
        }
        let policies = self.network_policies();
        let _ = orphans::rollback_created_pod(
            pods,
            &policies,
            name,
            &self.scope,
            rollback_created_policy(policy_created, pod_adopted),
        )
        .await;
    }
}

/// Whether a `kube` error is a Kubernetes `404 Not Found` (object already gone).
fn is_not_found(err: &kube::Error) -> bool {
    matches!(err, kube::Error::Api(ErrorResponse { code: 404, .. }))
}

fn is_conflict(err: &kube::Error) -> bool {
    matches!(err, kube::Error::Api(ErrorResponse { code: 409, .. }))
}

/// Map a pod `status.phase` string to a coarse lifecycle status, following the
/// same classification as the Docker backend's `map_status`.
fn map_phase(phase: Option<&str>) -> &'static str {
    match phase {
        Some("Pending") => "pending",
        Some("Running") => "running",
        Some("Succeeded") => "exited",
        Some("Failed") => "destroyed",
        Some("Unknown") => "pending",
        _ => "pending",
    }
}

fn phase_liveness(phase: Option<&str>) -> ContainerLiveness {
    match phase {
        Some("Running") => ContainerLiveness::Running,
        Some("Succeeded" | "Failed") => ContainerLiveness::Stopped,
        _ => ContainerLiveness::Unknown,
    }
}

/// Sanitize an image reference into an RFC1123-ish label fragment usable in a
/// resource name (lowercase alphanumerics + `-`, non-empty). Mirrors RSCTF's
/// `imageName.ToValidRFC1123String("chal")`.
fn sanitize_image(image: &str) -> String {
    // Take the last path segment, drop any tag/digest.
    let last = image.rsplit('/').next().unwrap_or(image);
    let base = last.split(':').next().unwrap_or(last);
    let cleaned: String = base
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "chal".to_string()
    } else {
        // RFC1123 label cap is 63 chars; leave room for the "-<suffix>" tail.
        trimmed.chars().take(40).collect()
    }
}

fn service_type(internal_only: bool) -> &'static str {
    if internal_only {
        "ClusterIP"
    } else {
        "NodePort"
    }
}

fn challenge_security_context() -> SecurityContext {
    let uid = std::env::var("RSCTF_K8S_CHALLENGE_UID")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(10_000);
    SecurityContext {
        allow_privilege_escalation: Some(false),
        capabilities: Some(Capabilities {
            // Challenge images commonly expose port 80. Preserve only the
            // narrow capability needed for a non-root process to bind it.
            add: Some(vec!["NET_BIND_SERVICE".to_string()]),
            drop: Some(vec!["ALL".to_string()]),
        }),
        privileged: Some(false),
        run_as_group: Some(uid),
        run_as_non_root: Some(true),
        run_as_user: Some(uid),
        seccomp_profile: Some(SeccompProfile {
            localhost_profile: None,
            type_: "RuntimeDefault".to_string(),
        }),
        ..Default::default()
    }
}

#[async_trait]
impl ContainerManager for KubernetesContainerManager {
    fn backend_kind(&self) -> ContainerBackendKind {
        ContainerBackendKind::Kubernetes
    }

    async fn storage_quota_enforced(&self) -> Option<bool> {
        Some(true)
    }

    async fn create(&self, spec: ContainerSpec) -> AppResult<ContainerInfo> {
        crate::services::container::validate_container_spec(&spec)?;
        if !crate::services::challenge_images::is_repository_digest(&spec.image) {
            return Err(AppError::bad_request(
                "Kubernetes challenge images must use a portable repository digest",
            ));
        }
        // Unique, DNS-safe resource name + the app label that ties the Service
        // to this pod (RSCTF uses a per-instance ResourceId label/selector).
        let uid = spec.operation_id.as_ref().map_or_else(
            || random_hex(8),
            |operation| crate::utils::codec::sha256_str(operation)[..16].to_string(),
        );
        let name = format!("{}-{}", sanitize_image(&spec.image), uid);
        let ad_internal = spec.ad_network.is_some();
        let isolated = spec.network_mode == crate::utils::enums::NetworkMode::Isolated;
        let internal_only = ad_internal || spec.proxy_only;
        let ad_config = if ad_internal {
            Some(ad_network_config()?)
        } else {
            None
        };
        if !spec.control_plane_callback_ports.is_empty()
            && ad_config
                .as_ref()
                .and_then(|config| config.control_namespace.as_ref())
                .is_none()
        {
            return Err(AppError::internal(
                "managed KotH reporting on Kubernetes requires RSCTF_K8S_CONTROL_NAMESPACE",
            ));
        }

        // Environment: caller-supplied vars plus the dynamic flag contract.
        let mut env: Vec<EnvVar> = spec
            .env
            .iter()
            .map(|(k, v)| EnvVar {
                name: k.clone(),
                value: Some(v.clone()),
                value_from: None,
            })
            .collect();
        if let Some(flag) = spec.flag.as_deref() {
            if !flag.is_empty() {
                env.push(EnvVar {
                    name: FLAG_ENV.to_string(),
                    value: Some(flag.to_string()),
                    value_from: None,
                });
                env.push(EnvVar {
                    name: FLAG_FILE_ENV.to_string(),
                    value: Some(FLAG_FILE_PATH.to_string()),
                    value_from: None,
                });
            }
        }

        // Resource limits: cpu_count whole cores -> `<n*100>m` (matching RSCTF's
        // `CPUCount * 100`), memory MB -> `<n>Mi`. Modest requests so the pod can
        // actually schedule.
        let mut limits = BTreeMap::new();
        limits.insert(
            "cpu".to_string(),
            Quantity(format!("{}m", spec.cpu_count * 100)),
        );
        limits.insert(
            "memory".to_string(),
            Quantity(format!("{}Mi", spec.memory_limit)),
        );
        limits.insert(
            "ephemeral-storage".to_string(),
            Quantity(format!("{}Mi", spec.storage_limit)),
        );
        let mut requests = BTreeMap::new();
        requests.insert("cpu".to_string(), Quantity("10m".to_string()));
        requests.insert("memory".to_string(), Quantity("32Mi".to_string()));
        requests.insert(
            "ephemeral-storage".to_string(),
            Quantity(format!("{}Mi", spec.storage_limit.min(32))),
        );

        let labels =
            orphans::workload_labels(&name, &uid, &self.scope, spec.operation_id.as_deref());
        let app_label = labels[APP_LABEL].clone();

        let container = Container {
            name: name.clone(),
            image: Some(spec.image.clone()),
            env: Some(env),
            ports: Some(vec![ContainerPort {
                container_port: spec.expose_port,
                ..Default::default()
            }]),
            resources: Some(ResourceRequirements {
                limits: Some(limits),
                requests: Some(requests),
                claims: None,
            }),
            security_context: Some(challenge_security_context()),
            ..Default::default()
        };

        let pod = Pod {
            metadata: ObjectMeta {
                name: Some(name.clone()),
                namespace: Some(self.namespace.clone()),
                labels: Some(labels.clone()),
                ..Default::default()
            },
            spec: Some(PodSpec {
                containers: vec![container],
                restart_policy: Some("Never".to_string()),
                automount_service_account_token: Some(false),
                ..Default::default()
            }),
            status: None,
        };

        // Install isolation before the selected pod exists, eliminating the
        // startup window in which an attacker-controlled image could reach the
        // cluster or Internet. The unique selector makes a crash-orphaned policy
        // harmless; normal destroy/rollback still removes it by name.
        let policies = self.network_policies();
        let has_network_policy = network_policy_required(ad_internal, spec.proxy_only, isolated);
        let private_policy = if let Some(config) = ad_config.as_ref() {
            Some(ad_network_policy(
                &name,
                &labels,
                None,
                spec.expose_port,
                spec.allow_egress,
                &spec.control_plane_callback_ports,
                config,
            ))
        } else if spec.proxy_only && isolated {
            Some(isolated_proxy_network_policy(
                &name,
                &labels,
                spec.expose_port,
            )?)
        } else if spec.proxy_only {
            Some(proxy_network_policy(&name, &labels, spec.expose_port)?)
        } else if isolated {
            Some(isolated_network_policy(&name, &labels, spec.expose_port)?)
        } else {
            None
        };
        debug_assert_eq!(private_policy.is_some(), has_network_policy);
        let mut policy_created = false;
        if let Some(policy) = private_policy {
            match policies.create(&PostParams::default(), &policy).await {
                Ok(_) => policy_created = true,
                Err(error) if is_conflict(&error) && spec.operation_id.is_some() => {
                    orphans::adopt(
                        &policies,
                        &name,
                        &self.scope,
                        spec.operation_id.as_deref(),
                        "network policy",
                    )
                    .await?;
                }
                Err(error) => {
                    return Err(AppError::internal(format!(
                        "failed to enforce private challenge NetworkPolicy: {error}"
                    )))
                }
            }
        }

        let pods = self.pods();
        let (created_pod, adopted) = match pods.create(&PostParams::default(), &pod).await {
            Ok(pod) => (pod, false),
            Err(error) if is_conflict(&error) && spec.operation_id.is_some() => {
                match orphans::adopt(
                    &pods,
                    &name,
                    &self.scope,
                    spec.operation_id.as_deref(),
                    "pod",
                )
                .await
                {
                    Ok(pod) => (pod, true),
                    Err(error) => {
                        // The conflicting Pod may match this policy's
                        // selector even when its ownership labels prevent
                        // adoption. Retain the policy; the scoped orphan
                        // reconciler removes it only after it can prove an
                        // owned Pod is absent.
                        return Err(error);
                    }
                }
            }
            Err(e) => {
                // A timed-out create can have reached the API server even
                // when the client saw an error. Removing isolation here
                // would expose that ambiguous Pod, so retain the policy for
                // adoption or fail-closed orphan reconciliation.
                return Err(AppError::internal(format!("failed to create pod: {e}")));
            }
        };
        let pod_uid = created_pod.metadata.uid.clone();

        // Service exposing the challenge port: A&D and PlatformProxy are not
        // node-published; direct challenges retain NodePort. The Service is
        // owned by the pod; private-policy cleanup applies to A&D and proxy-only
        // Jeopardy workloads.
        let mut owner_refs = None;
        if let Some(uid) = pod_uid {
            owner_refs = Some(vec![OwnerReference {
                api_version: "v1".to_string(),
                kind: "Pod".to_string(),
                name: name.clone(),
                uid,
                ..Default::default()
            }]);
        }

        let service = Service {
            metadata: ObjectMeta {
                name: Some(name.clone()),
                namespace: Some(self.namespace.clone()),
                labels: Some(labels.clone()),
                owner_references: owner_refs,
                ..Default::default()
            },
            spec: Some(ServiceSpec {
                type_: Some(service_type(internal_only).to_string()),
                // Preserve source addresses for isolated NodePorts. Without
                // Local routing, kube-proxy may SNAT a hostile Pod to a node
                // address that an ingress IPBlock legitimately allows.
                external_traffic_policy: (!internal_only && isolated).then(|| "Local".to_string()),
                selector: Some(BTreeMap::from([(APP_LABEL.to_string(), app_label.clone())])),
                ports: Some(vec![ServicePort {
                    port: spec.expose_port,
                    target_port: Some(IntOrString::Int(spec.expose_port)),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            status: None,
        };

        let services = self.services();
        let (created_svc, service_adopted) =
            match services.create(&PostParams::default(), &service).await {
                Ok(svc) => (svc, false),
                Err(error) if is_conflict(&error) && spec.operation_id.is_some() => {
                    match orphans::adopt(
                        &services,
                        &name,
                        &self.scope,
                        spec.operation_id.as_deref(),
                        "service",
                    )
                    .await
                    {
                        Ok(service) => (service, true),
                        Err(error) => {
                            self.rollback_new_pod(&pods, &name, policy_created, adopted)
                                .await;
                            return Err(error);
                        }
                    }
                }
                Err(e) => {
                    // Roll back the pod so a failed service create doesn't leak it.
                    self.rollback_new_pod(&pods, &name, policy_created, adopted)
                        .await;
                    return Err(AppError::internal(format!("failed to create service: {e}")));
                }
            };

        let (ip, port) = if internal_only {
            // A&D and PlatformProxy services use cluster-only addressing; never
            // allocate a node-wide externally scannable port for either mode.
            let cluster_ip = created_svc
                .spec
                .as_ref()
                .and_then(|s| s.cluster_ip.clone())
                .filter(|ip| !ip.is_empty() && ip != "None");
            let Some(cluster_ip) = cluster_ip else {
                if !service_adopted {
                    let _ = services.delete(&name, &DeleteParams::default()).await;
                }
                self.rollback_new_pod(&pods, &name, policy_created, adopted)
                    .await;
                return Err(AppError::internal(
                    "Kubernetes did not allocate a ClusterIP for the private service",
                ));
            };
            if let Some(service_cidr) = ad_config.as_ref().map(|config| &config.service_cidr) {
                if !service_ip_is_routed(&cluster_ip, service_cidr) {
                    if !service_adopted {
                        let _ = services.delete(&name, &DeleteParams::default()).await;
                    }
                    self.rollback_new_pod(&pods, &name, policy_created, adopted)
                        .await;
                    return Err(AppError::internal(format!(
                        "Kubernetes A&D ClusterIP {cluster_ip} is outside RSCTF_K8S_AD_SERVICE_CIDR {service_cidr}"
                    )));
                }
            }
            (cluster_ip, spec.expose_port)
        } else {
            // Direct challenge containers retain the externally reachable NodePort.
            let node_port = created_svc
                .spec
                .as_ref()
                .and_then(|s| s.ports.as_ref())
                .and_then(|ports| ports.first())
                .and_then(|p| p.node_port)
                .unwrap_or(spec.expose_port);
            let node_ip = self
                .public_entry
                .clone()
                .or_else(|| {
                    created_pod
                        .status
                        .as_ref()
                        .and_then(|st| st.host_ip.clone())
                        .filter(|h| !h.is_empty())
                })
                .unwrap_or_default();
            (node_ip, node_port)
        };

        let status = created_pod
            .status
            .as_ref()
            .and_then(|st| st.phase.as_deref());

        Ok(ContainerInfo {
            id: name,
            ip,
            port,
            status: map_phase(status).to_string(),
        })
    }

    async fn destroy(&self, id: &str) -> AppResult<()> {
        orphans::destroy_owned(
            self.pods(),
            self.services(),
            self.network_policies(),
            id,
            &self.scope,
        )
        .await
    }

    async fn list_managed(&self) -> Vec<String> {
        orphans::list_managed(
            self.pods(),
            self.services(),
            self.network_policies(),
            &self.scope,
        )
        .await
    }

    async fn query(&self, id: &str) -> AppResult<ContainerStatus> {
        let pod = self.pods().get(id).await.map_err(|e| {
            if is_not_found(&e) {
                AppError::not_found(format!("pod not found: {id}"))
            } else {
                AppError::internal(format!("failed to get pod: {e}"))
            }
        })?;

        let phase = pod.status.as_ref().and_then(|st| st.phase.as_deref());

        // Best-effort live usage from the metrics.k8s.io aggregated API
        // (metrics-server). RSCTF's `KubernetesManager` leaves these null; here
        // we opportunistically populate them and degrade to `None` on any error
        // so a missing/unreachable metrics-server never fails the query.
        let (memory_bytes, cpu_usage) = self.fetch_pod_metrics(id).await;

        Ok(ContainerStatus {
            id: id.to_string(),
            status: map_phase(phase).to_string(),
            memory_bytes,
            cpu_usage,
        })
    }

    async fn inspect_liveness(&self, id: &str) -> AppResult<ContainerLiveness> {
        match self.pods().get(id).await {
            Ok(pod) => Ok(phase_liveness(
                pod.status
                    .as_ref()
                    .and_then(|status| status.phase.as_deref()),
            )),
            Err(error) if is_not_found(&error) => Ok(ContainerLiveness::Stopped),
            Err(error) => Err(AppError::internal(format!(
                "failed to inspect pod liveness: {error}"
            ))),
        }
    }

    /// Execute directly in the platform-managed challenge container. Kubernetes
    /// multiplexes stdout, stderr, and process status over a websocket; drain
    /// both output streams concurrently so either one cannot back-pressure the
    /// other and deadlock the command.
    async fn exec(&self, id: &str, cmd: Vec<String>) -> AppResult<String> {
        exec::run(self.pods(), id, cmd).await
    }

    async fn exec_classified(
        &self,
        id: &str,
        cmd: Vec<String>,
        admission: ContainerExecAdmission,
    ) -> Result<String, ContainerExecError> {
        exec::run_classified(self.pods(), id, cmd, admission).await
    }
}

#[cfg(test)]
mod tests;

/// Select a Kubernetes container backend from the environment.
///
/// Returns `Some` when an ambient Kubernetes configuration (a kubeconfig file or
/// an in-cluster service account) is reachable, and `None` otherwise so the
/// caller can fall back to the Docker / Noop backend.
///
/// Config inference and client construction run on a dedicated thread with its
/// own current-thread runtime, so this is safe to call from a synchronous
/// startup path regardless of whether an outer Tokio runtime is already active
/// (avoids the "cannot start a runtime from within a runtime" panic — the same
/// pattern the Docker backend uses for reachability probing).
pub fn from_env() -> Option<Arc<dyn ContainerManager>> {
    let handle = std::thread::spawn(|| -> Option<KubernetesContainerManager> {
        if let Err(error) = validate_policy_enforcement_acknowledgement() {
            tracing::error!(%error, "Kubernetes isolation configuration is incomplete");
            return None;
        }
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        rt.block_on(async {
            // `Config::infer` succeeds only when a kubeconfig or in-cluster
            // service account is present; treat that as "K8s is available".
            let config = match tokio::time::timeout(Duration::from_secs(5), Config::infer()).await {
                Ok(Ok(cfg)) => cfg,
                _ => return None,
            };
            // `Client::try_from` merely constructs the client; it does not
            // round-trip to the apiserver. Confirm the apiserver actually
            // responds with a short version probe (`GET /version`) before
            // advertising K8s as available, so a stale/unreachable kubeconfig
            // makes the caller fall back to the Docker/Noop backend instead of
            // handing out a client whose every request will fail.
            let client = Client::try_from(config).ok()?;
            match tokio::time::timeout(Duration::from_secs(5), client.apiserver_version()).await {
                Ok(Ok(_)) => Some(KubernetesContainerManager::with_client(client)),
                _ => None,
            }
        })
    });

    let manager = handle.join().ok().flatten()?;
    Some(Arc::new(manager))
}
