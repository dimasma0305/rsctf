#!/usr/bin/env bash
# Sourced only inside the disposable Kind cluster created by the callback gate.
test_team_storage() (
  set -euo pipefail
  [[ "$(kubectl config current-context)" == "kind-$cluster_name" ]]
  local_assets="$kubeconfig_directory/team-csi"
  mkdir "$local_assets"
  sig_storage=https://raw.githubusercontent.com/kubernetes-csi
  driver="$sig_storage/csi-driver-host-path/6fab18ae4fc4f486193b618114eeb817d9929242/deploy/kubernetes-1.35/hostpath"
  kubectl apply -f "$sig_storage/external-provisioner/72f13ee46fb93c11a72ef91549e70158283c03af/deploy/kubernetes/rbac.yaml"
  kubectl apply -f "$sig_storage/external-attacher/40d44ae14961a415348180a0e666651e82e44eb7/deploy/kubernetes/rbac.yaml"
  kubectl apply -f "$driver/csi-hostpath-driverinfo.yaml"
  curl --proto '=https' -fsSL --retry 3 "$driver/csi-hostpath-plugin.yaml" -o "$local_assets/driver.yaml"
  # The fixture uses provisioning and attachment. Snapshot/resize controllers
  # have separate APIs and RBAC which this test cluster does not install.
  kubectl create --dry-run=client -f "$local_assets/driver.yaml" -o json |
    jq -s '{apiVersion:"v1",kind:"List",items:[.[] | if .kind=="List" then .items[] else . end]} | .items |= map(select((.metadata.name | test("snapshot|resizer")) | not)
        | if .kind=="StatefulSet" then .spec.template.spec.containers |=
          map(select(.name!="csi-snapshotter" and .name!="csi-resizer")) else . end)' |
    kubectl apply -f -
  kubectl rollout status statefulset/csi-hostpathplugin --timeout=5m
  kubectl apply -f - <<'YAML'
apiVersion: storage.k8s.io/v1
kind: StorageClass
metadata:
  name: rsctf-test-csi
  annotations:
    storageclass.kubernetes.io/is-default-class: "true"
provisioner: hostpath.csi.k8s.io
volumeBindingMode: Immediate
YAML
  starter=docker.io/library/redis@sha256:57e4255cc971b1c8d6f8a6ca283ac01bc3ecdd914273ef6bfaede87aaecd6541
  revision_one=docker.io/library/redis@sha256:7f7d30b7fdd09467bac58198a1f166dd69789d348d4fe859cccb20e0cf287fbe
  revision_two=docker.io/library/redis@sha256:da23c2478319ceb4903857d77a2fd7aa5137d9e9d4c8165dafbacedac6403e95
  for image in "$starter" "$revision_one" "$revision_two"; do
    docker exec "$cluster_name-control-plane" crictl pull "$image"
  done
  kubectl run team-route-client -n rsctf-retry --image="$starter" --restart=Never \
    --overrides='{"spec":{"automountServiceAccountToken":false,"securityContext":{"runAsNonRoot":true,"runAsUser":10000}}}' \
    --command -- sleep 1800
  kubectl wait -n rsctf-retry --for=condition=Ready pod/team-route-client --timeout=120s
  client_ip="$(kubectl get pod -n rsctf-retry team-route-client -o jsonpath='{.status.podIP}')"
  RSCTF_K8S_NAMESPACE=rsctf-retry RSCTF_K8S_NETWORK_POLICY_ENFORCED=true \
    RSCTF_K8S_AD_SERVICE_CIDR=10.96.0.0/12 RSCTF_K8S_AD_INGRESS_CIDRS="$client_ip/32" \
    RSCTF_K8S_READINESS_PROBE=true RSCTF_K8S_TEST_STARTER="$starter" RSCTF_K8S_TEST_REVISION_ONE="$revision_one" \
    RSCTF_K8S_TEST_REVISION_TWO="$revision_two" RSCTF_K8S_LIVE_STORAGE=1 \
    run_rust_test services::k8s::team_claim::tests::real_team_storage_survives_recreation
  kubectl delete storageclass rsctf-test-csi
)
