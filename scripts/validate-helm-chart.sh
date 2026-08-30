#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

fail() {
  echo "::error::$1" >&2
  exit 1
}

assert_contains() {
  local rendered="$1"
  local expected="$2"
  local label="$3"
  grep -Fq -- "$expected" <<<"$rendered" || fail "$label"
}

assert_absent() {
  local rendered="$1"
  local forbidden="$2"
  local label="$3"
  if grep -Fq -- "$forbidden" <<<"$rendered"; then
    fail "$label"
  fi
}

assert_pool_floor() {
  local label=$1 role=$2 floor=$3
  shift 3
  helm template "pool-${label}" charts/rsctf "$@" \
    --set "runtimeRole=${role}" \
    --set "config.dbMaxConnections=${floor}" >/dev/null \
    || fail "${label} role rejected its exact database pool floor ${floor}"
  if helm template "pool-${label}" charts/rsctf "$@" \
    --set "runtimeRole=${role}" \
    --set "config.dbMaxConnections=$((floor - 1))" >/dev/null 2>&1; then
    fail "${label} role accepted a database pool below its exact floor ${floor}"
  fi
}

jwt=(
  --set-string secrets.jwtSecret=0123456789abcdef0123456789abcdef
  --set-string secrets.identityHashKey=fedcba9876543210fedcba9876543210
)

helm lint charts/rsctf --strict "${jwt[@]}"

identity_rendered="$(helm template rsctf charts/rsctf "${jwt[@]}" \
  --show-only templates/deployment.yaml \
  --show-only templates/secret.yaml)"
assert_contains "$identity_rendered" 'name: RSCTF_IDENTITY_HASH_KEY' \
  "runtime Pod is missing the stable identity hash key"
assert_contains "$identity_rendered" 'key: identity-hash-key' \
  "runtime Pod does not use the configured identity hash Secret key"
assert_contains "$identity_rendered" '"identity-hash-key": "fedcba9876543210fedcba9876543210"' \
  "chart-managed Secret is missing the identity hash key"

if helm template rsctf charts/rsctf \
  --set-string secrets.jwtSecret=0123456789abcdef0123456789abcdef >/dev/null 2>&1; then
  fail "chart accepted a missing identity hash key"
fi
if helm template rsctf charts/rsctf \
  --set-string secrets.jwtSecret=0123456789abcdef0123456789abcdef \
  --set-string secrets.identityHashKey=short >/dev/null 2>&1; then
  fail "chart accepted a short identity hash key"
fi
if helm template rsctf charts/rsctf \
  --set-string secrets.jwtSecret=0123456789abcdef0123456789abcdef \
  --set-string secrets.identityHashKey=0123456789abcdef0123456789abcdef >/dev/null 2>&1; then
  fail "chart accepted the JWT secret as the identity hash key"
fi

default_config="$(helm template rsctf charts/rsctf "${jwt[@]}" \
  --show-only templates/configmap.yaml)"
assert_contains "$default_config" 'RSCTF_AD_SUBMIT_BURST_FLAGS: "400"' \
  "default A&D submit burst was not rendered"
assert_contains "$default_config" 'RSCTF_KOTH_CAPABILITY_IP_ADMISSION_PER_MINUTE: "30000"' \
  "default managed KotH capability admission was not rendered"
assert_contains "$default_config" 'RSCTF_DB_MAX_CONNECTIONS: "34"' \
  "default pool does not cover the all+VPN reconciler floor"
benchmark_config="$(helm template rsctf charts/rsctf "${jwt[@]}" \
  --set config.adSubmitBurstFlags=3200 \
  --show-only templates/configmap.yaml)"
assert_contains "$benchmark_config" 'RSCTF_AD_SUBMIT_BURST_FLAGS: "3200"' \
  "explicit A&D submit burst was not rendered"
abuse_probe_config="$(helm template rsctf charts/rsctf "${jwt[@]}" \
  --set config.kothCapabilityIpAdmissionPerMinute=3000 \
  --show-only templates/configmap.yaml)"
assert_contains "$abuse_probe_config" 'RSCTF_KOTH_CAPABILITY_IP_ADMISSION_PER_MINUTE: "3000"' \
  "explicit managed KotH capability admission was not rendered"
managed_koth_config="$(helm template rsctf charts/rsctf "${jwt[@]}" \
  --namespace rsctf-system \
  --set containerBackend=kubernetes \
  --set kubernetes.adServiceCidr=10.96.0.0/12 \
  --set 'kubernetes.dnsCidrs[0]=169.254.20.10/32' \
  --set kubernetes.networkPolicyEnforced=true \
  --set config.kothReporterBaseUrl=http://rsctf.rsctf-system.svc:8080 \
  --show-only templates/configmap.yaml)"
assert_contains "$managed_koth_config" 'RSCTF_KOTH_REPORTER_BASE_URL: "http://rsctf.rsctf-system.svc:8080"' \
  "managed KotH reporter origin was not rendered"
assert_contains "$managed_koth_config" 'RSCTF_K8S_KOTH_REPORTER_POD_SELECTOR: "app.kubernetes.io/name=rsctf,app.kubernetes.io/instance=rsctf,app.kubernetes.io/component=all"' \
  "monolithic managed KotH callback did not select its exact Service pods"
assert_contains "$managed_koth_config" 'RSCTF_K8S_DNS_CIDRS: "169.254.20.10/32"' \
  "managed KotH callback did not render the configured cluster resolver"
for invalid_burst in 99 3201; do
  if helm template rsctf charts/rsctf "${jwt[@]}" \
    --set config.adSubmitBurstFlags="$invalid_burst" >/dev/null 2>&1; then
    fail "chart accepted out-of-range A&D submit burst $invalid_burst"
  fi
done
for invalid_admission in 2999 1000001; do
  if helm template rsctf charts/rsctf "${jwt[@]}" \
    --set config.kothCapabilityIpAdmissionPerMinute="$invalid_admission" >/dev/null 2>&1; then
    fail "chart accepted out-of-range managed KotH capability admission $invalid_admission"
  fi
done

rbac="$(helm template rsctf charts/rsctf \
  --show-only templates/rbac.yaml \
  --set containerBackend=kubernetes \
  --set kubernetes.adServiceCidr=10.96.0.0/12 \
  --set kubernetes.networkPolicyEnforced=true \
  "${jwt[@]}")"
grep -A1 -F 'resources: ["pods/exec"]' <<<"$rbac" \
  | grep -Fq 'verbs: ["create"]' \
  || fail "Kubernetes exec RBAC does not grant create on pods/exec"

worker=(
  "${jwt[@]}"
  --set containerBackend=worker
  --set trafficCapture.enabled=false
  --set workerPlane.enabled=true
  --set workerPlane.existingSecret.name=rsctf-worker-tls
  --set workerPlane.publicEndpoint=workers.ctf.example:9443
  --set workerPlane.serverName=workers.ctf.example
)

listener="$(helm template rsctf charts/rsctf "${worker[@]}" \
  --show-only templates/deployment.yaml \
  --show-only templates/service.yaml)"
assert_contains "$listener" 'name: RSCTF_WORKER_LISTEN' \
  "worker listener environment is missing"
assert_contains "$listener" 'secretName: "rsctf-worker-tls"' \
  "worker listener TLS Secret is missing"
assert_contains "$listener" 'name: rsctf-workers' \
  "worker listener Service is missing"
assert_contains "$listener" 'name: worker-tls' \
  "worker listener TLS volume is missing"

if helm template rsctf charts/rsctf "${worker[@]}" \
  --set workerBackend.defaultOs=windows >/dev/null 2>&1; then
  fail "chart accepted the unsupported Windows worker default"
fi

web=(
  --set runtimeRole=web
  --set replicaCount=2
  --set-string image.tag=1.2.3
  --set postgresql.enabled=false
  --set redis.enabled=false
  --set existingSecret.name=rsctf-shared
  --set persistence.enabled=true
  --set persistence.existingClaim=rsctf-files-rwx
  --set 'persistence.accessModes[0]=ReadWriteMany'
  --set containerBackend=worker
  --set workerBackend.localBackend=none
  --set trafficCapture.enabled=false
  --set config.dbMaxConnections=26
)
web_rendered="$(helm template rsctf-web charts/rsctf "${web[@]}")"
if helm template rsctf-web charts/rsctf "${web[@]}" \
  --set config.dbMaxConnections=25 >/dev/null 2>&1; then
  fail "web role accepted a database pool below its replica-safe floor"
fi

split_pool=(
  --set replicaCount=1
  --set-string image.tag=1.2.3
  --set postgresql.enabled=false
  --set redis.enabled=false
  --set existingSecret.name=rsctf-shared
  --set persistence.enabled=true
  --set persistence.existingClaim=rsctf-files-rwx
  --set 'persistence.accessModes[0]=ReadWriteMany'
  --set containerBackend=none
  --set workerBackend.localBackend=none
  --set trafficCapture.enabled=false
)
assert_pool_floor engine engine 16 "${split_pool[@]}"
assert_pool_floor control control 19 "${split_pool[@]}"
assert_pool_floor network network 17 "${split_pool[@]}"
assert_pool_floor all all 31 "${jwt[@]}" --set vpn.enabled=false

vpn_pool=(
  "${split_pool[@]}"
  --set containerBackend=docker
  --set docker.socket.enabled=true
  --set vpn.enabled=true
  --set vpn.serverEndpoint=vpn.ctf.example:51820
)
assert_pool_floor control-vpn control 22 "${vpn_pool[@]}"
assert_pool_floor network-vpn network 20 "${vpn_pool[@]}"
assert_pool_floor all-vpn all 34 "${jwt[@]}" \
  --set containerBackend=docker \
  --set docker.socket.enabled=true \
  --set vpn.enabled=true \
  --set vpn.serverEndpoint=vpn.ctf.example:51820
assert_absent "$web_rendered" 'RSCTF_WORKER_LISTEN' \
  "web role received the singleton worker listener"
assert_absent "$web_rendered" 'worker-ca.key' \
  "web role received the worker CA key"
assert_absent "$web_rendered" 'name: docker-socket' \
  "web role received the Docker socket"
assert_absent "$web_rendered" '- NET_RAW' \
  "web role received NET_RAW while capture is disabled"
assert_contains "$web_rendered" 'app.kubernetes.io/component: "web"' \
  "runtime Deployment metadata does not identify its cutover component"

if helm template rsctf-web charts/rsctf "${web[@]}" \
  --set workerPlane.enabled=true \
  --set workerPlane.existingSecret.name=rsctf-worker-tls \
  --set workerPlane.publicEndpoint=workers.ctf.example:9443 \
  --set workerPlane.serverName=workers.ctf.example >/dev/null 2>&1; then
  fail "web role accepted the singleton worker listener and CA key"
fi

if helm template rsctf-web charts/rsctf "${web[@]}" \
  --set workerBackend.localBackend=docker \
  --set docker.socket.enabled=true >/dev/null 2>&1; then
  fail "web role accepted a hybrid local backend"
fi

if helm template rsctf-control charts/rsctf "${web[@]}" \
  --set runtimeRole=control \
  --set replicaCount=1 \
  --set workerBackend.localBackend=docker \
  --set docker.socket.enabled=true \
  --set workerPlane.enabled=true \
  --set workerPlane.existingSecret.name=rsctf-worker-tls \
  --set workerPlane.publicEndpoint=workers.ctf.example:9443 \
  --set workerPlane.serverName=workers.ctf.example \
  --set config.dbMaxConnections=22 >/dev/null 2>&1; then
  fail "split control role accepted a hybrid local backend"
fi

pure="$(helm template rsctf charts/rsctf "${worker[@]}" \
  --set workerBackend.localBackend=none \
  --set trafficCapture.enabled=false)"
assert_contains "$pure" 'RSCTF_WORKER_LOCAL_BACKEND: "none"' \
  "pure worker mode did not select the none local backend"
assert_absent "$pure" 'name: docker-socket' \
  "pure worker mode received the Docker socket"
assert_absent "$pure" 'kind: Role' \
  "pure worker mode received Kubernetes runtime RBAC"
assert_absent "$pure" '- NET_RAW' \
  "pure worker mode received NET_RAW while capture is disabled"

docker_hybrid="$(helm template rsctf charts/rsctf "${worker[@]}" \
  --set workerBackend.localBackend=docker \
  --set docker.socket.enabled=true \
  --set trafficCapture.enabled=true)"
assert_contains "$docker_hybrid" 'RSCTF_WORKER_LOCAL_BACKEND: "docker"' \
  "Docker hybrid did not select its local backend"
assert_contains "$docker_hybrid" 'runAsUser: 0' \
  "Docker hybrid did not run with Docker-socket ownership"
assert_contains "$docker_hybrid" 'name: docker-socket' \
  "Docker hybrid did not mount the Docker socket"
assert_contains "$docker_hybrid" '- NET_RAW' \
  "capture-enabled Docker hybrid did not receive NET_RAW"

vpn_owner="$(helm template rsctf charts/rsctf "${jwt[@]}" \
  --set containerBackend=kubernetes \
  --set kubernetes.adServiceCidr=10.96.0.0/12 \
  --set kubernetes.networkPolicyEnforced=true \
  --set vpn.enabled=true \
  --set vpn.serverEndpoint=vpn.ctf.example:51820)"
assert_contains "$vpn_owner" '- NET_ADMIN' \
  "VPN owner did not receive NET_ADMIN"
assert_contains "$vpn_owner" '- NET_RAW' \
  "VPN owner did not receive NET_RAW for the iptables ipset matcher"

kubernetes_hybrid="$(helm template rsctf charts/rsctf "${worker[@]}" \
  --set workerBackend.localBackend=kubernetes \
  --set kubernetes.challengeNamespace=rsctf-challenges \
  --set kubernetes.adServiceCidr=10.96.0.0/12 \
  --set kubernetes.networkPolicyEnforced=true)"
assert_contains "$kubernetes_hybrid" 'RSCTF_WORKER_LOCAL_BACKEND: "kubernetes"' \
  "Kubernetes hybrid did not select its local backend"
assert_contains "$kubernetes_hybrid" 'automountServiceAccountToken: true' \
  "Kubernetes hybrid did not mount its ServiceAccount token"
assert_contains "$kubernetes_hybrid" 'kind: Role' \
  "Kubernetes hybrid did not render runtime RBAC"
assert_contains "$kubernetes_hybrid" 'namespace: rsctf-challenges' \
  "Kubernetes hybrid RBAC uses the wrong namespace"
assert_absent "$kubernetes_hybrid" 'name: docker-socket' \
  "Kubernetes hybrid received the Docker socket"

split=(
  --namespace rsctf-system
  --set runtimeRole=web
  --set replicaCount=2
  --set-string image.tag=1.2.3
  --set postgresql.enabled=false
  --set redis.enabled=false
  --set existingSecret.name=rsctf-shared
  --set persistence.enabled=true
  --set persistence.existingClaim=rsctf-files-rwx
  --set 'persistence.accessModes[0]=ReadWriteMany'
  --set containerBackend=kubernetes
  --set kubernetes.challengeNamespace=rsctf-challenges
  --set kubernetes.createChallengeNamespace=false
  --set kubernetes.adServiceCidr=10.96.0.0/12
  --set kubernetes.networkPolicyEnforced=true
  --set config.dbMaxConnections=26
)
helm template rsctf-web charts/rsctf "${split[@]}" >/dev/null
reporter_selector='app.kubernetes.io/name=rsctf,app.kubernetes.io/instance=rsctf-network,app.kubernetes.io/component=network'
network_reporter="$(helm template rsctf-network charts/rsctf "${split[@]}" \
  --set runtimeRole=network \
  --set replicaCount=1 \
  --set config.dbMaxConnections=17 \
  --set config.kothReporterBaseUrl=http://rsctf-network.rsctf-system.svc:8080 \
  --show-only templates/configmap.yaml \
  --show-only templates/service.yaml)"
assert_contains "$network_reporter" "RSCTF_K8S_KOTH_REPORTER_POD_SELECTOR: \"$reporter_selector\"" \
  "network reporter callback did not select the exact network Service pods"
assert_contains "$network_reporter" $'  selector:\n    app.kubernetes.io/name: rsctf\n    app.kubernetes.io/instance: rsctf-network\n    app.kubernetes.io/component: "network"' \
  "network callback Service selector does not match the reporter egress identity"
for label in \
  'app.kubernetes.io/name: rsctf' \
  'app.kubernetes.io/instance: rsctf-network' \
  'app.kubernetes.io/component: "network"'; do
  assert_contains "$network_reporter" "$label" \
    "network callback Service is missing selector label $label"
done
engine_reporter="$(helm template rsctf-engine charts/rsctf "${split[@]}" \
  --set runtimeRole=engine \
  --set replicaCount=2 \
  --set config.dbMaxConnections=16 \
  --set config.kothReporterBaseUrl=http://rsctf-network.rsctf-system.svc:8080 \
  --set-string 'kubernetes.kothReporterPodSelector=app.kubernetes.io/name=rsctf\,app.kubernetes.io/instance=rsctf-network\,app.kubernetes.io/component=network' \
  --show-only templates/configmap.yaml)"
assert_contains "$engine_reporter" "RSCTF_K8S_KOTH_REPORTER_POD_SELECTOR: \"$reporter_selector\"" \
  "engine callback policy did not use the network Service identity"
if helm template rsctf-engine charts/rsctf "${split[@]}" \
  --set runtimeRole=engine \
  --set replicaCount=2 \
  --set config.dbMaxConnections=16 \
  --set config.kothReporterBaseUrl=http://rsctf-network.rsctf-system.svc:8080 >/dev/null 2>&1; then
  fail "Kubernetes engine accepted managed KotH reporting without the callback Service selector"
fi
if helm template rsctf-engine charts/rsctf "${split[@]}" \
  --set runtimeRole=engine \
  --set replicaCount=2 \
  --set config.dbMaxConnections=16 \
  --set config.kothReporterBaseUrl=http://rsctf-network.rsctf-system.svc:8080 \
  --set-string kubernetes.kothReporterPodSelector=app.kubernetes.io/name=rsctf >/dev/null 2>&1; then
  fail "Kubernetes engine accepted a callback selector shared by unrelated rsctf roles"
fi
for invalid_reporter_origin in \
  http://rsctf-network.rsctf-system.svc:0 \
  http://rsctf-network:8080 \
  http://rsctf-network.other-system.svc:8080; do
  if helm template rsctf-network charts/rsctf "${split[@]}" \
    --set runtimeRole=network \
    --set replicaCount=1 \
    --set config.dbMaxConnections=17 \
    --set config.kothReporterBaseUrl="$invalid_reporter_origin" >/dev/null 2>&1; then
    fail "Kubernetes managed reporting accepted an origin outside the rsctf release namespace: $invalid_reporter_origin"
  fi
done
split_ingress="$(helm template rsctf-web charts/rsctf "${split[@]}" \
  --set ingress.enabled=true \
  --set ingress.statefulRoutes.enabled=true \
  --set ingress.statefulRoutes.serviceName=rsctf-control \
  --show-only templates/ingress.yaml)"
stateful_backend="$(awk '
  $1 == "-" && $2 == "path:" { active = ($3 == "/api/stateful"); next }
  active && $1 == "name:" { gsub(/"/, "", $2); print $2; exit }
' <<<"$split_ingress")"
web_backend="$(awk '
  $1 == "-" && $2 == "path:" { active = ($3 == "/"); next }
  active && $1 == "name:" { gsub(/"/, "", $2); print $2; exit }
' <<<"$split_ingress")"
[[ "$stateful_backend" == "rsctf-control" ]] \
  || fail "split Ingress did not route /api/stateful to its configured singleton"
[[ "$web_backend" == "rsctf-web" ]] \
  || fail "split Ingress did not leave ordinary traffic on the web Service"
vpn_web="$(helm template rsctf-web charts/rsctf "${split[@]}" \
  --set vpn.enabled=true \
  --set vpn.serverEndpoint=vpn.ctf.example:51820)"
assert_absent "$vpn_web" '- NET_ADMIN' \
  "VPN-aware web role received NET_ADMIN"
assert_absent "$vpn_web" '- NET_RAW' \
  "VPN-aware web role received NET_RAW"
assert_absent "$vpn_web" 'name: tun' \
  "VPN-aware web role received the TUN device"

must_reject_split() {
  local label="$1"
  shift
  if helm template rsctf-web charts/rsctf "${split[@]}" "$@" >/dev/null 2>&1; then
    fail "split-role chart accepted $label"
  fi
}

must_reject_split "bundled PostgreSQL" --set postgresql.enabled=true
must_reject_split "bundled Redis" --set redis.enabled=true
must_reject_split "a generated application Secret" --set-string existingSecret.name=
must_reject_split "a release-owned challenge namespace" --set kubernetes.createChallengeNamespace=true
must_reject_split "an implicit challenge namespace" --set-string kubernetes.challengeNamespace=
must_reject_split "the mutable latest tag" --set-string image.tag=latest
must_reject_split "a missing NetworkPolicy enforcement acknowledgement" \
  --set kubernetes.networkPolicyEnforced=false

if helm template rsctf-migrate charts/rsctf \
  --set runtimeRole=migrate \
  --set replicaCount=1 \
  --set postgresql.enabled=false \
  --set redis.enabled=false \
  --set existingSecret.name=rsctf-shared \
  --set config.dbMaxConnections=2 >/dev/null 2>&1; then
  fail "migration role accepted the mutable latest tag"
fi

migrate_rendered="$(helm template rsctf-migrate charts/rsctf \
  --set runtimeRole=migrate \
  --set replicaCount=1 \
  --set postgresql.enabled=false \
  --set redis.enabled=false \
  --set existingSecret.name=rsctf-shared \
  --set-string image.tag=1.2.3 \
  --set config.dbMaxConnections=2 \
  --show-only templates/migrate-job.yaml)"
assert_contains "$migrate_rendered" 'name: RSCTF_IDENTITY_HASH_KEY' \
  "migration Pod is missing the stable identity hash key"
assert_contains "$migrate_rendered" 'key: identity-hash-key' \
  "migration Pod does not use the configured identity hash Secret key"

digest_a="sha256:$(printf 'a%.0s' {1..64})"
digest_b="sha256:$(printf 'b%.0s' {1..64})"
migrate_digest_args=(
  --set runtimeRole=migrate
  --set replicaCount=1
  --set postgresql.enabled=false
  --set redis.enabled=false
  --set existingSecret.name=rsctf-shared
  --set-string image.repository=registry.example/rsctf
  --set config.dbMaxConnections=2
  --show-only templates/migrate-job.yaml
)
migrate_digest_a="$(helm template rsctf-migrate charts/rsctf \
  "${migrate_digest_args[@]}" --set-string "image.digest=$digest_a")"
migrate_digest_b="$(helm template rsctf-migrate charts/rsctf \
  "${migrate_digest_args[@]}" --set-string "image.digest=$digest_b")"
assert_contains "$migrate_digest_a" "image: \"registry.example/rsctf@$digest_a\"" \
  "migration Job did not pin the exact manifest digest"
assert_absent "$migrate_digest_a" 'helm.sh/hook' \
  "migration Job must not run automatically while old runtime Pods serve"
job_a="$(awk '$1 == "name:" { print $2; exit }' <<<"$migrate_digest_a")"
job_b="$(awk '$1 == "name:" { print $2; exit }' <<<"$migrate_digest_b")"
[[ -n "$job_a" && -n "$job_b" && "$job_a" != "$job_b" ]] \
  || fail "migration Job identity is not scoped to the immutable digest"
long_name=rsctf-competition-control-plane-with-an-intentionally-long-resource-name
long_job_a="$(helm template rsctf-migrate charts/rsctf \
  "${migrate_digest_args[@]}" --set-string "image.digest=$digest_a" \
  --set-string "fullnameOverride=$long_name" \
  | awk '$1 == "name:" { print $2; exit }')"
long_job_b="$(helm template rsctf-migrate charts/rsctf \
  "${migrate_digest_args[@]}" --set-string "image.digest=$digest_b" \
  --set-string "fullnameOverride=$long_name" \
  | awk '$1 == "name:" { print $2; exit }')"
[[ ${#long_job_a} -le 63 && "$long_job_a" != "$long_job_b" ]] \
  || fail "long migration Job names do not retain their digest identity"
if helm template rsctf-migrate charts/rsctf \
  "${migrate_digest_args[@]}" --set-string image.digest=sha256:abc >/dev/null 2>&1; then
  fail "chart accepted an invalid image digest"
fi

echo "Helm chart validation passed."
