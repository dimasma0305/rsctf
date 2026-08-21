#!/usr/bin/env bash
# Stop every Compose runtime before a schema-incompatible migration.

set -Eeuo pipefail

usage() {
  cat >&2 <<'EOF'
Usage: compose-maintenance-cutover.sh \
  --project-name NAME \
  [--project-directory DIR] [--env-file FILE] [--image IMAGE] \
  [--compose-file FILE ...] [--migrate-service SERVICE] \
  [--state-file FILE] [--timeout 600]

Pass --image as an immutable repository@sha256:... reference, export the same
RSCTF_IMAGE, or provide it as an exact RSCTF_IMAGE entry in --env-file. Explicit
--image takes precedence. The selected Compose configuration must already
describe that intended release.
The script records the replica counts, stops every rsctf runtime container in
the project, verifies none are running, runs one migration container from
RSCTF_IMAGE, and only then removes the stopped old containers and force-recreates
the new runtime services. A small local state file preserves replica counts for
fail-closed retries and is removed only after the new release is healthy.
EOF
  exit 2
}

die() {
  printf 'rsctf Compose cutover: %s\n' "$*" >&2
  exit 1
}

cleanup_state_tmp() {
  if [[ -n ${state_tmp:-} && -f "$state_tmp" && ! -L "$state_tmp" ]]; then
    rm -- "$state_tmp"
  fi
}

array_contains() {
  local needle=$1
  shift
  local value
  for value in "$@"; do
    [[ "$value" == "$needle" ]] && return 0
  done
  return 1
}

project_name=''
project_directory=''
env_file=''
image_reference=''
migrate_service=rsctf
timeout=600
state_file=''
compose_files=()

while (($#)); do
  case "$1" in
    --project-name | --project-directory | --env-file | --image | --compose-file | --migrate-service | --state-file | --timeout)
      (($# >= 2)) || usage
      key=$1
      value=$2
      shift 2
      case "$key" in
        --project-name) project_name=$value ;;
        --project-directory) project_directory=$value ;;
        --env-file) env_file=$value ;;
        --image) image_reference=$value ;;
        --compose-file) compose_files+=("$value") ;;
        --migrate-service) migrate_service=$value ;;
        --state-file) state_file=$value ;;
        --timeout) timeout=$value ;;
      esac
      ;;
    -h | --help) usage ;;
    *) usage ;;
  esac
done

[[ -n "$project_name" ]] || usage
[[ "$project_name" =~ ^[a-z0-9][a-z0-9_.-]*$ ]] \
  || die "project name must use lowercase letters, digits, dot, underscore, or dash"
[[ "$migrate_service" =~ ^[A-Za-z0-9][A-Za-z0-9_.-]*$ ]] \
  || die "migration service name is invalid"
[[ "$timeout" =~ ^[1-9][0-9]*$ ]] \
  || die "timeout must be a positive number of seconds"
command -v docker >/dev/null || die "Docker is required"
command -v python3 >/dev/null || die "python3 is required"

if [[ -n "$env_file" ]]; then
  [[ -f "$env_file" ]] || die "environment file does not exist"
fi
if [[ -z "$image_reference" ]]; then
  image_reference=${RSCTF_IMAGE:-}
fi
if [[ -z "$image_reference" && -n "$env_file" ]]; then
  image_reference=$(python3 /dev/fd/3 "$env_file" 3<<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
values = []
for raw_line in path.read_text(encoding="utf-8").splitlines():
    line = raw_line.strip()
    if not line or line.startswith("#"):
        continue
    if line.startswith("export "):
        line = line[7:].lstrip()
    key, separator, value = line.partition("=")
    if separator and key.strip() == "RSCTF_IMAGE":
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        values.append(value)
if len(values) > 1:
    raise SystemExit("environment file contains duplicate RSCTF_IMAGE entries")
if values:
    print(values[0])
PY
  ) || die "could not resolve a unique RSCTF_IMAGE from the environment file"
fi
[[ "$image_reference" =~ ^[^[:space:]@]+@sha256:[0-9a-f]{64}$ ]] \
  || die "--image, RSCTF_IMAGE, or --env-file must provide an immutable repository@sha256:<64 lowercase hex> reference"
RSCTF_IMAGE=$image_reference
export RSCTF_IMAGE

compose=(docker compose --project-name "$project_name")
if [[ -n "$project_directory" ]]; then
  [[ -d "$project_directory" ]] || die "project directory does not exist"
  compose+=(--project-directory "$project_directory")
fi
if [[ -n "$env_file" ]]; then
  compose+=(--env-file "$env_file")
fi
for file in "${compose_files[@]}"; do
  [[ -f "$file" ]] || die "Compose file does not exist: $file"
  compose+=(--file "$file")
done
if [[ -z "$state_file" ]]; then
  state_file="${project_directory:-.}/.rsctf-${project_name}-cutover-state.json"
fi
state_parent=$(dirname -- "$state_file")
[[ -d "$state_parent" ]] || die "cutover state parent directory does not exist"
[[ ! -L "$state_file" ]] || die "cutover state file must not be a symbolic link"
if [[ -e "$state_file" ]]; then
  [[ -f "$state_file" ]] || die "cutover state path is not a regular file"
fi

config_json=$("${compose[@]}" config --format json) \
  || die "Compose configuration could not be rendered"
inventory=$(printf '%s' "$config_json" | python3 /dev/fd/3 "$migrate_service" "$RSCTF_IMAGE" 3<<'PY'
import json
import sys

migrate_service, expected_image = sys.argv[1:]
document = json.load(sys.stdin)
runtime_roles = {"all", "web", "control", "engine", "network"}
services = document.get("services", {})
runtime = []
for name, service in services.items():
    environment = service.get("environment") or {}
    role = str(environment.get("RSCTF_ROLE", "")).strip().lower()
    if role in runtime_roles:
        runtime.append((name, role, service.get("image")))
if not runtime:
    raise SystemExit("Compose configuration has no rsctf runtime-role service")
names = {name for name, _, _ in runtime}
if migrate_service not in names:
    raise SystemExit(
        f"migration service {migrate_service!r} is not one of the runtime services {sorted(names)}"
    )
for name, _, image in runtime:
    if image != expected_image:
        raise SystemExit(
            f"runtime service {name!r} resolves image {image!r}, expected {expected_image!r}"
        )
role_counts = {}
for _, role, _ in runtime:
    role_counts[role] = role_counts.get(role, 0) + 1
if "all" in role_counts:
    if role_counts != {"all": 1}:
        raise SystemExit("runtimeRole=all cannot be mixed with split services")
elif "control" in role_counts:
    if role_counts != {"web": 1, "control": 1}:
        raise SystemExit("control topology requires exactly one web and one control service")
else:
    if role_counts != {"web": 1, "engine": 1, "network": 1}:
        raise SystemExit("engine topology requires exactly one web, engine, and network service")
for name in sorted(services):
    print(f"CONFIG\t{name}\t")
for name, role, _ in sorted(runtime):
    print(f"RUNTIME\t{name}\t{role}")
PY
) || die "Compose runtime inventory is unsafe"

configured_services=()
runtime_services=()
runtime_roles=()
while IFS=$'\t' read -r kind service role; do
  case "$kind" in
    CONFIG) configured_services+=("$service") ;;
    RUNTIME)
      runtime_services+=("$service")
      runtime_roles+=("$role")
      ;;
  esac
done <<<"$inventory"
((${#runtime_services[@]} > 0)) || die "Compose runtime inventory was empty"

live_replicas=()
for service in "${runtime_services[@]}"; do
  id_output=$(docker container ls --all \
    --filter "label=com.docker.compose.project=$project_name" \
    --filter "label=com.docker.compose.service=$service" \
    --filter "label=com.docker.compose.oneoff=False" \
    --format '{{.ID}}') || die "cannot enumerate containers for runtime service $service"
  ids=()
  if [[ -n "$id_output" ]]; then
    mapfile -t ids <<<"$id_output"
  fi
  for id in "${ids[@]}"; do
    [[ "$id" =~ ^[0-9a-f]{12,64}$ ]] || die "Docker returned an invalid container id"
    restart_policy=$(docker container inspect --format '{{.HostConfig.RestartPolicy.Name}}' "$id") \
      || die "cannot inspect restart policy for runtime container $id"
    [[ "$restart_policy" == unless-stopped ]] \
      || die "runtime container $id must use restart: unless-stopped so it cannot auto-resume during maintenance"
  done
  live_replicas+=("${#ids[@]}")
done

runtime_replicas=()
create_state=false
if [[ -e "$state_file" ]]; then
  state_inventory=$(python3 /dev/fd/3 "$state_file" "$RSCTF_IMAGE" \
    "${runtime_services[@]}" 3<<'PY'
import json
import os
import stat
import sys

path, expected_image, *services = sys.argv[1:]
metadata = os.lstat(path)
if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.geteuid() or metadata.st_nlink != 1:
    raise SystemExit("cutover state must be one regular file owned by the current user")
if metadata.st_mode & 0o022:
    raise SystemExit("cutover state must not be writable by group or other users")
with open(path, encoding="utf-8") as handle:
    document = json.load(handle)
if document.get("version") != 1 or document.get("image") != expected_image:
    raise SystemExit("cutover state belongs to another format or image digest")
counts = document.get("services")
if not isinstance(counts, dict) or set(counts) != set(services):
    raise SystemExit("cutover state service set does not match the rendered Compose project")
for service in services:
    count = counts[service]
    if not isinstance(count, int) or isinstance(count, bool) or count < 1:
        raise SystemExit("cutover state has an invalid replica count")
    print(f"{service}\t{count}")
PY
  ) || die "cutover state is invalid; do not guess replica counts"
  while IFS=$'\t' read -r service count; do
    [[ "$service" == "${runtime_services[${#runtime_replicas[@]}]}" ]] \
      || die "cutover state service order is invalid"
    runtime_replicas+=("$count")
  done <<<"$state_inventory"
  for index in "${!runtime_services[@]}"; do
    ((live_replicas[index] <= runtime_replicas[index])) \
      || die "runtime service ${runtime_services[$index]} exceeds its saved replica count"
  done
else
  for index in "${!runtime_services[@]}"; do
    ((live_replicas[index] > 0)) \
      || die "runtime service ${runtime_services[$index]} has no container and no validated cutover state"
    runtime_replicas+=("${live_replicas[$index]}")
  done
  create_state=true
fi

for index in "${!runtime_services[@]}"; do
  case "${runtime_roles[$index]}" in
    all | control | network)
      ((runtime_replicas[index] == 1)) \
        || die "runtimeRole=${runtime_roles[$index]} requires exactly one replica before migration"
      ;;
  esac
done

if [[ "$create_state" == true ]]; then
  umask 077
  state_tmp=$(mktemp "${state_file}.tmp.XXXXXX") \
    || die "cannot create cutover state beside $state_file"
  trap cleanup_state_tmp EXIT
  python3 /dev/fd/3 "$RSCTF_IMAGE" "${runtime_services[@]}" -- \
    "${runtime_replicas[@]}" 3<<'PY' >"$state_tmp" \
    || die "cannot write cutover state"
import json
import sys

image, *arguments = sys.argv[1:]
separator = arguments.index("--")
services = arguments[:separator]
counts = [int(value) for value in arguments[separator + 1:]]
if len(services) != len(counts):
    raise SystemExit("service/count mismatch")
json.dump({"version": 1, "image": image, "services": dict(zip(services, counts))}, sys.stdout)
sys.stdout.write("\n")
PY
  chmod 600 "$state_tmp" || die "cannot protect cutover state"
  mv -- "$state_tmp" "$state_file" || die "cannot install cutover state"
  state_tmp=''
fi

printf 'Stopping all rsctf runtime containers in project %s...\n' "$project_name"
"${compose[@]}" stop --timeout "$timeout" "${runtime_services[@]}" \
  || die "runtime stop failed; migration was not started"

# Stop project-scoped one-off/renamed runtime containers too. They remain
# stopped until migration succeeds so a failed migration is retryable without
# resurrecting the old image merely to rediscover replica counts.
# Every target is resolved and its project label and restart policy are
# re-verified; dependencies and the firewall guard have no runtime role.
project_id_output=$(docker container ls --all \
  --filter "label=com.docker.compose.project=$project_name" --format '{{.ID}}') \
  || die "cannot enumerate remaining project containers"
project_ids=()
if [[ -n "$project_id_output" ]]; then
  mapfile -t project_ids <<<"$project_id_output"
fi
for id in "${project_ids[@]}"; do
  [[ "$id" =~ ^[0-9a-f]{12,64}$ ]] || die "Docker returned an invalid project container id"
  actual_project=$(docker container inspect --format \
    '{{index .Config.Labels "com.docker.compose.project"}}' "$id") \
    || die "cannot verify project ownership for container $id"
  [[ "$actual_project" == "$project_name" ]] \
    || die "container $id changed project ownership during cutover"
  container_service=$(docker container inspect --format \
    '{{index .Config.Labels "com.docker.compose.service"}}' "$id") \
    || die "cannot verify Compose service ownership for container $id"
  environment=$(docker container inspect --format '{{json .Config.Env}}' "$id") \
    || die "cannot inspect environment shape for container $id"
  role=$(printf '%s' "$environment" | python3 /dev/fd/3 3<<'PY'
import json
import sys
role = ""
for value in json.load(sys.stdin) or []:
    if value.startswith("RSCTF_ROLE="):
        role = value.split("=", 1)[1].strip().lower()
print(role)
PY
  )
  if array_contains "$container_service" "${runtime_services[@]}" \
    || [[ "$role" =~ ^(all|web|control|engine|network)$ ]]; then
      restart_policy=$(docker container inspect --format '{{.HostConfig.RestartPolicy.Name}}' "$id") \
        || die "cannot inspect restart policy for runtime container $id"
      [[ "$restart_policy" == unless-stopped ]] \
        || die "runtime container $id must use restart: unless-stopped so it cannot auto-resume during maintenance"
      running=$(docker container inspect --format '{{.State.Running}}' "$id") \
        || die "cannot inspect runtime container $id"
      if [[ "$running" == true ]]; then
        docker container stop --time "$timeout" "$id" >/dev/null \
          || die "could not stop project runtime container $id"
      fi
  elif ! array_contains "$container_service" "${configured_services[@]}"; then
    die "project container $id belongs to unrendered service $container_service; resolve the orphan before migration"
  fi
done

assert_no_running_runtime_containers() {
  local remaining_id_output id environment container_service running
  local -a remaining_ids=()

  remaining_id_output=$(docker container ls --all \
    --filter "label=com.docker.compose.project=$project_name" --format '{{.ID}}') \
    || die "cannot verify the drained Compose project"
  if [[ -n "$remaining_id_output" ]]; then
    mapfile -t remaining_ids <<<"$remaining_id_output"
  fi
  for id in "${remaining_ids[@]}"; do
    [[ "$id" =~ ^[0-9a-f]{12,64}$ ]] || die "Docker returned an invalid project container id"
    environment=$(docker container inspect --format '{{json .Config.Env}}' "$id") \
      || die "cannot verify remaining project container $id"
    container_service=$(docker container inspect --format \
      '{{index .Config.Labels "com.docker.compose.service"}}' "$id") \
      || die "cannot verify remaining Compose service for container $id"
    if array_contains "$container_service" "${runtime_services[@]}" \
      || printf '%s' "$environment" | python3 /dev/fd/3 3<<'PY'
import json
import sys
runtime = {"all", "web", "control", "engine", "network"}
for value in json.load(sys.stdin) or []:
    if value.startswith("RSCTF_ROLE=") and value.split("=", 1)[1].strip().lower() in runtime:
        raise SystemExit(0)
raise SystemExit(1)
PY
    then
      running=$(docker container inspect --format '{{.State.Running}}' "$id") \
        || die "cannot inspect remaining runtime container $id"
      [[ "$running" == false ]] \
        || die "runtime container $id is still running after the stop-the-world drain"
    elif ! array_contains "$container_service" "${configured_services[@]}"; then
      die "project container $id belongs to unrendered service $container_service during quiescence verification"
    fi
  done
}

remove_stopped_runtime_containers() {
  local project_id_output id actual_project container_service environment role running
  local -a project_ids=()

  project_id_output=$(docker container ls --all \
    --filter "label=com.docker.compose.project=$project_name" --format '{{.ID}}') \
    || die "cannot enumerate stopped runtime containers for replacement"
  if [[ -n "$project_id_output" ]]; then
    mapfile -t project_ids <<<"$project_id_output"
  fi
  for id in "${project_ids[@]}"; do
    [[ "$id" =~ ^[0-9a-f]{12,64}$ ]] || die "Docker returned an invalid project container id"
    actual_project=$(docker container inspect --format \
      '{{index .Config.Labels "com.docker.compose.project"}}' "$id") \
      || die "cannot re-verify project ownership for container $id"
    [[ "$actual_project" == "$project_name" ]] \
      || die "container $id changed project ownership during cutover"
    container_service=$(docker container inspect --format \
      '{{index .Config.Labels "com.docker.compose.service"}}' "$id") \
      || die "cannot re-verify Compose service for container $id"
    environment=$(docker container inspect --format '{{json .Config.Env}}' "$id") \
      || die "cannot inspect stopped project container $id"
    role=$(printf '%s' "$environment" | python3 /dev/fd/3 3<<'PY'
import json
import sys
role = ""
for value in json.load(sys.stdin) or []:
    if value.startswith("RSCTF_ROLE="):
        role = value.split("=", 1)[1].strip().lower()
print(role)
PY
    )
    if array_contains "$container_service" "${runtime_services[@]}" \
      || [[ "$role" =~ ^(all|web|control|engine|network)$ ]]; then
        running=$(docker container inspect --format '{{.State.Running}}' "$id") \
          || die "cannot inspect stopped runtime container $id"
        [[ "$running" == false ]] \
          || die "runtime container $id resumed before old-image removal"
        docker container rm "$id" >/dev/null \
          || die "could not remove stopped old runtime container $id"
    elif ! array_contains "$container_service" "${configured_services[@]}"; then
      die "project container $id belongs to unrendered service $container_service before old-image removal"
    fi
  done
}

assert_no_running_runtime_containers

printf 'Old runtime containers are stopped; running the immutable migration image...\n'
"${compose[@]}" run --rm --no-deps \
  -e RSCTF_ROLE=migrate -e RSCTF_MIGRATE=1 "$migrate_service" \
  || die "migration failed; old runtime containers remain stopped and must not be restored"

# The database preflight is point-in-time. Detect an external reconciler that
# recreated an old runtime while the migration container was running.
assert_no_running_runtime_containers

# Replica counts now live in the validated state file. After migration success,
# remove every stopped old binary before starting a process against the new
# schema. If startup fails, the state file makes the new-image retry possible.
remove_stopped_runtime_containers

scale_args=()
for index in "${!runtime_services[@]}"; do
  scale_args+=(--scale "${runtime_services[$index]}=${runtime_replicas[$index]}")
done
"${compose[@]}" up --detach --force-recreate --wait --wait-timeout "$timeout" \
  "${scale_args[@]}" "${runtime_services[@]}" \
  || die "new runtime startup failed; do not restore the old image after migration"

new_runtime_ids=()
for service in "${runtime_services[@]}"; do
  id_output=$(docker container ls \
    --filter "label=com.docker.compose.project=$project_name" \
    --filter "label=com.docker.compose.service=$service" \
    --filter "label=com.docker.compose.oneoff=False" \
    --format '{{.ID}}') || die "cannot enumerate new runtime service $service"
  ids=()
  if [[ -n "$id_output" ]]; then
    mapfile -t ids <<<"$id_output"
  fi
  expected_replicas=''
  for index in "${!runtime_services[@]}"; do
    if [[ "${runtime_services[$index]}" == "$service" ]]; then
      expected_replicas=${runtime_replicas[$index]}
      break
    fi
  done
  [[ -n "$expected_replicas" && ${#ids[@]} -eq "$expected_replicas" ]] \
    || die "runtime service $service did not restore its captured replica count"
  for id in "${ids[@]}"; do
    image=$(docker container inspect --format '{{.Config.Image}}' "$id") \
      || die "cannot verify new runtime container $id"
    [[ "$image" == "$RSCTF_IMAGE" ]] \
      || die "runtime container $id does not use the expected immutable image"
    new_runtime_ids+=("$id")
  done
done

# Every old runtime was removed before startup. Treat any additional runtime as
# a reconciler/race rather than silently adopting or deleting it.
project_id_output=$(docker container ls --all \
  --filter "label=com.docker.compose.project=$project_name" --format '{{.ID}}') \
  || die "cannot perform the final project runtime audit"
project_ids=()
if [[ -n "$project_id_output" ]]; then
  mapfile -t project_ids <<<"$project_id_output"
fi
for id in "${project_ids[@]}"; do
  if printf '%s\n' "${new_runtime_ids[@]}" | grep -Fxq -- "$id"; then
    continue
  fi
  environment=$(docker container inspect --format '{{json .Config.Env}}' "$id") \
    || die "cannot inspect final project container $id"
  container_service=$(docker container inspect --format \
    '{{index .Config.Labels "com.docker.compose.service"}}' "$id") \
    || die "cannot inspect final Compose service for container $id"
  role=$(printf '%s' "$environment" | python3 /dev/fd/3 3<<'PY'
import json
import sys
role = ""
for value in json.load(sys.stdin) or []:
    if value.startswith("RSCTF_ROLE="):
        role = value.split("=", 1)[1].strip().lower()
print(role)
PY
  )
  if array_contains "$container_service" "${runtime_services[@]}" \
    || [[ "$role" =~ ^(all|web|control|engine|network)$ ]]; then
    die "unexpected extra runtime container $id appeared during new runtime startup"
  elif ! array_contains "$container_service" "${configured_services[@]}"; then
    die "unexpected unrendered project service $container_service appeared during startup"
  fi
done

[[ ! -L "$state_file" && -f "$state_file" ]] \
  || die "cutover state changed type before cleanup"
rm -- "$state_file" || die "could not remove completed cutover state"
printf 'Compose maintenance cutover completed at immutable image %s.\n' "$RSCTF_IMAGE"
