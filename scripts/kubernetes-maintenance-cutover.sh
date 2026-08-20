#!/usr/bin/env bash
# Stop every rsctf runtime Pod before applying a schema-incompatible release.

set -Eeuo pipefail

usage() {
  cat >&2 <<'EOF'
Usage: kubernetes-maintenance-cutover.sh \
  --namespace NAMESPACE \
  --chart CHART \
  --image-repository REPOSITORY \
  --image-digest sha256:... \
  --database-secret SECRET \
  --migrate-release RELEASE \
  --runtime-release RELEASE [--runtime-release RELEASE ...] \
  [--database-url-key KEY] [--identity-hash-key KEY] \
  [--chart-version VERSION] [--timeout 10m]

This is a stop-the-world operation. Pause GitOps reconcilers first. The script
refuses HPA-managed or ambiguous runtime sets, scales every selected runtime
Deployment to zero, verifies all old Pods are gone, runs one digest-scoped
migration Job, and only then upgrades/restores the runtime releases.
EOF
  exit 2
}

die() {
  printf 'rsctf Kubernetes cutover: %s\n' "$*" >&2
  exit 1
}

namespace=''
chart=''
chart_version=''
image_repository=''
image_digest=''
database_secret=''
database_url_key=database-url
identity_hash_key=identity-hash-key
migrate_release=''
timeout=10m
runtime_releases=()

while (($#)); do
  case "$1" in
    --namespace | --chart | --chart-version | --image-repository | --image-digest | --database-secret | --database-url-key | --identity-hash-key | --migrate-release | --runtime-release | --timeout)
      (($# >= 2)) || usage
      key=$1
      value=$2
      shift 2
      case "$key" in
        --namespace) namespace=$value ;;
        --chart) chart=$value ;;
        --chart-version) chart_version=$value ;;
        --image-repository) image_repository=$value ;;
        --image-digest) image_digest=$value ;;
        --database-secret) database_secret=$value ;;
        --database-url-key) database_url_key=$value ;;
        --identity-hash-key) identity_hash_key=$value ;;
        --migrate-release) migrate_release=$value ;;
        --runtime-release) runtime_releases+=("$value") ;;
        --timeout) timeout=$value ;;
      esac
      ;;
    -h | --help) usage ;;
    *) usage ;;
  esac
done

[[ -n "$namespace" && -n "$chart" && -n "$image_repository" \
  && -n "$image_digest" && -n "$database_secret" && -n "$migrate_release" ]] || usage
((${#runtime_releases[@]} > 0)) || usage
[[ "$namespace" =~ ^[a-z0-9]([-a-z0-9.]*[a-z0-9])?$ ]] \
  || die "namespace is not a DNS-safe Kubernetes name"
[[ "$migrate_release" =~ ^[a-z0-9]([-a-z0-9.]*[a-z0-9])?$ ]] \
  || die "migrate release is not a valid Helm release name"
[[ "$database_secret" =~ ^[a-z0-9]([-a-z0-9.]*[a-z0-9])?$ ]] \
  || die "database Secret is not a DNS-safe Kubernetes name"
for secret_key in "$database_url_key" "$identity_hash_key"; do
  [[ "$secret_key" =~ ^[-._a-zA-Z0-9]+$ ]] \
    || die "Secret key names may contain only letters, digits, dash, underscore, or dot"
done
[[ "$image_digest" =~ ^sha256:[0-9a-f]{64}$ ]] \
  || die "image digest must be sha256 followed by 64 lowercase hexadecimal characters"
[[ -n "$image_repository" && "$image_repository" != *[[:space:]@]* ]] \
  || die "image repository must not contain whitespace or a digest"
[[ "$timeout" =~ ^[1-9][0-9]*[sm]$ ]] \
  || die "timeout must be a positive whole number of seconds or minutes (for example 600s or 10m)"

for release in "${runtime_releases[@]}"; do
  [[ "$release" =~ ^[a-z0-9]([-a-z0-9.]*[a-z0-9])?$ ]] \
    || die "runtime release $release is not a valid Helm release name"
  [[ "$release" != "$migrate_release" ]] \
    || die "the migration release cannot also be a runtime release"
done
if [[ $(printf '%s\n' "${runtime_releases[@]}" | sort -u | wc -l) -ne ${#runtime_releases[@]} ]]; then
  die "runtime releases must be unique"
fi

command -v kubectl >/dev/null || die "kubectl is required"
command -v helm >/dev/null || die "Helm is required"
command -v python3 >/dev/null || die "python3 is required"

# `kubectl scale` does not alter Helm's stored values. Those values preserve
# the intended replica count across a failed migration, when live Deployments
# correctly remain at zero and the cutover must be safely retryable.
helm_replica_args=()
for release in "${runtime_releases[@]}"; do
  values_json=$(helm --namespace "$namespace" get values "$release" --all --output json) \
    || die "cannot read stored Helm values for runtime release $release"
  stored_role_replicas=$(printf '%s' "$values_json" | python3 /dev/fd/3 3<<'PY'
import json
import sys

document = json.load(sys.stdin)
role = document.get("runtimeRole")
value = document.get("replicaCount")
if role not in {"all", "web", "control", "engine", "network"}:
    raise SystemExit("stored runtimeRole is not a long-running role")
if not isinstance(value, int) or isinstance(value, bool) or value < 1:
    raise SystemExit("stored replicaCount must be a positive integer")
print(f"{role}\t{value}")
PY
  ) || die "runtime release $release has no safe stored runtimeRole/replicaCount"
  IFS=$'\t' read -r stored_role stored_replicas <<<"$stored_role_replicas"
  helm_replica_args+=("$release=$stored_role=$stored_replicas")
done

deployments_json=$(kubectl -n "$namespace" get deployments -o json) \
  || die "cannot list Deployments in namespace $namespace"
inventory=$(printf '%s' "$deployments_json" | python3 /dev/fd/3 \
  "${helm_replica_args[@]}" 3<<'PY'
import json
import sys

stored = {}
for value in sys.argv[1:]:
    release, role, replicas = value.rsplit("=", 2)
    stored[release] = (role, int(replicas))
requested = list(stored)
document = json.load(sys.stdin)
runtime_roles = {"all", "web", "control", "engine", "network"}
items = document.get("items", [])

def runtime_role(item):
    metadata_role = item.get("metadata", {}).get("labels", {}).get("app.kubernetes.io/component")
    template_role = (
        item.get("spec", {}).get("template", {}).get("metadata", {}).get("labels", {})
        .get("app.kubernetes.io/component")
    )
    if metadata_role and template_role and metadata_role != template_role:
        raise SystemExit(
            f"Deployment {item.get('metadata', {}).get('name')!r} has conflicting component labels"
        )
    return metadata_role or template_role

selected = []
for release in requested:
    matches = [
        item for item in items
        if item.get("metadata", {}).get("labels", {}).get("app.kubernetes.io/instance") == release
        and runtime_role(item) in runtime_roles
        and item.get("metadata", {}).get("labels", {}).get("app.kubernetes.io/managed-by") == "Helm"
    ]
    if len(matches) != 1:
        raise SystemExit(
            f"runtime release {release!r} matched {len(matches)} Deployments; expected exactly one"
        )
    if runtime_role(matches[0]) != stored[release][0]:
        raise SystemExit(
            f"runtime release {release!r} live component does not match its stored runtimeRole"
        )
    selected.append((release, matches[0]))

app_names = {
    item.get("metadata", {}).get("labels", {}).get("app.kubernetes.io/name")
    for _, item in selected
}
if None in app_names or len(app_names) != 1:
    raise SystemExit("runtime releases do not share one unambiguous app.kubernetes.io/name")
app_name = next(iter(app_names))

related_releases = {
    item.get("metadata", {}).get("labels", {}).get("app.kubernetes.io/instance")
    for item in items
    if item.get("metadata", {}).get("labels", {}).get("app.kubernetes.io/name") == app_name
    and runtime_role(item) in runtime_roles
}
if related_releases != set(requested):
    missing = sorted(related_releases - set(requested))
    extra = sorted(set(requested) - related_releases)
    raise SystemExit(
        f"ambiguous runtime set for app {app_name!r}; unlisted={missing}, unmatched={extra}"
    )

role_counts = {}
restore_priority = {
    # These roles have no peer-readiness dependency.
    "all": 0,
    "control": 0,
    "network": 0,
    # An engine may require network readiness when VPN is enabled, and web
    # always requires its control provider(s) before /healthz can succeed.
    "engine": 1,
    "web": 2,
}
selected.sort(key=lambda entry: restore_priority[runtime_role(entry[1])])

for release, item in selected:
    role = runtime_role(item)
    role_counts[role] = role_counts.get(role, 0) + 1
if "all" in role_counts:
    if role_counts != {"all": 1}:
        raise SystemExit("runtimeRole=all cannot be mixed with split roles")
elif "control" in role_counts:
    if role_counts != {"web": 1, "control": 1}:
        raise SystemExit("control topology requires exactly one web and one control release")
else:
    if role_counts != {"web": 1, "engine": 1, "network": 1}:
        raise SystemExit("engine topology requires exactly one web, engine, and network release")

print(f"APP\t{app_name}")
for release, item in selected:
    metadata = item["metadata"]
    live_replicas = item.get("spec", {}).get("replicas", 1)
    if not isinstance(live_replicas, int) or isinstance(live_replicas, bool) or live_replicas < 0:
        raise SystemExit(f"Deployment {metadata.get('name')!r} has an invalid replica count")
    # A positive live count captures deliberate kubectl scaling on the first
    # run. Zero means a prior fail-closed attempt; recover from Helm's durable
    # release values instead of resurrecting the old image to rediscover it.
    replicas = live_replicas if live_replicas > 0 else stored[release][1]
    if runtime_role(item) in {"all", "control", "network"} and replicas != 1:
        raise SystemExit(
            f"runtimeRole={runtime_role(item)} requires exactly one replica before migration"
        )
    print(f"DEPLOYMENT\t{release}\t{metadata['name']}\t{replicas}")
PY
) || die "runtime Deployment inventory is unsafe or ambiguous"

app_name=''
deployment_names=()
deployment_replicas=()
deployment_releases=()
while IFS=$'\t' read -r kind first second third; do
  case "$kind" in
    APP) app_name=$first ;;
    DEPLOYMENT)
      deployment_releases+=("$first")
      deployment_names+=("$second")
      deployment_replicas+=("$third")
      ;;
  esac
done <<<"$inventory"
[[ -n "$app_name" && ${#deployment_names[@]} -eq ${#runtime_releases[@]} ]] \
  || die "runtime Deployment inventory was incomplete"

hpa_json=$(kubectl -n "$namespace" get horizontalpodautoscalers.autoscaling -o json) \
  || die "cannot inspect HorizontalPodAutoscalers"
printf '%s' "$hpa_json" | python3 /dev/fd/3 "${deployment_names[@]}" 3<<'PY' \
  || die "disable every runtime HPA before the cutover"
import json
import sys

targets = set(sys.argv[1:])
managed = []
for item in json.load(sys.stdin).get("items", []):
    ref = item.get("spec", {}).get("scaleTargetRef", {})
    if ref.get("kind") == "Deployment" and ref.get("name") in targets:
        managed.append(item.get("metadata", {}).get("name", "<unnamed>"))
if managed:
    raise SystemExit("runtime Deployments are HPA-managed: " + ", ".join(sorted(managed)))
PY

printf 'Scaling rsctf runtime Deployments to zero in %s...\n' "$namespace"
for deployment in "${deployment_names[@]}"; do
  kubectl -n "$namespace" scale "deployment/$deployment" --replicas=0 \
    || die "failed to scale deployment/$deployment to zero; runtime remains in maintenance"
done

timeout_value=${timeout%[sm]}
if [[ "$timeout" == *m ]]; then
  timeout_seconds=$((timeout_value * 60))
else
  timeout_seconds=$timeout_value
fi
deadline=$((SECONDS + timeout_seconds))
runtime_selector="app.kubernetes.io/name=$app_name,app.kubernetes.io/component in (all,web,control,engine,network)"

quiesced=false
while ((SECONDS < deadline)); do
  current_deployments=$(kubectl -n "$namespace" get deployments -o json) \
    || die "lost permission to verify runtime Deployment scale-down"
  current_pods=$(kubectl -n "$namespace" get pods -l "$runtime_selector" -o json) \
    || die "lost permission to verify old runtime Pod termination"
  if python3 /dev/fd/3 "${deployment_names[@]}" \
    3<<'PY' 4<<<"$current_deployments" 5<<<"$current_pods"
import json
import os
import sys

expected = set(sys.argv[1:])
with os.fdopen(4) as deployments_file:
    deployments = json.load(deployments_file)
seen = set()
for item in deployments.get("items", []):
    name = item.get("metadata", {}).get("name")
    if name not in expected:
        continue
    seen.add(name)
    spec = item.get("spec", {})
    status = item.get("status", {})
    if spec.get("replicas", 1) != 0:
        raise SystemExit(1)
    for field in ("replicas", "readyReplicas", "availableReplicas", "updatedReplicas"):
        if status.get(field, 0) != 0:
            raise SystemExit(1)
with os.fdopen(5) as pods_file:
    pods = json.load(pods_file)
if seen != expected or pods.get("items"):
    raise SystemExit(1)
PY
  then
    quiesced=true
    break
  fi
  sleep 2
done
[[ "$quiesced" == true ]] \
  || die "runtime Pods did not terminate before the timeout; migration was not started"

printf 'Old runtime Pods are absent; running the digest-scoped migration Job...\n'
prior_jobs_json=$(kubectl -n "$namespace" get jobs \
  -l "app.kubernetes.io/instance=$migrate_release,app.kubernetes.io/component=migrate" \
  -o json) || die "cannot inspect prior migration Jobs"
prior_job=$(printf '%s' "$prior_jobs_json" | python3 /dev/fd/3 \
  "$image_repository@$image_digest" 3<<'PY'
import json
import sys

expected_image = sys.argv[1]
matches = []
for item in json.load(sys.stdin).get("items", []):
    containers = item.get("spec", {}).get("template", {}).get("spec", {}).get("containers", [])
    if not any(container.get("name") == "migrate" and container.get("image") == expected_image for container in containers):
        continue
    matches.append(item)
if len(matches) > 1:
    raise SystemExit("multiple Jobs claim the current migration digest")
if not matches:
    raise SystemExit(0)
item = matches[0]
status = item.get("status", {})
name = item.get("metadata", {}).get("name", "")
if status.get("active", 0) != 0:
    raise SystemExit("a migration Job for the current digest is still active")
if status.get("succeeded", 0) < 1 and status.get("failed", 0) < 1:
    raise SystemExit("the existing migration Job is neither successful nor failed")
print(name)
PY
) || die "prior migration Job state is ambiguous"
if [[ -n "$prior_job" ]]; then
  [[ "$prior_job" =~ ^[a-z0-9]([-a-z0-9.]*[a-z0-9])?$ ]] \
    || die "prior migration Job has an unsafe name"
  # A completed Job does not execute again on `helm upgrade`. Recreate it on
  # every cutover attempt so quiescence preflight and identity bootstrap also
  # run after a database restore or a crash between schema commit and bootstrap.
  kubectl -n "$namespace" delete "job/$prior_job" --wait=true --timeout="$timeout" \
    || die "prior migration Job could not be removed for a fresh safe attempt"
fi

helm_common=(
  --namespace "$namespace"
  --set runtimeRole=migrate
  --set replicaCount=1
  --set postgresql.enabled=false
  --set redis.enabled=false
  --set-string "existingSecret.name=$database_secret"
  --set-string "existingSecret.databaseUrlKey=$database_url_key"
  --set-string "existingSecret.identityHashKey=$identity_hash_key"
  --set-string "image.repository=$image_repository"
  --set-string "image.digest=$image_digest"
  --set config.dbMaxConnections=2
  --wait
  --wait-for-jobs
  --timeout "$timeout"
)
if [[ -n "$chart_version" ]]; then
  helm_common+=(--version "$chart_version")
fi
if ! helm upgrade --install "$migrate_release" "$chart" "${helm_common[@]}"; then
  kubectl -n "$namespace" logs \
    -l "app.kubernetes.io/instance=$migrate_release,app.kubernetes.io/component=migrate" \
    --all-containers --prefix --tail=100 >&2 2>/dev/null || true
  die "migration failed; all old runtime Deployments remain scaled to zero and must not be restored"
fi

jobs_json=$(kubectl -n "$namespace" get jobs \
  -l "app.kubernetes.io/instance=$migrate_release,app.kubernetes.io/component=migrate" \
  -o json) || die "cannot verify the completed migration Job"
migration_job=$(printf '%s' "$jobs_json" | python3 /dev/fd/3 \
  "$image_repository@$image_digest" 3<<'PY'
import json
import sys

expected_image = sys.argv[1]
matches = []
for item in json.load(sys.stdin).get("items", []):
    containers = item.get("spec", {}).get("template", {}).get("spec", {}).get("containers", [])
    if not any(container.get("name") == "migrate" and container.get("image") == expected_image for container in containers):
        continue
    status = item.get("status", {})
    if status.get("succeeded", 0) < 1 or status.get("failed", 0) != 0 or status.get("active", 0) != 0:
        raise SystemExit("digest-scoped migration Job is not unambiguously complete")
    matches.append(item.get("metadata", {}).get("name"))
if len(matches) != 1 or not matches[0]:
    raise SystemExit(f"expected one successful digest-scoped migration Job, found {matches!r}")
print(matches[0])
PY
) || die "migration Job success could not be proven; runtime releases remain stopped"
kubectl -n "$namespace" logs "job/$migration_job" \
  --all-containers --prefix --tail=100 \
  || die "migration Job completed but its logs could not be inspected; runtime releases remain stopped"

# A controller that ignored the scale-down is detected before any runtime is
# restored. Do not rely on the Job alone: its DB preflight is defense-in-depth.
remaining_deployments=$(kubectl -n "$namespace" get deployments -o json) \
  || die "cannot re-verify runtime Deployment scale after migration"
remaining_pods=$(kubectl -n "$namespace" get pods -l "$runtime_selector" -o json) \
  || die "cannot re-verify old runtime Pod absence after migration"
python3 /dev/fd/3 "${deployment_names[@]}" \
  3<<'PY' 4<<<"$remaining_deployments" 5<<<"$remaining_pods" \
  || die "a runtime Deployment or Pod reappeared during migration; do not start or roll back any runtime release"
import json
import os
import sys

expected = set(sys.argv[1:])
with os.fdopen(4) as deployments_file:
    deployments = json.load(deployments_file)
seen = set()
for item in deployments.get("items", []):
    name = item.get("metadata", {}).get("name")
    if name not in expected:
        continue
    seen.add(name)
    if item.get("spec", {}).get("replicas", 1) != 0:
        raise SystemExit(1)
    status = item.get("status", {})
    for field in ("replicas", "readyReplicas", "availableReplicas", "updatedReplicas"):
        if status.get(field, 0) != 0:
            raise SystemExit(1)
with os.fdopen(5) as pods_file:
    pods = json.load(pods_file)
if seen != expected or pods.get("items"):
    raise SystemExit(1)
PY

printf 'Migration succeeded; upgrading runtime releases to %s@%s...\n' \
  "$image_repository" "$image_digest"
for index in "${!deployment_releases[@]}"; do
  release=${deployment_releases[$index]}
  replicas=${deployment_replicas[$index]}
  runtime_args=(
    --namespace "$namespace"
    --reset-then-reuse-values
    --set-string "image.repository=$image_repository"
    --set-string "image.digest=$image_digest"
    --set config.migrate=false
    --set "replicaCount=$replicas"
    --wait
    --timeout "$timeout"
  )
  if [[ -n "$chart_version" ]]; then
    runtime_args+=(--version "$chart_version")
  fi
  helm upgrade "$release" "$chart" "${runtime_args[@]}" \
    || die "runtime release $release failed; do not roll it back to the old image and leave any not-yet-upgraded releases at zero"
done

expected_image="$image_repository@$image_digest"
final_deployments=$(kubectl -n "$namespace" get deployments -o json) \
  || die "cannot verify final runtime Deployment state"
python3 /dev/fd/3 "$expected_image" \
  "${deployment_names[@]}" -- "${deployment_replicas[@]}" \
  3<<'PY' 4<<<"$final_deployments" \
  || die "runtime Deployments did not reach the expected digest, replica count, and readiness"
import json
import os
import sys

expected_image, *arguments = sys.argv[1:]
separator = arguments.index("--")
names = arguments[:separator]
replicas = [int(value) for value in arguments[separator + 1:]]
if len(names) != len(replicas):
    raise SystemExit(1)
expected = dict(zip(names, replicas))
with os.fdopen(4) as deployments_file:
    deployments = json.load(deployments_file)
seen = set()
for item in deployments.get("items", []):
    name = item.get("metadata", {}).get("name")
    if name not in expected:
        continue
    seen.add(name)
    count = expected[name]
    if item.get("spec", {}).get("replicas") != count:
        raise SystemExit(1)
    status = item.get("status", {})
    if status.get("readyReplicas", 0) != count or status.get("availableReplicas", 0) != count:
        raise SystemExit(1)
    containers = item.get("spec", {}).get("template", {}).get("spec", {}).get("containers", [])
    images = [container.get("image") for container in containers if container.get("name") == "rsctf"]
    if images != [expected_image]:
        raise SystemExit(1)
if seen != set(expected):
    raise SystemExit(1)
PY

printf 'Kubernetes maintenance cutover completed at immutable image %s.\n' "$expected_image"
