#!/bin/sh

set -eu

PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
export PATH
umask 077

REPOSITORY="dimasma0305/rsctf"
STATE_DIRECTORY="/var/lib/rsctf-worker"
READY_FILE="/run/rsctf-worker-agent/connected"
CONNECTION_TIMEOUT_SECONDS="${RSCTF_WORKER_CONNECTION_TIMEOUT_SECONDS:-45}"
SERVER_URL=""
VERSION=""
TEMP_DIRECTORY=""
ENROLLMENT_TOKEN=""
TERMINAL_SETTINGS=""
EXISTING_ENROLLMENT=false
UNINSTALL=false

usage() {
  cat <<'EOF'
Install, update, or uninstall an RSCTF Linux worker.

Usage:
  bootstrap-worker.sh --server-url https://ctf.example [--version vX.Y.Z]
  bootstrap-worker.sh --uninstall

This script runs with a POSIX sh and supports both GNU wget and BusyBox wget.
The enrollment token is read privately from the controlling terminal. It is
never accepted in a URL, command-line argument, or environment variable.

The worker service requires a persistent, systemd-based Linux host or VM.
Running the installer inside a container or Docker Desktop's internal VM is not
supported because that environment cannot persistently host the worker service.

Uninstall refuses to continue while RSCTF-managed workloads exist, then removes
the local service, identity, binary, and dedicated service account. Disable the
worker in RSCTF Admin before uninstalling it.
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

uninstall_worker() {
  [ -r /dev/tty ] && [ -w /dev/tty ] ||
    die "an interactive terminal is required for uninstall confirmation"
  for required_command in docker getent groupdel rm systemctl userdel; do
    require_command "$required_command"
  done
  docker info >/dev/null 2>&1 ||
    die "Docker must be running so uninstall can verify that no managed workloads remain"
  managed_containers=$(docker ps --all --quiet \
    --filter label=io.rsctf.worker.managed=true)
  managed_networks=$(docker network ls --quiet \
    --filter label=io.rsctf.worker.managed=true)
  if [ -n "$managed_containers" ] || [ -n "$managed_networks" ]; then
    die "RSCTF-managed containers or networks still exist; drain this worker and remove its workloads before uninstalling"
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

  systemctl disable --now rsctf-worker-agent.service >/dev/null 2>&1 || true
  rm -f /etc/systemd/system/rsctf-worker-agent.service \
    /usr/local/bin/rsctf-worker-agent
  if [ -d /usr/local/share/doc/rsctf-worker-agent ]; then
    rm -rf /usr/local/share/doc/rsctf-worker-agent
  fi
  if [ -d "$STATE_DIRECTORY" ]; then
    rm -rf "$STATE_DIRECTORY"
  fi
  systemctl daemon-reload
  systemctl reset-failed rsctf-worker-agent.service >/dev/null 2>&1 || true

  owner_label=$(docker volume inspect --format \
    '{{ index .Labels "io.rsctf.worker.daemon-owner" }}' \
    rsctf-worker-owner 2>/dev/null || true)
  if [ -n "$owner_label" ]; then
    docker volume rm rsctf-worker-owner >/dev/null
  fi

  if worker_record=$(getent passwd rsctf-worker 2>/dev/null); then
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
    if [ "$worker_identity_safe" = true ]; then
      userdel rsctf-worker
    else
      printf 'WARNING: retained unexpected rsctf-worker account; inspect it manually.\n' >&2
    fi
  fi
  if getent group rsctf-worker >/dev/null 2>&1; then
    groupdel rsctf-worker 2>/dev/null ||
      printf 'WARNING: retained non-empty rsctf-worker group; inspect it manually.\n' >&2
  fi

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
  [ -z "$SERVER_URL" ] && [ -z "$VERSION" ] ||
    die "--uninstall cannot be combined with --server-url or --version"
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

if [ ! -d /run/systemd/system ]; then
  die "systemd is not active; use a persistent systemd-based Linux host or VM, not a container or Docker Desktop internal VM"
fi
for required_command in docker journalctl runuser stty systemctl; do
  require_command "$required_command"
done
docker info >/dev/null 2>&1 ||
  die "Docker is not running or root cannot access its daemon"

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
if [ "$EXISTING_ENROLLMENT" = false ]; then
  [ -r /dev/tty ] && [ -w /dev/tty ] ||
    die "an interactive terminal is required for the private token prompt"
fi

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
sh "$INSTALLER" --version "$VERSION" --skip-attestation --bootstrap

if ! runuser -u rsctf-worker -- \
  /usr/local/bin/rsctf-worker-agent doctor; then
  die "worker runtime preflight failed before enrollment; fix Docker storage/runtime configuration and rerun this command"
fi

show_worker_diagnostics() {
  printf '\nRecent worker service diagnostics:\n' >&2
  systemctl --no-pager --full status rsctf-worker-agent.service >&2 || true
  journalctl --no-pager --unit rsctf-worker-agent.service --lines 30 >&2 || true
}

assert_worker_online() {
  elapsed_seconds=0
  stable_seconds=0

  printf 'Waiting up to %s seconds for the authenticated worker control session...\n' \
    "$CONNECTION_TIMEOUT_SECONDS"
  while [ "$elapsed_seconds" -lt "$CONNECTION_TIMEOUT_SECONDS" ]; do
    if ! systemctl is-active --quiet rsctf-worker-agent.service; then
      show_worker_diagnostics
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

  show_worker_diagnostics
  die "worker health check timed out: the service did not establish a stable mTLS control session; the worker remains installed but offline"
}

start_and_check_worker() {
  rm -f "$READY_FILE"
  systemctl enable rsctf-worker-agent.service >/dev/null
  systemctl reset-failed rsctf-worker-agent.service >/dev/null 2>&1 || true
  systemctl restart rsctf-worker-agent.service
  assert_worker_online
  systemctl --no-pager --full status rsctf-worker-agent.service
}

if [ "$EXISTING_ENROLLMENT" = true ]; then
  start_and_check_worker
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

if ! printf '%s\n' "$ENROLLMENT_TOKEN" | runuser -u rsctf-worker -- \
  /usr/local/bin/rsctf-worker-agent enroll \
  --server-url "$SERVER_URL" \
  --token-stdin \
  --state-dir "$STATE_DIRECTORY"; then
  die "worker enrollment failed; issue a fresh token before retrying if it was consumed"
fi
ENROLLMENT_TOKEN=""

start_and_check_worker
printf 'RSCTF worker installed, enrolled, and started successfully.\n'
