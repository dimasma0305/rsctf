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
  test_team_player_api
  kubectl delete storageclass rsctf-test-csi
)

# Uses only the disposable Kind network and containers created by this test.
test_team_player_api() (
  set -euo pipefail
  node="$cluster_name-control-plane"
  registry_id=''
  postgres_id=''
  source_id=''
  tags=()
  references=()
  cleanup_team_api() {
    for container in "$source_id" "$postgres_id" "$registry_id"; do
      if [[ -n "$container" ]]; then docker rm -fv "$container" >/dev/null; fi
    done
    if (( ${#tags[@]} )); then docker image rm "${tags[@]}" >/dev/null; fi
  }
  trap cleanup_team_api EXIT
  registry_id="$(docker run -d --network kind -p 127.0.0.1::5000 \
    registry:3.1.2@sha256:82f6ff4a7f982960c8aa51617fea3aabc485df6e55cce80d22deb1de36f1bc3c)"
  registry="$(docker port "$registry_id" 5000/tcp)"
  curl --fail --silent --retry 10 --retry-connrefused --max-time 10 "http://$registry/v2/" >/dev/null
  registry_ip="$(docker inspect -f '{{(index .NetworkSettings.Networks "kind").IPAddress}}' "$registry_id")"
  # This localhost registry belongs to this runner. Point only the disposable
  # node at its Docker-network address, leaving the host's containerd alone.
  docker exec "$node" mkdir -p "/etc/containerd/certs.d/$registry"
  docker exec -i "$node" tee "/etc/containerd/certs.d/$registry/hosts.toml" >/dev/null <<TOML
[host."http://$registry_ip:5000"]
  capabilities = ["pull", "resolve"]
TOML
  repo="$registry/rsctf/service-900041"
  docker pull "$starter"
  source_id="$(docker create "$starter")"
  for revision in starter previous patched; do
    tag="$repo:$revision"
    docker commit --change 'ENV RSCTF_FLAG_FILE=/data/flag' \
      --change "ENV RSCTF_TEST_REVISION=$revision" "$source_id" "$tag" >/dev/null
    tags+=("$tag")
    docker push "$tag"
    # Docker's local repoDigests array can contain sibling tags' digests. Read
    # the manifest currently published at this tag from the fixture registry.
    digest="$(curl --fail --silent --head -H 'Accept: application/vnd.docker.distribution.manifest.v2+json,application/vnd.oci.image.manifest.v1+json' \
      "http://$registry/v2/rsctf/service-900041/manifests/$revision" |
      awk 'tolower($1)=="docker-content-digest:" {gsub(/\r/,"",$2);print $2}')"
    [[ "$digest" =~ ^sha256:[0-9a-f]{64}$ ]]
    references+=("$repo@$digest")
  done
  config="$(docker image inspect --format '{{.Id}}' "$repo:patched")"
  password="$(openssl rand -hex 24)"
  postgres_id="$(docker run -d -p 127.0.0.1::5432 -e "POSTGRES_PASSWORD=$password" postgres:18-alpine)"
  db_address="$(docker port "$postgres_id" 5432/tcp)"
  for _ in {1..40}; do
    if docker exec "$postgres_id" pg_isready -h 127.0.0.1 -U postgres >/dev/null; then break; fi
    sleep 1
  done
  docker exec "$postgres_id" pg_isready -h 127.0.0.1 -U postgres
  RSCTF_TEST_DATABASE_URL="postgresql://postgres:$password@$db_address/postgres" \
    RSCTF_K8S_NAMESPACE=rsctf-retry RSCTF_K8S_NETWORK_POLICY_ENFORCED=true \
    RSCTF_K8S_AD_SERVICE_CIDR=10.96.0.0/12 RSCTF_K8S_AD_INGRESS_CIDRS="$client_ip/32" \
    RSCTF_K8S_TEST_STARTER="${references[0]}" RSCTF_K8S_TEST_REVISION_ONE="${references[1]}" \
    RSCTF_K8S_TEST_REVISION_TWO="${references[2]}" RSCTF_K8S_TEST_CANDIDATE_CONFIG="$config" \
    RSCTF_K8S_READINESS_PROBE=true RSCTF_K8S_LIVE_STORAGE=1 \
    run_rust_test controllers::game::ad::player_reset_tests::kubernetes::player_kubernetes_patch_activation_and_rollbacks
)
