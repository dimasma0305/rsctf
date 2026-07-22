#!/usr/bin/env bash

set -euo pipefail

PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
export PATH
umask 077

readonly REPOSITORY="dimasma0305/rsctf"
SERVER_URL=""
VERSION=""
TEMP_DIRECTORY=""
ENROLLMENT_TOKEN=""

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
[[ -r /dev/tty && -w /dev/tty ]] || die "an interactive terminal is required for the private token prompt"

for command in curl docker gh runuser systemctl; do
  command -v "$command" >/dev/null 2>&1 || die "required command is missing: $command"
done
docker info >/dev/null 2>&1 || die "Docker is not running or root cannot access its daemon"

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
readonly ATTESTATION="${TEMP_DIRECTORY}/rsctf-worker-agent-attestation.json"
readonly DOWNLOAD_BASE="${RELEASE_BASE}/download/${VERSION}"

curl "${CURL_ARGS[@]}" --max-filesize 1048576 \
  --output "$INSTALLER" "${DOWNLOAD_BASE}/install-worker.sh"
curl "${CURL_ARGS[@]}" --max-filesize 16777216 \
  --output "$ATTESTATION" "${DOWNLOAD_BASE}/rsctf-worker-agent-attestation.json"

gh attestation verify "$INSTALLER" \
  --bundle "$ATTESTATION" \
  --hostname github.com \
  --repo "$REPOSITORY" \
  --signer-workflow "${REPOSITORY}/.github/workflows/worker-agent-release.yml" \
  --source-ref "refs/tags/${VERSION}" \
  --deny-self-hosted-runners >/dev/null

bash "$INSTALLER" --version "$VERSION"

printf 'One-time enrollment token: ' >/dev/tty
IFS= read -r -s ENROLLMENT_TOKEN </dev/tty
printf '\n' >/dev/tty
[[ -n "$ENROLLMENT_TOKEN" ]] || die "the enrollment token must not be empty"

if ! printf '%s\n' "$ENROLLMENT_TOKEN" | runuser -u rsctf-worker -- \
  /usr/local/bin/rsctf-worker-agent enroll \
    --server-url "$SERVER_URL" \
    --token-stdin \
    --state-dir /var/lib/rsctf-worker; then
  die "worker enrollment failed; issue a fresh token before retrying if it was consumed"
fi
ENROLLMENT_TOKEN=""

systemctl enable --now rsctf-worker-agent.service
systemctl --no-pager --full status rsctf-worker-agent.service
printf 'RSCTF worker installed, enrolled, and started successfully.\n'
