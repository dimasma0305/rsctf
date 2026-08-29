#!/usr/bin/env bash
set -Eeuo pipefail

for command_name in kind kubectl; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "missing required command: $command_name" >&2
    exit 1
  fi
done

cluster_name="rsctf-koth-callback-${RANDOM}"
node_image='kindest/node:v1.36.1@sha256:3489c7674813ba5d8b1a9977baea8a6e553784dab7b84759d1014dbd78f7ebd5'
policy_directory=''

cleanup() {
  kind delete cluster --name "$cluster_name" >/dev/null 2>&1 || true
  if [[ -n "$policy_directory" && -d "$policy_directory" && ! -L "$policy_directory" ]]; then
    rm -r -- "$policy_directory"
  fi
}
trap cleanup EXIT

kind create cluster \
  --name "$cluster_name" \
  --image "$node_image" \
  --wait 120s

kubectl create namespace rsctf-system
kubectl create namespace rsctf-challenges
kubectl create namespace rsctf-retry
kubectl create namespace rsctf-rejection

operation_id='rsctf-live-legacy-operation'
operation_hash="$(printf '%s' "$operation_id" | sha256sum | cut -c1-32)"
legacy_uid="$(printf '%s' "$operation_id" | sha256sum | cut -c1-16)"
legacy_name="agnhost-sha256-${legacy_uid}"
retry_scope="$(printf '%s\0%s' 'rsctf-retry' 'rsctf-retry' | sha256sum | cut -c1-32)"
retry_image='registry.k8s.io/e2e-test-images/agnhost@sha256:99c6b4bb4a1e1df3f0b3752168c89358794d02258ebebc26bf21c29399011a85'

kubectl apply -f - <<YAML
apiVersion: v1
kind: Pod
metadata:
  name: ${legacy_name}
  namespace: rsctf-retry
  labels:
    app: rsctf-${legacy_uid}
    rsctf.managed: "true"
    rsctf.container: ${legacy_name}
    rsctf.scope: ${retry_scope}
    rsctf.operation: ${operation_hash}
spec:
  automountServiceAccountToken: false
  restartPolicy: Never
  containers:
    - name: ${legacy_name}
      image: ${retry_image}
      ports:
        - containerPort: 8080
      resources:
        limits:
          cpu: 100m
          memory: 64Mi
          ephemeral-storage: 512Mi
        requests:
          cpu: 10m
          memory: 32Mi
          ephemeral-storage: 32Mi
      securityContext:
        allowPrivilegeEscalation: false
        capabilities:
          add: [NET_BIND_SERVICE]
          drop: [ALL]
        privileged: false
        runAsGroup: 10000
        runAsNonRoot: true
        runAsUser: 10000
        seccompProfile:
          type: RuntimeDefault
YAML
legacy_pod_uid="$(kubectl get pod --namespace rsctf-retry "$legacy_name" -o jsonpath='{.metadata.uid}')"
kubectl apply -f - <<YAML
apiVersion: v1
kind: Service
metadata:
  name: ${legacy_name}
  namespace: rsctf-retry
  labels:
    app: rsctf-${legacy_uid}
    rsctf.managed: "true"
    rsctf.container: ${legacy_name}
    rsctf.scope: ${retry_scope}
    rsctf.operation: ${operation_hash}
  ownerReferences:
    - apiVersion: v1
      kind: Pod
      name: ${legacy_name}
      uid: ${legacy_pod_uid}
spec:
  type: NodePort
  selector:
    app: rsctf-${legacy_uid}
  ports:
    - port: 8080
      targetPort: 8080
YAML
kubectl apply --namespace rsctf-rejection -f - <<'YAML'
apiVersion: v1
kind: ResourceQuota
metadata:
  name: reject-services
spec:
  hard:
    count/services: "0"
YAML

RSCTF_K8S_NAMESPACE='rsctf-retry' \
RSCTF_K8S_PUBLIC_ENTRY='192.0.2.10' \
RSCTF_K8S_NETWORK_POLICY_ENFORCED='true' \
RSCTF_K8S_REJECTION_NAMESPACE='rsctf-rejection' \
  cargo test --locked --lib \
    services::k8s::retry_tests::real_kubernetes_legacy_retry_and_authoritative_rollback \
    -- --ignored --exact

kubectl apply -f - <<'YAML'
apiVersion: v1
kind: Pod
metadata:
  name: reporter
  namespace: rsctf-system
  labels:
    app.kubernetes.io/name: rsctf
    app.kubernetes.io/instance: rsctf-network
    app.kubernetes.io/component: network
spec:
  containers:
    - name: http
      image: registry.k8s.io/e2e-test-images/agnhost:2.53@sha256:99c6b4bb4a1e1df3f0b3752168c89358794d02258ebebc26bf21c29399011a85
      args: ["netexec", "--http-port=8080"]
      ports:
        - name: http
          containerPort: 8080
---
apiVersion: v1
kind: Service
metadata:
  name: rsctf-network
  namespace: rsctf-system
spec:
  selector:
    app.kubernetes.io/name: rsctf
    app.kubernetes.io/instance: rsctf-network
    app.kubernetes.io/component: network
  ports:
    - name: http
      port: 8080
      targetPort: http
---
apiVersion: v1
kind: Pod
metadata:
  name: unrelated
  namespace: rsctf-system
  labels:
    app.kubernetes.io/name: rsctf
    app.kubernetes.io/instance: rsctf-web
    app.kubernetes.io/component: web
spec:
  containers:
    - name: http
      image: registry.k8s.io/e2e-test-images/agnhost:2.53@sha256:99c6b4bb4a1e1df3f0b3752168c89358794d02258ebebc26bf21c29399011a85
      args: ["netexec", "--http-port=8080"]
      ports:
        - name: http
          containerPort: 8080
---
apiVersion: v1
kind: Service
metadata:
  name: rsctf-web
  namespace: rsctf-system
spec:
  selector:
    app.kubernetes.io/name: rsctf
    app.kubernetes.io/instance: rsctf-web
    app.kubernetes.io/component: web
  ports:
    - name: http
      port: 8080
      targetPort: http
---
apiVersion: v1
kind: Pod
metadata:
  name: callback-client
  namespace: rsctf-challenges
  labels:
    app: rsctf-koth-callback-test
spec:
  containers:
    - name: curl
      image: curlimages/curl:8.16.0@sha256:463eaf6072688fe96ac64fa623fe73e1dbe25d8ad6c34404a669ad3ce1f104b6
      command: ["sleep", "3600"]
YAML

kubectl wait --namespace rsctf-system \
  --for=condition=Ready pod/reporter pod/unrelated --timeout=120s
kubectl wait --namespace rsctf-challenges \
  --for=condition=Ready pod/callback-client --timeout=120s

# The dollar-prefixed fields belong to awk, not this shell.
# shellcheck disable=SC2016
dns_server="$(kubectl exec --namespace rsctf-challenges callback-client -- \
  awk '$1 == "nameserver" { print $2; exit }' /etc/resolv.conf)"
case "$dns_server" in
  *:*) dns_cidr="${dns_server}/128" ;;
  *.*) dns_cidr="${dns_server}/32" ;;
  *)
    echo 'challenge Pod did not expose a usable cluster DNS resolver' >&2
    exit 1
    ;;
esac

policy_directory="$(mktemp -d /tmp/rsctf-koth-policy.XXXXXX)"
emit_policy() {
  local output=$1
  local operation_id=$2
  RSCTF_K8S_AD_SERVICE_CIDR='10.96.0.0/12' \
  RSCTF_K8S_CONTROL_NAMESPACE='rsctf-system' \
  RSCTF_K8S_KOTH_REPORTER_POD_SELECTOR='app.kubernetes.io/name=rsctf,app.kubernetes.io/instance=rsctf-network,app.kubernetes.io/component=network' \
  RSCTF_K8S_DNS_CIDRS="$dns_cidr" \
  RSCTF_K8S_POLICY_OUTPUT="$output" \
  RSCTF_K8S_POLICY_OPERATION_ID="$operation_id" \
    cargo test --locked --lib \
      services::k8s::tests::emit_managed_koth_callback_policy_for_live_test \
      -- --ignored --exact
}

route_a_policy="${policy_directory}/route-a-original.json"
route_b_policy="${policy_directory}/route-b.json"
policy_file="${policy_directory}/route-a-restored.json"
emit_policy "$route_a_policy" \
  'koth-cycle:41:attempt:3:managed-reporter-v2:0123456789abcdef:00112233445566778899aabbccddeeff'
emit_policy "$route_b_policy" \
  'koth-cycle:41:attempt:3:managed-reporter-v2:fedcba9876543210:112233445566778899aabbccddeeff00'
emit_policy "$policy_file" \
  'koth-cycle:41:attempt:3:managed-reporter-v2:0123456789abcdef:2233445566778899aabbccddeeff0011'

mapfile -t orphan_names < <(
  jq -r '.metadata.name' "$route_a_policy" "$route_b_policy" "$policy_file" | sort -u
)
if [[ "${#orphan_names[@]}" -ne 3 ]]; then
  echo 'A-to-B-to-A credential rotation reused a Kubernetes workload identity' >&2
  exit 1
fi

# Leave the first two policies as crash orphans. The restored route must create
# a third resource rather than adopting the original route-A policy.
kubectl apply --namespace rsctf-challenges -f "$route_a_policy"
kubectl apply --namespace rsctf-challenges -f "$route_b_policy"

jq --exit-status \
  --arg dns_cidr "$dns_cidr" \
  '.spec.egress[0].to[0].podSelector.matchLabels == {
      "app.kubernetes.io/name": "rsctf",
      "app.kubernetes.io/instance": "rsctf-network",
      "app.kubernetes.io/component": "network"
    }
    and (.spec.egress[0].ports | any(.protocol == "TCP" and .port == 8080))
    and (.spec.egress[1].to | any(.ipBlock.cidr == $dns_cidr))' \
  "$policy_file" >/dev/null

mapfile -t policy_labels < <(
  jq -r '.spec.podSelector.matchLabels | to_entries[] | "\(.key)=\(.value)"' \
    "$policy_file"
)
for label in "${policy_labels[@]}"; do
  kubectl label --namespace rsctf-challenges pod/callback-client \
    --overwrite "$label" >/dev/null
done

callback_url='http://rsctf-network.rsctf-system.svc:8080/echo?msg=callback-ok'
unrelated_url='http://rsctf-web.rsctf-system.svc:8080/echo?msg=unexpected'

preflight_ready=0
for _ in $(seq 1 30); do
  if callback_response="$(kubectl exec --namespace rsctf-challenges callback-client -- \
    curl --fail --silent --show-error --connect-timeout 2 --max-time 5 "$callback_url")" \
    && grep -Fq 'callback-ok' <<<"$callback_response" \
    && kubectl exec --namespace rsctf-challenges callback-client -- \
      curl --fail --silent --connect-timeout 2 --max-time 5 \
      "$unrelated_url" >/dev/null 2>&1; then
    preflight_ready=1
    break
  fi
  sleep 1
done

if [[ "$preflight_ready" != 1 ]]; then
  echo 'callback Services did not both become reachable before policy enforcement' >&2
  kubectl get pods,services,endpointslices --all-namespaces -o wide >&2
  exit 1
fi

if kubectl exec --namespace rsctf-challenges callback-client -- \
  curl --fail --silent --connect-timeout 2 --max-time 5 \
  'http://rsctf-network:8080/echo?msg=wrong-namespace' >/dev/null 2>&1; then
  echo 'bare callback Service unexpectedly resolved from the challenge namespace' >&2
  exit 1
fi

kubectl apply --namespace rsctf-challenges -f "$policy_file"

policy_enforced=0
for _ in $(seq 1 30); do
  if callback_response="$(kubectl exec --namespace rsctf-challenges callback-client -- \
    curl --fail --silent --show-error --connect-timeout 2 --max-time 5 "$callback_url")" \
    && grep -Fq 'callback-ok' <<<"$callback_response" \
    && ! kubectl exec --namespace rsctf-challenges callback-client -- \
      curl --fail --silent --connect-timeout 1 --max-time 2 \
      "$unrelated_url" >/dev/null 2>&1; then
    policy_enforced=1
    break
  fi
  sleep 1
done

if [[ "$policy_enforced" != 1 ]]; then
  echo 'NetworkPolicy did not preserve the exact callback while blocking unrelated rsctf Pods' >&2
  kubectl get pods,services,networkpolicies --all-namespaces -o wide >&2
  exit 1
fi

echo 'Kubernetes managed KotH callback DNS and egress isolation passed.'
