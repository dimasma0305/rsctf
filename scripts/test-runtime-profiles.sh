#!/usr/bin/env bash
# Disposable real Docker/Kubernetes/Kata lifecycle gate. Never targets an existing cluster.
set -Eeuo pipefail
[[ "${RSCTF_ALLOW_RUNTIME_PROFILE_CLUSTER_TEST:-}" == 1 ]] || {
  echo 'Set RSCTF_ALLOW_RUNTIME_PROFILE_CLUSTER_TEST=1 on a disposable KVM-capable test host.' >&2
  exit 1
}
for tool in docker kind kubectl helm curl sha256sum; do command -v "$tool" >/dev/null; done
[[ -c /dev/kvm && -c /dev/vhost-vsock ]] || {
  echo 'KVM and vhost-vsock are required; this gate cannot pass by skipping MicroVM execution.' >&2
  exit 1
}
cluster="rsctf-profiles-${RANDOM}"
scratch="$(mktemp -d /tmp/rsctf-profiles.XXXXXX)"
export KUBECONFIG="$scratch/kubeconfig"
cleanup() {
  result=$?
  if [[ "$result" != 0 ]]; then
    kubectl get pods -A -o wide || true
    kubectl get events -A --sort-by=.lastTimestamp | tail -80 || true
    docker exec "${cluster}-control-plane" df -h /dev/shm || true
    docker exec "${cluster}-control-plane" journalctl -u containerd \
      --since '-15 minutes' --no-pager -n 600 || true
  fi
  kind delete cluster --name "$cluster" || true
  rm -r -- "$scratch"
  exit "$result"
}
trap cleanup EXIT
cat > "$scratch/kind.yaml" <<'YAML'
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
nodes:
  - role: control-plane
    extraMounts:
      - hostPath: /lib/modules
        containerPath: /lib/modules
        readOnly: true
YAML
kind create cluster --name "$cluster" --config "$scratch/kind.yaml" \
  --image kindest/node:v1.36.1@sha256:3489c7674813ba5d8b1a9977baea8a6e553784dab7b84759d1014dbd78f7ebd5 --wait 120s
# kind's minimal node image has systemd but omits the system bus. Kata's Rust
# runtime uses that bus to place the VM in its systemd cgroup. Install it inside
# our disposable node; never mount the runner's host bus into the cluster.
docker exec "${cluster}-control-plane" sh -ec '
  apt-get update -qq
  DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends dbus
  systemctl enable --now dbus.socket dbus.service
  test -S /run/dbus/system_bus_socket
  busctl --system list --no-pager
'
# QEMU's virtio-fs shared guest RAM lives in /dev/shm. Docker's default 64 MiB
# mount makes guest memory faults fail with SIGBUS before the agent can start.
# This changes only this disposable node's tmpfs ceiling, not host mounts, and
# does not allocate 4 GiB eagerly.
docker exec "${cluster}-control-plane" mount -o remount,size=4G /dev/shm
docker exec "${cluster}-control-plane" df -h /dev/shm
kubectl create namespace rsctf-profile-challenges
curl --fail --silent --show-error --location \
  https://github.com/kata-containers/kata-containers/releases/download/4.3.1/kata-deploy-4.3.1.tgz \
  --output "$scratch/kata-deploy.tgz"
printf '%s  %s\n' c25077416d37c178bf72df0204ea3ba89418efad1b1299b0d5d34ec02556187e "$scratch/kata-deploy.tgz" | sha256sum --check --strict
helm install kata-deploy "$scratch/kata-deploy.tgz" --namespace kube-system \
  --set shims.disableAll=true --set shims.qemu-runtime-rs.enabled=true \
  --set 'tolerations[0].operator=Exists' --wait --timeout 15m
kubectl get runtimeclass kata-qemu-runtime-rs -o yaml
export RSCTF_RUNTIME_PROFILE_TEST_IMAGE='docker.io/nginxinc/nginx-unprivileged@sha256:b5047b9b76845eac124358373c6852377c52e4b05834a47647e955dd91cb8f1e'
docker pull "$RSCTF_RUNTIME_PROFILE_TEST_IMAGE"
export RSCTF_K8S_NAMESPACE=rsctf-profile-challenges
export RSCTF_K8S_NETWORK_POLICY_ENFORCED=true
export RSCTF_K8S_AD_SERVICE_CIDR=10.96.0.0/12
test_name=services::container::profiles::real_tests::real_runtime_profile_switch_matrix_proves_guest_isolation
if [[ -n "${RSCTF_K8S_TEST_BINARY:-}" ]]; then
  "$RSCTF_K8S_TEST_BINARY" "$test_name" --ignored --exact --nocapture
else
  scripts/bounded-cargo.sh test --locked --lib --all-features "$test_name" -- --ignored --exact --nocapture
fi
