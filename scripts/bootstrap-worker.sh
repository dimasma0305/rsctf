#!/bin/sh

set -eu

PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
export PATH
umask 077

REPOSITORY="dimasma0305/rsctf"
STATE_DIRECTORY="/var/lib/rsctf-worker"
READY_FILE="/run/rsctf-worker-agent/connected"
CONTAINER_READY_FILE="${STATE_DIRECTORY}/connected"
CONTAINER_NAME="rsctf-worker-agent"
CONTAINER_STATE_VOLUME="rsctf-worker-state"
CONNECTION_TIMEOUT_SECONDS="${RSCTF_WORKER_CONNECTION_TIMEOUT_SECONDS:-45}"
SERVER_URL=""
VERSION=""
SERVICE_MODE="auto"
SERVICE_MODE_SET=false
ALLOW_UNBOUNDED_STORAGE=false
TEMP_DIRECTORY=""
ENROLLMENT_TOKEN=""
TERMINAL_SETTINGS=""
EXISTING_ENROLLMENT=false
UNINSTALL=false
CONTAINER_IMAGE=""
DOCKER_ROOT=""
DOCKER_SOCKET="${RSCTF_WORKER_DOCKER_SOCKET:-/var/run/docker.sock}"

usage() {
  cat <<'EOF'
Install, update, or uninstall an RSCTF Linux worker.

Usage:
  bootstrap-worker.sh --server-url https://ctf.example [--version vX.Y.Z]
                      [--service-mode auto|systemd|docker]
                      [--allow-unbounded-storage]
  bootstrap-worker.sh --uninstall

This script runs with a POSIX sh and supports both GNU wget and BusyBox wget.
The enrollment token is read privately from the controlling terminal. It is
never accepted in a URL, command-line argument, or environment variable.

Auto mode uses a native systemd service when available. Otherwise Docker
supervises the agent container and stores its identity in a durable named
volume, so the host does not need an init system. If an installed Docker daemon
is stopped, the bootstrap starts and enables it through systemd or OpenRC, or
starts it through a compatible service or runit manager.

--allow-unbounded-storage is a development-only escape hatch for Docker engines
that cannot enforce per-workload writable-layer quotas. Never use it for an
adversarial event or on a host containing unrelated data.

Uninstall refuses to continue while RSCTF-managed workloads exist, then removes
the local service or supervised container, identity, binary/image, and dedicated
service account where applicable. Disable the worker in RSCTF Admin first.
EOF
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

restore_terminal() {
  if [ -n "$TERMINAL_SETTINGS" ]; then
    stty "$TERMINAL_SETTINGS" </dev/tty 2>/dev/null || true
    TERMINAL_SETTINGS=""
  fi
}

cleanup() {
  cleanup_status=$?
  ENROLLMENT_TOKEN=""
  restore_terminal
  if [ -n "$TEMP_DIRECTORY" ] && [ -d "$TEMP_DIRECTORY" ]; then
    rm -rf "$TEMP_DIRECTORY"
  fi
  return "$cleanup_status"
}

trap cleanup 0
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "required command is missing: $1"
}

is_unsigned_integer() {
  case "$1" in
    "" | *[!0-9]*)
      return 1
      ;;
    *)
      return 0
      ;;
  esac
}

is_release_version() {
  printf '%s\n' "$1" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$'
}

validate_https_headers() {
  response_headers=$1
  response_url=$2

  awk '
    tolower($1) == "location:" {
      gsub(/\r/, "", $2)
      if (tolower($2) !~ /^https:\/\//) {
        exit 1
      }
    }
  ' "$response_headers" ||
    die "download redirected away from HTTPS: ${response_url}"
}

download() {
  download_url=$1
  download_destination=$2
  download_maximum_bytes=$3
  download_headers="${download_destination}.headers"

  case "$download_url" in
    https://*) ;;
    *) die "refusing a non-HTTPS download URL: ${download_url}" ;;
  esac

  rm -f "$download_destination" "$download_headers"
  # POSIX ulimit uses 512-byte blocks. Allow one MiB for response headers and
  # rounding, then enforce the exact body limit after wget exits.
  download_limit_blocks=$(((download_maximum_bytes + 1048576 + 511) / 512))
  if ! (
    ulimit -f "$download_limit_blocks" 2>/dev/null || :
    exec wget -q -S -T 30 -O "$download_destination" "$download_url"
  ) 2>"$download_headers"; then
    rm -f "$download_destination"
    die "download failed: ${download_url}"
  fi

  validate_https_headers "$download_headers" "$download_url"
  download_size=$(wc -c <"$download_destination")
  if [ "$download_size" -gt "$download_maximum_bytes" ]; then
    rm -f "$download_destination"
    die "download exceeds ${download_maximum_bytes} bytes: ${download_url}"
  fi
}

resolve_latest_release() {
  latest_release_url=$1
  latest_headers="${TEMP_DIRECTORY}/latest.headers"

  case "$latest_release_url" in
    https://*) ;;
    *) die "refusing a non-HTTPS release URL" ;;
  esac
  if ! wget -q -S -T 30 --spider "$latest_release_url" 2>"$latest_headers"; then
    die "could not resolve the latest RSCTF release"
  fi
  validate_https_headers "$latest_headers" "$latest_release_url"
  awk '
    tolower($1) == "location:" {
      gsub(/\r/, "", $2)
      location = $2
    }
    END { print location }
  ' "$latest_headers"
}

checksum_for() {
  checksum_file=$1
  checksum_name=$2

  awk -v expected_name="$checksum_name" '
    NF == 2 &&
    length($1) == 64 &&
    $1 ~ /^[0-9A-Fa-f]+$/ &&
    $2 == expected_name {
      matches += 1
      hash = tolower($1)
    }
    END {
      printf "%d:%s\n", matches, hash
    }
  ' "$checksum_file"
}

ensure_docker_daemon() {
  if docker info >/dev/null 2>&1; then
    return 0
  fi

  printf 'Docker daemon is unavailable; attempting to start the installed service.\n'
  docker_start_method=""

  if [ -d /run/systemd/system ] &&
    command -v systemctl >/dev/null 2>&1 &&
    systemctl show --property=Version --value >/dev/null 2>&1; then
    if systemctl start docker.service; then
      docker_start_method=systemd
      systemctl enable docker.service >/dev/null 2>&1 ||
        printf 'WARNING: systemd could not enable Docker at boot.\n' >&2
    fi
  fi

  if [ -z "$docker_start_method" ] &&
    command -v rc-service >/dev/null 2>&1; then
    if rc-service docker start; then
      docker_start_method=OpenRC
      if command -v rc-update >/dev/null 2>&1; then
        rc-update add docker default >/dev/null 2>&1 ||
          printf 'WARNING: OpenRC could not enable Docker at boot.\n' >&2
      fi
    fi
  fi

  if [ -z "$docker_start_method" ] &&
    command -v service >/dev/null 2>&1 &&
    service docker start; then
    docker_start_method=service
  fi

  if [ -z "$docker_start_method" ] &&
    command -v sv >/dev/null 2>&1 &&
    sv up docker; then
    docker_start_method=runit
  fi

  [ -n "$docker_start_method" ] ||
    die "Docker is installed but its daemon could not be started automatically; start Docker with this host's service manager, then rerun the installer"

  docker_start_wait=0
  while [ "$docker_start_wait" -lt 30 ]; do
    if docker info >/dev/null 2>&1; then
      printf 'Docker daemon started through %s and passed its readiness check.\n' \
        "$docker_start_method"
      return 0
    fi
    sleep 1
    docker_start_wait=$((docker_start_wait + 1))
  done

  die "Docker service startup through ${docker_start_method} completed, but the daemon did not become ready within 30 seconds; inspect the Docker service logs and rerun the installer"
}

uninstall_worker() {
  [ -r /dev/tty ] && [ -w /dev/tty ] ||
    die "an interactive terminal is required for uninstall confirmation"
  for required_command in docker rm; do
    require_command "$required_command"
  done
  ensure_docker_daemon
  managed_containers=$(docker ps --all --quiet \
    --filter label=io.rsctf.worker.managed=true)
  managed_networks=$(docker network ls --quiet \
    --filter label=io.rsctf.worker.managed=true)
  if [ -n "$managed_containers" ] || [ -n "$managed_networks" ]; then
    die "RSCTF-managed containers or networks still exist; drain this worker and remove its workloads before uninstalling"
  fi

  supervised_container_exists=false
  if docker container inspect "$CONTAINER_NAME" >/dev/null 2>&1; then
    supervised_container_exists=true
    supervised_label=$(docker container inspect --format \
      '{{ index .Config.Labels "io.rsctf.worker.agent" }}' \
      "$CONTAINER_NAME") ||
      die "could not inspect the existing ${CONTAINER_NAME} container"
    [ "$supervised_label" = true ] ||
      die "a container named ${CONTAINER_NAME} exists without the RSCTF agent label; refusing to remove it"
  fi

  state_volume_exists=false
  if docker volume inspect "$CONTAINER_STATE_VOLUME" >/dev/null 2>&1; then
    state_volume_exists=true
    state_volume_label=$(docker volume inspect --format \
      '{{ index .Labels "io.rsctf.worker.state" }}' \
      "$CONTAINER_STATE_VOLUME") ||
      die "could not inspect the existing ${CONTAINER_STATE_VOLUME} volume"
    [ "$state_volume_label" = true ] ||
      die "a volume named ${CONTAINER_STATE_VOLUME} exists without the RSCTF state label; refusing to remove it"
  fi

  if [ -L "$STATE_DIRECTORY" ] ||
    { [ -e "$STATE_DIRECTORY" ] && [ ! -d "$STATE_DIRECTORY" ]; }; then
    die "${STATE_DIRECTORY} is not a real directory; refusing recursive removal"
  fi
  if [ -L "/usr/local/share/doc/rsctf-worker-agent" ] ||
    { [ -e "/usr/local/share/doc/rsctf-worker-agent" ] &&
      [ ! -d "/usr/local/share/doc/rsctf-worker-agent" ]; }; then
    die "the worker documentation path is not a real directory; refusing recursive removal"
  fi

  printf '%s\n' \
    'This permanently deletes the local worker certificate and configuration.' \
    'Disable this worker in RSCTF Admin first so its certificate is rejected.' \
    >/dev/tty
  printf 'Type REMOVE to uninstall this worker: ' >/dev/tty
  IFS= read -r confirmation </dev/tty
  [ "$confirmation" = "REMOVE" ] || die "worker uninstall was cancelled"
  confirmation=""

  if [ "$supervised_container_exists" = true ]; then
    docker container rm --force "$CONTAINER_NAME" >/dev/null
  fi
  if [ "$state_volume_exists" = true ]; then
    docker volume rm "$CONTAINER_STATE_VOLUME" >/dev/null
  fi

  if command -v systemctl >/dev/null 2>&1; then
    systemctl disable --now rsctf-worker-agent.service >/dev/null 2>&1 || true
  fi
  rm -f /etc/systemd/system/rsctf-worker-agent.service \
    /usr/local/bin/rsctf-worker-agent
  if [ -d /usr/local/share/doc/rsctf-worker-agent ]; then
    rm -rf /usr/local/share/doc/rsctf-worker-agent
  fi
  if [ -d "$STATE_DIRECTORY" ]; then
    rm -rf "$STATE_DIRECTORY"
  fi
  if command -v systemctl >/dev/null 2>&1; then
    systemctl daemon-reload >/dev/null 2>&1 || true
    systemctl reset-failed rsctf-worker-agent.service >/dev/null 2>&1 || true
  fi

  owner_label=$(docker volume inspect --format \
    '{{ index .Labels "io.rsctf.worker.daemon-owner" }}' \
    rsctf-worker-owner 2>/dev/null || true)
  if [ -n "$owner_label" ]; then
    docker volume rm rsctf-worker-owner >/dev/null
  fi

  worker_record=""
  if command -v getent >/dev/null 2>&1; then
    worker_record=$(getent passwd rsctf-worker 2>/dev/null || true)
  fi
  if [ -n "$worker_record" ]; then
    IFS=: read -r worker_name _ worker_uid _ _ worker_home worker_shell <<EOF
$worker_record
EOF
    worker_identity_safe=false
    if [ "$worker_name" = "rsctf-worker" ] &&
      is_unsigned_integer "$worker_uid" &&
      [ "$worker_uid" -ne 0 ] &&
      [ "$worker_home" = "$STATE_DIRECTORY" ]; then
      case "$worker_shell" in
        */nologin) worker_identity_safe=true ;;
      esac
    fi
    if [ "$worker_identity_safe" = true ] &&
      command -v userdel >/dev/null 2>&1; then
      userdel rsctf-worker
    else
      printf 'WARNING: retained unexpected rsctf-worker account; inspect it manually.\n' >&2
    fi
  fi
  if command -v getent >/dev/null 2>&1 &&
    getent group rsctf-worker >/dev/null 2>&1 &&
    command -v groupdel >/dev/null 2>&1; then
    groupdel rsctf-worker 2>/dev/null ||
      printf 'WARNING: retained non-empty rsctf-worker group; inspect it manually.\n' >&2
  fi

  worker_images=$(docker image ls --quiet \
    --filter label=io.rsctf.worker.agent.image=true)
  for worker_image in $worker_images; do
    docker image rm "$worker_image" >/dev/null 2>&1 || true
  done

  printf 'RSCTF worker software and local identity were removed.\n'
  printf 'The Admin worker record is retained for audit history; keep it Disabled.\n'
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --server-url)
      [ "$#" -ge 2 ] || die "--server-url requires a value"
      [ -z "$SERVER_URL" ] || die "--server-url may only be specified once"
      SERVER_URL=${2%/}
      shift 2
      ;;
    --version)
      [ "$#" -ge 2 ] || die "--version requires a value"
      [ -z "$VERSION" ] || die "--version may only be specified once"
      VERSION=$2
      shift 2
      ;;
    --service-mode)
      [ "$#" -ge 2 ] || die "--service-mode requires a value"
      [ "$SERVICE_MODE_SET" = false ] ||
        die "--service-mode may only be specified once"
      SERVICE_MODE=$2
      SERVICE_MODE_SET=true
      shift 2
      ;;
    --allow-unbounded-storage)
      [ "$ALLOW_UNBOUNDED_STORAGE" = false ] ||
        die "--allow-unbounded-storage may only be specified once"
      ALLOW_UNBOUNDED_STORAGE=true
      shift
      ;;
    --uninstall)
      [ "$UNINSTALL" = false ] || die "--uninstall may only be specified once"
      UNINSTALL=true
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: $1 (use --help for usage)"
      ;;
  esac
done

if [ "$UNINSTALL" = true ]; then
  [ -z "$SERVER_URL" ] &&
    [ -z "$VERSION" ] &&
    [ "$SERVICE_MODE_SET" = false ] &&
    [ "$ALLOW_UNBOUNDED_STORAGE" = false ] ||
    die "--uninstall cannot be combined with install options"
fi

if [ "$(id -u)" -ne 0 ]; then
  [ -f "$0" ] ||
    die "save this bootstrap to a file before running it as a non-root user"
  if [ "$UNINSTALL" = true ]; then
    set -- --uninstall
  else
    set -- --server-url "$SERVER_URL"
    if [ -n "$VERSION" ]; then
      set -- "$@" --version "$VERSION"
    fi
    if [ "$SERVICE_MODE_SET" = true ]; then
      set -- "$@" --service-mode "$SERVICE_MODE"
    fi
    if [ "$ALLOW_UNBOUNDED_STORAGE" = true ]; then
      set -- "$@" --allow-unbounded-storage
    fi
  fi
  if command -v sudo >/dev/null 2>&1; then
    exec sudo sh "$0" "$@"
  fi
  if command -v doas >/dev/null 2>&1; then
    exec doas sh "$0" "$@"
  fi
  die "root, sudo, or doas is required to install the worker"
fi

[ "$(uname -s)" = "Linux" ] ||
  die "the worker runtime requires a dedicated Linux host or VM"
if [ "$UNINSTALL" = true ]; then
  uninstall_worker
  exit 0
fi

printf '%s\n' "$SERVER_URL" |
  grep -Eq '^https://([A-Za-z0-9-]+\.)*[A-Za-z0-9-]+(:[0-9]{1,5})?$' ||
  die "--server-url must be an HTTPS origin without a path, query, or credentials"
[ -z "$VERSION" ] || is_release_version "$VERSION" ||
  die "--version must have the form vX.Y.Z"
is_unsigned_integer "$CONNECTION_TIMEOUT_SECONDS" &&
  [ "$CONNECTION_TIMEOUT_SECONDS" -ge 1 ] &&
  [ "$CONNECTION_TIMEOUT_SECONDS" -le 300 ] ||
  die "RSCTF_WORKER_CONNECTION_TIMEOUT_SECONDS must be 1..300"

for required_command in awk cat grep mktemp rm sha256sum sleep uname wc wget; do
  require_command "$required_command"
done

TEMP_DIRECTORY=$(mktemp -d /tmp/rsctf-worker-bootstrap.XXXXXX)
HEALTH_RESPONSE="${TEMP_DIRECTORY}/healthz"
download "${SERVER_URL}/healthz" "$HEALTH_RESPONSE" 16
server_health_size=$(wc -c <"$HEALTH_RESPONSE")
server_health=$(cat "$HEALTH_RESPONSE")
[ "$server_health_size" -eq 2 ] && [ "$server_health" = "ok" ] ||
  die "RSCTF health check returned an unexpected response; installation did not start"
printf 'Verified RSCTF server health at %s/healthz.\n' "$SERVER_URL"

case "$SERVICE_MODE" in
  auto)
    if [ -d /run/systemd/system ] &&
      command -v systemctl >/dev/null 2>&1 &&
      systemctl show --property=Version --value >/dev/null 2>&1; then
      SERVICE_MODE=systemd
    else
      SERVICE_MODE=docker
    fi
    ;;
  systemd | docker) ;;
  *) die "--service-mode must be auto, systemd, or docker" ;;
esac
if [ "$SERVICE_MODE" = systemd ] && [ ! -d /run/systemd/system ]; then
  die "systemd service mode was requested but systemd is not active"
fi
if [ "$SERVICE_MODE" = systemd ] &&
  [ "$ALLOW_UNBOUNDED_STORAGE" = true ]; then
  die "--allow-unbounded-storage is supported only by Docker-supervised mode; configure quota-capable storage for a systemd worker"
fi

require_command docker
require_command stty
if [ "$SERVICE_MODE" = systemd ]; then
  for required_command in journalctl runuser systemctl; do
    require_command "$required_command"
  done
  systemctl show --property=Version --value >/dev/null 2>&1 ||
    die "systemd service mode was requested but its manager is unavailable"
fi
ensure_docker_daemon
printf 'Selected %s worker service mode.\n' "$SERVICE_MODE"

RELEASE_BASE="https://github.com/${REPOSITORY}/releases"
if [ -z "$VERSION" ]; then
  latest_url=$(resolve_latest_release "${RELEASE_BASE}/latest")
  latest_prefix="${RELEASE_BASE}/tag/"
  case "$latest_url" in
    "${latest_prefix}"*) VERSION=${latest_url#"$latest_prefix"} ;;
    *) die "the latest release redirected to an unexpected URL" ;;
  esac
  is_release_version "$VERSION" ||
    die "the latest release does not use a vX.Y.Z tag"
fi

INSTALLER="${TEMP_DIRECTORY}/install-worker.sh"
CHECKSUMS="${TEMP_DIRECTORY}/SHA256SUMS"
DOWNLOAD_BASE="${RELEASE_BASE}/download/${VERSION}"

download "${DOWNLOAD_BASE}/install-worker.sh" "$INSTALLER" 1048576
download "${DOWNLOAD_BASE}/SHA256SUMS" "$CHECKSUMS" 1048576

installer_checksum=$(checksum_for "$CHECKSUMS" "install-worker.sh")
installer_checksum_matches=${installer_checksum%%:*}
expected_installer_hash=${installer_checksum#*:}
[ "$installer_checksum_matches" -eq 1 ] ||
  die "SHA256SUMS must contain exactly one checksum for install-worker.sh"
actual_installer_hash=$(sha256sum "$INSTALLER" | awk '{ print tolower($1) }')
[ "$actual_installer_hash" = "$expected_installer_hash" ] ||
  die "SHA-256 verification failed for install-worker.sh"

# The release installer verifies the worker archive against the matching
# SHA256SUMS asset. Keep this public one-line flow independent of GitHub CLI
# versions; operators who need provenance verification can use the installer's
# explicit attestation mode after downloading it separately.
sh "$INSTALLER" \
  --version "$VERSION" \
  --service-mode "$SERVICE_MODE" \
  --skip-attestation \
  --bootstrap

inspect_systemd_identity() {
  identity_file_count=0
  for identity_file in worker-key.pem worker-cert.pem worker-ca.pem worker.json; do
    if [ -e "${STATE_DIRECTORY}/${identity_file}" ] ||
      [ -L "${STATE_DIRECTORY}/${identity_file}" ]; then
      identity_file_count=$((identity_file_count + 1))
    fi
  done
  if [ "$identity_file_count" -eq 4 ]; then
    EXISTING_ENROLLMENT=true
  elif [ "$identity_file_count" -ne 0 ]; then
    die "the worker state directory contains an incomplete identity; revoke that worker and clean the state deliberately before enrolling again"
  fi
}

show_systemd_diagnostics() {
  printf '\nRecent worker service diagnostics:\n' >&2
  systemctl --no-pager --full status rsctf-worker-agent.service >&2 || true
  journalctl --no-pager --unit rsctf-worker-agent.service --lines 30 >&2 || true
}

assert_systemd_worker_online() {
  elapsed_seconds=0
  stable_seconds=0

  printf 'Waiting up to %s seconds for the authenticated worker control session...\n' \
    "$CONNECTION_TIMEOUT_SECONDS"
  while [ "$elapsed_seconds" -lt "$CONNECTION_TIMEOUT_SECONDS" ]; do
    if ! systemctl is-active --quiet rsctf-worker-agent.service; then
      show_systemd_diagnostics
      die "worker health check failed: the service stopped before connecting to RSCTF"
    fi
    if [ -f "$READY_FILE" ]; then
      stable_seconds=$((stable_seconds + 1))
      if [ "$stable_seconds" -ge 4 ]; then
        printf 'Worker health check passed: mTLS control session accepted by RSCTF.\n'
        return 0
      fi
    else
      stable_seconds=0
    fi
    sleep 1
    elapsed_seconds=$((elapsed_seconds + 1))
  done

  show_systemd_diagnostics
  die "worker health check timed out: the service did not establish a stable mTLS control session; the worker remains installed but offline"
}

start_and_check_systemd_worker() {
  rm -f "$READY_FILE"
  systemctl enable rsctf-worker-agent.service >/dev/null
  systemctl reset-failed rsctf-worker-agent.service >/dev/null 2>&1 || true
  systemctl restart rsctf-worker-agent.service
  assert_systemd_worker_online
  systemctl --no-pager --full status rsctf-worker-agent.service
}

validate_docker_path() {
  docker_path=$1
  docker_path_name=$2
  case "$docker_path" in
    /*) ;;
    *) die "${docker_path_name} must be an absolute path" ;;
  esac
  case "$docker_path" in
    / | *,*) die "${docker_path_name} is unsafe for a Docker bind mount" ;;
  esac
}

prepare_docker_supervisor() {
  CONTAINER_IMAGE="rsctf-worker-agent-local:${VERSION#v}"
  docker_operating_system=$(docker info --format '{{.OSType}}') ||
    die "could not determine the Docker daemon operating system"
  [ "$docker_operating_system" = linux ] ||
    die "Docker-supervised Linux mode requires a Linux-container daemon"

  DOCKER_ROOT=$(docker info --format '{{.DockerRootDir}}') ||
    die "could not determine the Docker data root"
  [ -n "$DOCKER_ROOT" ] ||
    die "Docker did not report its data root"
  validate_docker_path "$DOCKER_ROOT" "Docker data root"
  validate_docker_path "$DOCKER_SOCKET" "Docker socket"

  if docker container inspect "$CONTAINER_NAME" >/dev/null 2>&1; then
    agent_label=$(docker container inspect --format \
      '{{ index .Config.Labels "io.rsctf.worker.agent" }}' \
      "$CONTAINER_NAME") ||
      die "could not inspect the existing ${CONTAINER_NAME} container"
    [ "$agent_label" = true ] ||
      die "a container named ${CONTAINER_NAME} exists without the RSCTF agent label; choose another Docker host or remove the collision deliberately"
  fi

  if docker volume inspect "$CONTAINER_STATE_VOLUME" >/dev/null 2>&1; then
    state_label=$(docker volume inspect --format \
      '{{ index .Labels "io.rsctf.worker.state" }}' \
      "$CONTAINER_STATE_VOLUME") ||
      die "could not inspect the existing worker state volume"
    [ "$state_label" = true ] ||
      die "a volume named ${CONTAINER_STATE_VOLUME} exists without the RSCTF state label; refusing to use it"
  else
    created_volume=$(docker volume create \
      --label io.rsctf.worker.state=true \
      "$CONTAINER_STATE_VOLUME") ||
      die "could not create the durable worker state volume"
    [ "$created_volume" = "$CONTAINER_STATE_VOLUME" ] ||
      die "Docker returned an unexpected worker state volume name"
  fi

  installation_status=$(
    docker run --rm \
      --network none \
      --read-only \
      --cap-drop ALL \
      --security-opt no-new-privileges:true \
      --pids-limit 64 \
      --mount "type=volume,src=${CONTAINER_STATE_VOLUME},dst=${STATE_DIRECTORY}" \
      "$CONTAINER_IMAGE" \
      installation-status --state-dir "$STATE_DIRECTORY"
  ) || die "could not safely inspect the Docker-supervised worker identity"
  case "$installation_status" in
    empty) EXISTING_ENROLLMENT=false ;;
    enrolled) EXISTING_ENROLLMENT=true ;;
    *) die "the worker image returned an unexpected identity status" ;;
  esac
}

run_docker_doctor() {
  docker run --rm \
    --network host \
    --read-only \
    --cap-drop ALL \
    --security-opt no-new-privileges:true \
    --pids-limit 128 \
    --tmpfs /tmp:rw,noexec,nosuid,nodev,size=67108864 \
    --mount "type=bind,src=${DOCKER_SOCKET},dst=/var/run/docker.sock" \
    --mount "type=bind,src=${DOCKER_ROOT},dst=${DOCKER_ROOT},readonly" \
    --env RSCTF_WORKER_DOCKER_ENDPOINT=/var/run/docker.sock \
    --env "RSCTF_WORKER_ALLOW_UNBOUNDED_STORAGE=${ALLOW_UNBOUNDED_STORAGE}" \
    "$CONTAINER_IMAGE" doctor
}

enroll_docker_worker() {
  printf '%s\n' "$ENROLLMENT_TOKEN" |
    docker run --rm --interactive \
      --network host \
      --read-only \
      --cap-drop ALL \
      --security-opt no-new-privileges:true \
      --pids-limit 128 \
      --tmpfs /tmp:rw,noexec,nosuid,nodev,size=67108864 \
      --mount "type=volume,src=${CONTAINER_STATE_VOLUME},dst=${STATE_DIRECTORY}" \
      "$CONTAINER_IMAGE" enroll \
      --server-url "$SERVER_URL" \
      --token-stdin \
      --state-dir "$STATE_DIRECTORY"
}

show_docker_diagnostics() {
  printf '\nRecent Docker-supervised worker diagnostics:\n' >&2
  docker container inspect --format \
    'name={{.Name}} running={{.State.Running}} status={{.State.Status}} restartCount={{.RestartCount}} image={{.Image}}' \
    "$CONTAINER_NAME" >&2 || true
  docker logs --tail 30 "$CONTAINER_NAME" >&2 || true
}

assert_docker_worker_online() {
  elapsed_seconds=0
  stable_seconds=0
  copied_ready="${TEMP_DIRECTORY}/docker-worker-connected"

  printf 'Waiting up to %s seconds for the authenticated worker control session...\n' \
    "$CONNECTION_TIMEOUT_SECONDS"
  while [ "$elapsed_seconds" -lt "$CONNECTION_TIMEOUT_SECONDS" ]; do
    container_running=$(docker container inspect --format \
      '{{.State.Running}}' "$CONTAINER_NAME" 2>/dev/null || true)
    if [ "$container_running" != true ]; then
      show_docker_diagnostics
      printf 'error: worker health check failed: the supervised container stopped before connecting to RSCTF\n' >&2
      return 1
    fi

    rm -f "$copied_ready"
    if docker cp \
      "${CONTAINER_NAME}:${CONTAINER_READY_FILE}" \
      "$copied_ready" >/dev/null 2>&1 &&
      [ -f "$copied_ready" ]; then
      stable_seconds=$((stable_seconds + 1))
      if [ "$stable_seconds" -ge 4 ]; then
        printf 'Worker health check passed: mTLS control session accepted by RSCTF.\n'
        return 0
      fi
    else
      stable_seconds=0
    fi
    sleep 1
    elapsed_seconds=$((elapsed_seconds + 1))
  done

  show_docker_diagnostics
  printf 'error: worker health check timed out: the supervised container did not establish a stable mTLS control session\n' >&2
  return 1
}

restore_previous_docker_worker() {
  rollback_container=$1
  previous_was_running=$2

  docker container rm --force "$CONTAINER_NAME" >/dev/null 2>&1 || true
  if ! docker rename "$rollback_container" "$CONTAINER_NAME"; then
    return 1
  fi
  if [ "$previous_was_running" = true ]; then
    docker start "$CONTAINER_NAME" >/dev/null || return 1
  fi
}

prune_old_worker_images() {
  current_image_id=$(docker image inspect --format '{{.Id}}' "$CONTAINER_IMAGE") ||
    return 1
  seen_image_ids=""
  old_image_ids=$(docker image ls --all --quiet \
    --filter label=io.rsctf.worker.agent.image=true) ||
    return 1
  for old_image_id in $old_image_ids; do
    case " $seen_image_ids " in
      *" $old_image_id "*) continue ;;
    esac
    seen_image_ids="${seen_image_ids} ${old_image_id}"
    [ "$old_image_id" = "$current_image_id" ] && continue
    docker image rm "$old_image_id" >/dev/null 2>&1 ||
      printf 'WARNING: retained an older worker image still in use: %s\n' \
        "$old_image_id" >&2
  done
}

start_and_check_docker_worker() {
  rollback_container=""
  previous_was_running=false

  if docker container inspect "$CONTAINER_NAME" >/dev/null 2>&1; then
    rollback_container="${CONTAINER_NAME}-rollback-$$"
    if docker container inspect "$rollback_container" >/dev/null 2>&1; then
      die "a stale Docker worker rollback container exists: ${rollback_container}"
    fi
    previous_was_running=$(docker container inspect --format \
      '{{.State.Running}}' "$CONTAINER_NAME") ||
      die "could not inspect the existing worker container"
    docker stop --time 30 "$CONTAINER_NAME" >/dev/null ||
      die "could not stop the existing worker container"
    docker rename "$CONTAINER_NAME" "$rollback_container" ||
      die "could not stage the existing worker container for rollback"
  fi

  if ! docker run --detach \
    --name "$CONTAINER_NAME" \
    --restart unless-stopped \
    --stop-timeout 30 \
    --network host \
    --read-only \
    --cap-drop ALL \
    --security-opt no-new-privileges:true \
    --pids-limit 256 \
    --tmpfs /tmp:rw,noexec,nosuid,nodev,size=67108864 \
    --log-driver local \
    --log-opt max-size=10m \
    --log-opt max-file=3 \
    --label io.rsctf.worker.agent=true \
    --label "io.rsctf.worker.agent.version=${VERSION#v}" \
    --mount "type=volume,src=${CONTAINER_STATE_VOLUME},dst=${STATE_DIRECTORY}" \
    --mount "type=bind,src=${DOCKER_SOCKET},dst=/var/run/docker.sock" \
    --mount "type=bind,src=${DOCKER_ROOT},dst=${DOCKER_ROOT},readonly" \
    --env RSCTF_WORKER_DOCKER_ENDPOINT=/var/run/docker.sock \
    --env "RSCTF_WORKER_ALLOW_UNBOUNDED_STORAGE=${ALLOW_UNBOUNDED_STORAGE}" \
    "$CONTAINER_IMAGE" run \
    --config "${STATE_DIRECTORY}/worker.json" \
    --ready-file "$CONTAINER_READY_FILE" \
    --accept-host-network-boundary \
    >"${TEMP_DIRECTORY}/worker-container-id"; then
    if [ -n "$rollback_container" ]; then
      if restore_previous_docker_worker \
        "$rollback_container" "$previous_was_running"; then
        die "Docker could not start the updated worker; the previous container was restored"
      fi
      die "Docker could not start the updated worker and rollback requires administrator recovery"
    fi
    die "Docker could not start the worker container"
  fi

  if assert_docker_worker_online; then
    if [ -n "$rollback_container" ]; then
      docker container rm "$rollback_container" >/dev/null ||
        die "the worker is healthy, but Docker could not remove its rollback container"
    fi
    prune_old_worker_images ||
      printf 'WARNING: the worker is healthy, but old worker image cleanup failed.\n' >&2
    docker ps --filter "name=^/${CONTAINER_NAME}$"
    return 0
  fi

  if [ -n "$rollback_container" ]; then
    if restore_previous_docker_worker \
      "$rollback_container" "$previous_was_running"; then
      die "updated worker health verification failed; the previous container was restored"
    fi
    die "updated worker health verification failed and rollback requires administrator recovery"
  fi
  die "worker health verification failed; the supervised container remains installed for diagnostics"
}

if [ "$SERVICE_MODE" = systemd ]; then
  inspect_systemd_identity
  if ! runuser -u rsctf-worker -- \
    /usr/local/bin/rsctf-worker-agent doctor; then
    die "worker runtime preflight failed before enrollment; fix Docker storage/runtime configuration and rerun this command"
  fi
else
  prepare_docker_supervisor
  if ! run_docker_doctor; then
    if [ "$ALLOW_UNBOUNDED_STORAGE" = false ]; then
      die "worker runtime preflight failed; configure quota-capable Docker storage, or use --allow-unbounded-storage only on a trusted disposable development worker"
    fi
    die "worker runtime preflight failed before enrollment; inspect the Docker daemon configuration and rerun this command"
  fi
fi

if [ "$EXISTING_ENROLLMENT" = false ]; then
  [ -r /dev/tty ] && [ -w /dev/tty ] ||
    die "an interactive terminal is required for the private token prompt"
fi

if [ "$EXISTING_ENROLLMENT" = true ]; then
  if [ "$SERVICE_MODE" = systemd ]; then
    start_and_check_systemd_worker
  else
    start_and_check_docker_worker
  fi
  printf 'RSCTF worker updated and restarted; the existing mTLS identity was preserved.\n'
  exit 0
fi

printf '%s\n' \
  'Security boundary: this host/VM must be dedicated to RSCTF challenge workloads.' \
  'Do not enroll a daily-use computer or a machine that holds unrelated secrets.' \
  >/dev/tty
printf 'Type DEDICATED to continue: ' >/dev/tty
IFS= read -r HOST_CONFIRMATION </dev/tty
[ "$HOST_CONFIRMATION" = "DEDICATED" ] ||
  die "dedicated worker-host confirmation was not provided"
HOST_CONFIRMATION=""

printf 'One-time enrollment token: ' >/dev/tty
TERMINAL_SETTINGS=$(stty -g </dev/tty) ||
  die "could not disable terminal echo for the enrollment token"
stty -echo </dev/tty
if ! IFS= read -r ENROLLMENT_TOKEN </dev/tty; then
  restore_terminal
  die "could not read the enrollment token"
fi
restore_terminal
printf '\n' >/dev/tty
[ -n "$ENROLLMENT_TOKEN" ] || die "the enrollment token must not be empty"

if [ "$SERVICE_MODE" = systemd ]; then
  if ! printf '%s\n' "$ENROLLMENT_TOKEN" | runuser -u rsctf-worker -- \
    /usr/local/bin/rsctf-worker-agent enroll \
    --server-url "$SERVER_URL" \
    --token-stdin \
    --state-dir "$STATE_DIRECTORY"; then
    die "worker enrollment failed; issue a fresh token before retrying if it was consumed"
  fi
else
  if ! enroll_docker_worker; then
    die "worker enrollment failed; issue a fresh token before retrying if it was consumed"
  fi
fi
ENROLLMENT_TOKEN=""

if [ "$SERVICE_MODE" = systemd ]; then
  start_and_check_systemd_worker
else
  start_and_check_docker_worker
fi
printf 'RSCTF worker installed, enrolled, and started successfully.\n'
