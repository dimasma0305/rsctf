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

jwt=(--set-string secrets.jwtSecret=0123456789abcdef0123456789abcdef)

helm lint charts/rsctf --strict "${jwt[@]}"

rbac="$(helm template rsctf charts/rsctf \
  --show-only templates/rbac.yaml \
  --set containerBackend=kubernetes \
  --set kubernetes.adServiceCidr=10.96.0.0/12 \
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
  --set persistence.accessModes[0]=ReadWriteMany
  --set containerBackend=worker
  --set workerBackend.localBackend=none
  --set trafficCapture.enabled=false
  --set config.dbMaxConnections=13
)
web_rendered="$(helm template rsctf-web charts/rsctf "${web[@]}")"
assert_absent "$web_rendered" 'RSCTF_WORKER_LISTEN' \
  "web role received the singleton worker listener"
assert_absent "$web_rendered" 'worker-ca.key' \
  "web role received the worker CA key"
assert_absent "$web_rendered" 'name: docker-socket' \
  "web role received the Docker socket"
assert_absent "$web_rendered" '- NET_RAW' \
  "web role received NET_RAW while capture is disabled"

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
  --set config.dbMaxConnections=20 >/dev/null 2>&1; then
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

kubernetes_hybrid="$(helm template rsctf charts/rsctf "${worker[@]}" \
  --set workerBackend.localBackend=kubernetes \
  --set kubernetes.challengeNamespace=rsctf-challenges \
  --set kubernetes.adServiceCidr=10.96.0.0/12)"
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
  --set runtimeRole=web
  --set replicaCount=2
  --set-string image.tag=1.2.3
  --set postgresql.enabled=false
  --set redis.enabled=false
  --set existingSecret.name=rsctf-shared
  --set persistence.enabled=true
  --set persistence.existingClaim=rsctf-files-rwx
  --set persistence.accessModes[0]=ReadWriteMany
  --set containerBackend=kubernetes
  --set kubernetes.challengeNamespace=rsctf-challenges
  --set kubernetes.createChallengeNamespace=false
  --set kubernetes.adServiceCidr=10.96.0.0/12
  --set config.dbMaxConnections=13
)
helm template rsctf-web charts/rsctf "${split[@]}" >/dev/null

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

if helm template rsctf-migrate charts/rsctf \
  --set runtimeRole=migrate \
  --set replicaCount=1 \
  --set postgresql.enabled=false \
  --set redis.enabled=false \
  --set existingSecret.name=rsctf-shared \
  --set config.dbMaxConnections=2 >/dev/null 2>&1; then
  fail "migration role accepted the mutable latest tag"
fi

echo "Helm chart validation passed."
