#!/usr/bin/env bash

set -euo pipefail

PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
export PATH
umask 077

readonly REPOSITORY="dimasma0305/rsctf"
readonly STATE_DIRECTORY="/var/lib/rsctf-worker"
SERVER_URL=""
VERSION=""
TEMP_DIRECTORY=""
ENROLLMENT_TOKEN=""
EXISTING_ENROLLMENT=false

usage() {
  cat <<'EOF'
Install and enroll an RSCTF Linux worker.

Usage:
  bootstrap-worker.sh --server-url https://ctf.example [--version vX.Y.Z]

The enrollment token is read privately from the controlling terminal. It is
never accepted in a URL, command-line argument, or environment variable.
EOF
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
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
    -h | --help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: $1 (use --help for usage)"
      ;;
  esac
done

[[ "$SERVER_URL" =~ ^https://([A-Za-z0-9-]+\.)*[A-Za-z0-9-]+(:[0-9]{1,5})?$ ]] || \
  die "--server-url must be an HTTPS origin without a path, query, or credentials"
[[ -z "$VERSION" || "$VERSION" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || \
  die "--version must have the form vX.Y.Z"
[[ "${EUID:-$(id -u)}" -eq 0 ]] || die "run this bootstrap through sudo"
[[ "$(uname -s)" == "Linux" ]] || die "the worker runtime requires a dedicated Linux host or VM"

for command in curl docker runuser sha256sum systemctl; do
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
readonly CURL_ARGS=(
  --disable --fail --silent --show-error --location
  --proto '=https' --proto-redir '=https' --tlsv1.2
  --connect-timeout 15 --max-time 300 --retry 5 --retry-all-errors
  --retry-max-time 300 --speed-limit 1024 --speed-time 30
)

if [[ -z "$VERSION" ]]; then
  latest_url="$(curl "${CURL_ARGS[@]}" --max-filesize 1048576 \
    --output /dev/null --write-out '%{url_effective}' "${RELEASE_BASE}/latest")" || \
    die "could not resolve the latest RSCTF release"
  VERSION="${latest_url##*/}"
  [[ "$VERSION" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || \
    die "the latest release does not use a vX.Y.Z tag"
fi
readonly VERSION

TEMP_DIRECTORY="$(mktemp -d /tmp/rsctf-worker-bootstrap.XXXXXXXX)"
readonly INSTALLER="${TEMP_DIRECTORY}/install-worker.sh"
readonly CHECKSUMS="${TEMP_DIRECTORY}/SHA256SUMS"
readonly DOWNLOAD_BASE="${RELEASE_BASE}/download/${VERSION}"

curl "${CURL_ARGS[@]}" --max-filesize 1048576 \
  --output "$INSTALLER" "${DOWNLOAD_BASE}/install-worker.sh"
curl "${CURL_ARGS[@]}" --max-filesize 1048576 \
  --output "$CHECKSUMS" "${DOWNLOAD_BASE}/SHA256SUMS"

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
bash "$INSTALLER" --version "$VERSION" --skip-attestation

if [[ "$EXISTING_ENROLLMENT" == "true" ]]; then
  systemctl enable --now rsctf-worker-agent.service
  systemctl is-active --quiet rsctf-worker-agent.service || \
    die "the updated worker service did not remain active"
  systemctl --no-pager --full status rsctf-worker-agent.service
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

systemctl enable --now rsctf-worker-agent.service
systemctl --no-pager --full status rsctf-worker-agent.service
printf 'RSCTF worker installed, enrolled, and started successfully.\n'
