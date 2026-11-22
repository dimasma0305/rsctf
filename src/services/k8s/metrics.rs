use kube::{
    core::{ApiResource, DynamicObject},
    Api,
};

impl super::KubernetesContainerManager {
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
    pub(super) async fn fetch_pod_metrics(&self, id: &str) -> (Option<u64>, Option<f64>) {
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
}

/// Parse a Kubernetes CPU quantity into cores.
pub(super) fn parse_cpu_cores(value: &str) -> Option<f64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(number) = value.strip_suffix('n') {
        number.trim().parse::<f64>().ok().map(|n| n / 1e9)
    } else if let Some(number) = value.strip_suffix('u') {
        number.trim().parse::<f64>().ok().map(|n| n / 1e6)
    } else if let Some(number) = value.strip_suffix('m') {
        number.trim().parse::<f64>().ok().map(|n| n / 1e3)
    } else {
        value.parse::<f64>().ok()
    }
}

/// Parse a Kubernetes memory quantity into bytes.
pub(super) fn parse_memory_bytes(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    const SUFFIXES: &[(&str, f64)] = &[
        ("Ki", 1024.0),
        ("Mi", 1_048_576.0),
        ("Gi", 1_073_741_824.0),
        ("Ti", 1_099_511_627_776.0),
        ("Pi", 1_125_899_906_842_624.0),
        ("Ei", 1_152_921_504_606_846_976.0),
        ("k", 1e3),
        ("M", 1e6),
        ("G", 1e9),
        ("T", 1e12),
        ("P", 1e15),
        ("E", 1e18),
    ];
    for (suffix, multiplier) in SUFFIXES {
        if let Some(number) = value.strip_suffix(suffix) {
            return number
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|number| number.is_finite() && *number >= 0.0)
                .map(|number| (number * multiplier) as u64);
        }
    }
    value
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite() && *number >= 0.0)
        .map(|number| number as u64)
}
