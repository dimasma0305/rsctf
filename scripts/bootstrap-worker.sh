#!/usr/bin/env bash

set -euo pipefail

PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
export PATH
umask 077

readonly REPOSITORY="dimasma0305/rsctf"
readonly STATE_DIRECTORY="/var/lib/rsctf-worker"
readonly READY_FILE="/run/rsctf-worker-agent/connected"
readonly CONNECTION_TIMEOUT_SECONDS="${RSCTF_WORKER_CONNECTION_TIMEOUT_SECONDS:-45}"
SERVER_URL=""
VERSION=""
TEMP_DIRECTORY=""
ENROLLMENT_TOKEN=""
EXISTING_ENROLLMENT=false
UNINSTALL=false

usage() {
  cat <<'EOF'
Install, update, or uninstall an RSCTF Linux worker.

Usage:
  bootstrap-worker.sh --server-url https://ctf.example [--version vX.Y.Z]
  bootstrap-worker.sh --uninstall

The enrollment token is read privately from the controlling terminal. It is
never accepted in a URL, command-line argument, or environment variable.

Uninstall refuses to continue while RSCTF-managed workloads exist, then removes
the local service, identity, binary, and dedicated service account. Disable the
worker in RSCTF Admin before uninstalling it.
EOF
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

uninstall_worker() {
  local managed_containers managed_networks owner_label worker_record

  [[ -r /dev/tty && -w /dev/tty ]] || \
    die "an interactive terminal is required for uninstall confirmation"
  for command in docker getent groupdel rm systemctl userdel; do
    command -v "$command" >/dev/null 2>&1 || die "required command is missing: $command"
  done
  docker info >/dev/null 2>&1 || \
    die "Docker must be running so uninstall can verify that no managed workloads remain"
  managed_containers="$(docker ps --all --quiet \
    --filter label=io.rsctf.worker.managed=true)"
  managed_networks="$(docker network ls --quiet \
    --filter label=io.rsctf.worker.managed=true)"
  if [[ -n "$managed_containers" || -n "$managed_networks" ]]; then
    die "RSCTF-managed containers or networks still exist; drain this worker and remove its workloads before uninstalling"
  fi
  if [[ -L "$STATE_DIRECTORY" || (-e "$STATE_DIRECTORY" && ! -d "$STATE_DIRECTORY") ]]; then
    die "${STATE_DIRECTORY} is not a real directory; refusing recursive removal"
  fi
  if [[ -L "/usr/local/share/doc/rsctf-worker-agent" || \
    (-e "/usr/local/share/doc/rsctf-worker-agent" && \
      ! -d "/usr/local/share/doc/rsctf-worker-agent") ]]; then
    die "the worker documentation path is not a real directory; refusing recursive removal"
  fi

  printf '%s\n' \
    'This permanently deletes the local worker certificate and configuration.' \
    'Disable this worker in RSCTF Admin first so its certificate is rejected.' \
    >/dev/tty
  printf 'Type REMOVE to uninstall this worker: ' >/dev/tty
  IFS= read -r confirmation </dev/tty
  [[ "$confirmation" == "REMOVE" ]] || die "worker uninstall was cancelled"
  confirmation=""

  systemctl disable --now rsctf-worker-agent.service >/dev/null 2>&1 || true
  rm -f -- /etc/systemd/system/rsctf-worker-agent.service \
    /usr/local/bin/rsctf-worker-agent
  if [[ -d /usr/local/share/doc/rsctf-worker-agent ]]; then
    rm -rf -- /usr/local/share/doc/rsctf-worker-agent
  fi
  if [[ -d "$STATE_DIRECTORY" ]]; then
    rm -rf -- "$STATE_DIRECTORY"
  fi
  systemctl daemon-reload
  systemctl reset-failed rsctf-worker-agent.service >/dev/null 2>&1 || true

  owner_label="$(docker volume inspect --format \
    '{{ index .Labels "io.rsctf.worker.daemon-owner" }}' \
    rsctf-worker-owner 2>/dev/null || true)"
  if [[ -n "$owner_label" ]]; then
    docker volume rm rsctf-worker-owner >/dev/null
  fi

  if worker_record="$(getent passwd rsctf-worker 2>/dev/null)"; then
    IFS=: read -r worker_name _ worker_uid _ _ worker_home worker_shell <<< "$worker_record"
    if [[ "$worker_name" == "rsctf-worker" && "$worker_uid" =~ ^[0-9]+$ && \
      "$worker_uid" -ne 0 && "$worker_home" == "$STATE_DIRECTORY" && \
      "$worker_shell" == */nologin ]]; then
      userdel rsctf-worker
    else
      printf 'WARNING: retained unexpected rsctf-worker account; inspect it manually.\n' >&2
    fi
  fi
  if getent group rsctf-worker >/dev/null 2>&1; then
    groupdel rsctf-worker 2>/dev/null || \
      printf 'WARNING: retained non-empty rsctf-worker group; inspect it manually.\n' >&2
  fi

  printf 'RSCTF worker software and local identity were removed.\n'
  printf 'The Admin worker record is retained for audit history; keep it Disabled.\n'
}

cleanup() {
  ENROLLMENT_TOKEN=""
  if [[ -n "$TEMP_DIRECTORY" && -d "$TEMP_DIRECTORY" ]]; then
    rm -rf -- "$TEMP_DIRECTORY"
  fi
}

trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

while (($# > 0)); do
  case "$1" in
    --server-url)
      (($# >= 2)) || die "--server-url requires a value"
      [[ -z "$SERVER_URL" ]] || die "--server-url may only be specified once"
      SERVER_URL="${2%/}"
      shift 2
      ;;
    --version)
      (($# >= 2)) || die "--version requires a value"
      [[ -z "$VERSION" ]] || die "--version may only be specified once"
      VERSION="$2"
      shift 2
      ;;
    --uninstall)
      [[ "$UNINSTALL" == "false" ]] || die "--uninstall may only be specified once"
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

[[ "${EUID:-$(id -u)}" -eq 0 ]] || die "run this bootstrap through sudo"
[[ "$(uname -s)" == "Linux" ]] || die "the worker runtime requires a dedicated Linux host or VM"
if [[ "$UNINSTALL" == "true" ]]; then
  [[ -z "$SERVER_URL" && -z "$VERSION" ]] || \
    die "--uninstall cannot be combined with --server-url or --version"
  uninstall_worker
  exit 0
fi

[[ "$SERVER_URL" =~ ^https://([A-Za-z0-9-]+\.)*[A-Za-z0-9-]+(:[0-9]{1,5})?$ ]] || \
  die "--server-url must be an HTTPS origin without a path, query, or credentials"
[[ -z "$VERSION" || "$VERSION" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || \
  die "--version must have the form vX.Y.Z"
[[ "$CONNECTION_TIMEOUT_SECONDS" =~ ^[0-9]+$ ]] && \
  ((CONNECTION_TIMEOUT_SECONDS >= 1 && CONNECTION_TIMEOUT_SECONDS <= 300)) || \
  die "RSCTF_WORKER_CONNECTION_TIMEOUT_SECONDS must be 1..300"

for command in awk head wc wget; do
  command -v "$command" >/dev/null 2>&1 || die "required command is missing: $command"
done

release_wget() {
  wget \
    --https-only \
    --secure-protocol=TLSv1_2 \
    --max-redirect=5 \
    --timeout=30 \
    --read-timeout=30 \
    --tries=5 \
    --retry-connrefused \
    --no-verbose \
    "$@"
}

server_health="$(release_wget --output-document=- "${SERVER_URL}/healthz" |
  head -c 3)" || die "RSCTF health check failed; installation did not start"
[[ "$server_health" == "ok" ]] || \
  die "RSCTF health check returned an unexpected response; installation did not start"
printf 'Verified RSCTF server health at %s/healthz.\n' "$SERVER_URL"

for command in docker journalctl runuser sha256sum systemctl; do
  command -v "$command" >/dev/null 2>&1 || die "required command is missing: $command"
done
docker info >/dev/null 2>&1 || die "Docker is not running or root cannot access its daemon"

identity_files=(worker-key.pem worker-cert.pem worker-ca.pem worker.json)
identity_file_count=0
for identity_file in "${identity_files[@]}"; do
  if [[ -e "${STATE_DIRECTORY}/${identity_file}" || -L "${STATE_DIRECTORY}/${identity_file}" ]]; then
    ((identity_file_count += 1))
  fi
done
if ((identity_file_count == ${#identity_files[@]})); then
  EXISTING_ENROLLMENT=true
elif ((identity_file_count != 0)); then
  die "the worker state directory contains an incomplete identity; revoke that worker and clean the state deliberately before enrolling again"
fi
if [[ "$EXISTING_ENROLLMENT" == "false" ]]; then
  [[ -r /dev/tty && -w /dev/tty ]] || die "an interactive terminal is required for the private token prompt"
fi

readonly RELEASE_BASE="https://github.com/${REPOSITORY}/releases"
if [[ -z "$VERSION" ]]; then
  latest_headers="$(release_wget --spider --server-response \
    "${RELEASE_BASE}/latest" 2>&1)" || \
    die "could not resolve the latest RSCTF release"
  latest_url="$(awk '/^  Location: / { location = $2 } END { print location }' \
    <<< "$latest_headers")"
  latest_url="${latest_url%$'\r'}"
  VERSION="${latest_url##*/}"
  [[ "$VERSION" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || \
    die "the latest release does not use a vX.Y.Z tag"
fi
readonly VERSION

TEMP_DIRECTORY="$(mktemp -d /tmp/rsctf-worker-bootstrap.XXXXXXXX)"
readonly INSTALLER="${TEMP_DIRECTORY}/install-worker.sh"
readonly CHECKSUMS="${TEMP_DIRECTORY}/SHA256SUMS"
readonly DOWNLOAD_BASE="${RELEASE_BASE}/download/${VERSION}"

download() {
  local url="$1"
  local destination="$2"
  local maximum_bytes="$3"
  local -a pipeline_status

  set +e
  release_wget --output-document=- "$url" |
    head -c "$((maximum_bytes + 1))" > "$destination"
  pipeline_status=("${PIPESTATUS[@]}")
  set -e
  if [[ "$(wc -c < "$destination")" -gt "$maximum_bytes" ]]; then
    die "download exceeds ${maximum_bytes} bytes: ${url}"
  fi
  [[ "${pipeline_status[0]}" -eq 0 && "${pipeline_status[1]}" -eq 0 ]] || \
    die "download failed: ${url}"
}

download "${DOWNLOAD_BASE}/install-worker.sh" "$INSTALLER" 1048576
download "${DOWNLOAD_BASE}/SHA256SUMS" "$CHECKSUMS" 1048576

expected_installer_hash=""
installer_checksum_matches=0
while IFS= read -r checksum_line || [[ -n "$checksum_line" ]]; do
  if ((${#checksum_line} >= 66)); then
    checksum_hash="${checksum_line:0:64}"
    checksum_name="${checksum_line:64}"
    if [[ "$checksum_hash" =~ ^[0-9A-Fa-f]{64}$ && "$checksum_name" == "  install-worker.sh" ]]; then
      expected_installer_hash="${checksum_hash,,}"
      ((installer_checksum_matches += 1))
    fi
  fi
done < "$CHECKSUMS"
[[ "$installer_checksum_matches" -eq 1 ]] || \
  die "SHA256SUMS must contain exactly one checksum for install-worker.sh"
actual_installer_hash="$(sha256sum "$INSTALLER")"
actual_installer_hash="${actual_installer_hash%% *}"
actual_installer_hash="${actual_installer_hash,,}"
[[ "$actual_installer_hash" == "$expected_installer_hash" ]] || \
  die "SHA-256 verification failed for install-worker.sh"

# The release installer verifies the worker archive against the matching
# SHA256SUMS asset. Keep this public one-line flow independent of GitHub CLI
# versions; operators who need provenance verification can use the installer's
# explicit attestation mode after downloading it separately.
bash "$INSTALLER" --version "$VERSION" --skip-attestation --bootstrap

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
  local deadline stable_since=0

  deadline=$((SECONDS + CONNECTION_TIMEOUT_SECONDS))
  printf 'Waiting up to %s seconds for the authenticated worker control session...\n' \
    "$CONNECTION_TIMEOUT_SECONDS"
  while ((SECONDS < deadline)); do
    if ! systemctl is-active --quiet rsctf-worker-agent.service; then
      show_worker_diagnostics
      die "worker health check failed: the service stopped before connecting to RSCTF"
    fi
    if [[ -f "$READY_FILE" ]]; then
      if ((stable_since == 0)); then
        stable_since="$SECONDS"
      elif ((SECONDS - stable_since >= 3)); then
        printf 'Worker health check passed: mTLS control session accepted by RSCTF.\n'
        return
      fi
    else
      stable_since=0
    fi
    sleep 1
  done

  show_worker_diagnostics
  die "worker health check timed out: the service did not establish a stable mTLS control session; the worker remains installed but offline"
}

start_and_check_worker() {
  rm -f -- "$READY_FILE"
  systemctl enable rsctf-worker-agent.service >/dev/null
  systemctl reset-failed rsctf-worker-agent.service >/dev/null 2>&1 || true
  systemctl restart rsctf-worker-agent.service
  assert_worker_online
  systemctl --no-pager --full status rsctf-worker-agent.service
}

if [[ "$EXISTING_ENROLLMENT" == "true" ]]; then
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
[[ "$HOST_CONFIRMATION" == "DEDICATED" ]] || die "dedicated worker-host confirmation was not provided"
HOST_CONFIRMATION=""

printf 'One-time enrollment token: ' >/dev/tty
IFS= read -r -s ENROLLMENT_TOKEN </dev/tty
printf '\n' >/dev/tty
[[ -n "$ENROLLMENT_TOKEN" ]] || die "the enrollment token must not be empty"

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
