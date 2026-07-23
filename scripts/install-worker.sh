#!/bin/sh

set -eu

PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
export PATH
umask 077

REPOSITORY="dimasma0305/rsctf"
WORKER_USER="rsctf-worker"
WORKER_GROUP="rsctf-worker"
STATE_DIRECTORY="/var/lib/rsctf-worker"
BINARY_PATH="/usr/local/bin/rsctf-worker-agent"
UNIT_PATH="/etc/systemd/system/rsctf-worker-agent.service"
DOCUMENTATION_DIRECTORY="/usr/local/share/doc/rsctf-worker-agent"

VERSION=""
TEMP_DIRECTORY=""
SKIP_ATTESTATION=false
BOOTSTRAP_MODE=false
SERVICE_WAS_ACTIVE=false
SERVICE_WAS_ENABLED=false
SERVICE_ENABLE_SUCCEEDED=false
SERVICE_RESTART_ATTEMPTED=false
INSTALL_TRANSACTION_ACTIVE=false
BINARY_WAS_PRESENT=false
UNIT_WAS_PRESENT=false
DOCUMENTATION_DIRECTORY_WAS_PRESENT=false
LICENSE_WAS_PRESENT=false
NOTICE_WAS_PRESENT=false
WORKER_GROUP_CREATED=false
WORKER_USER_CREATED=false
WORKER_DOCKER_MEMBERSHIP_ADDED=false
STATE_DIRECTORY_CREATED=false
STAGED_BINARY=""
STAGED_UNIT=""

usage() {
  cat <<'EOF'
Install the RSCTF worker agent on a systemd-based Linux host.

Usage:
  install-worker.sh [--version vX.Y.Z] [--skip-attestation] [--bootstrap]
  install-worker.sh --help

Options:
  --version vX.Y.Z    Install a specific GitHub release instead of the latest.
  --skip-attestation Install with HTTPS and SHA-256 verification only. This
                     weakens release authenticity and is not recommended.
  --bootstrap        Continue into enrollment through the verified public
                     bootstrap instead of printing manual enrollment steps.
  -h, --help          Show this help message.

This installer runs with a POSIX sh and supports GNU wget and BusyBox wget.
The installer never accepts an enrollment token. A fresh installation enables
the service without starting it and prints secure enrollment commands. An
already-active service is restarted after a verified upgrade.

GitHub CLI with `gh attestation verify` support is required unless the explicit
--skip-attestation escape hatch is used. Verification uses the release's local
bundle, so no GitHub login or token is required on the worker.

Supported platforms: systemd-based Linux amd64 and Linux arm64/aarch64 hosts.
Containers and Docker Desktop's internal VM are not persistent worker hosts.
EOF
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

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

  if ! wget -q -S -T 30 --spider "$latest_release_url" 2>"$latest_headers"; then
    die "could not resolve the latest worker release"
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

cleanup() {
  cleanup_status=$?

  if [ "$INSTALL_TRANSACTION_ACTIVE" = true ]; then
    printf 'WARNING: installation was interrupted; restoring the previous worker installation.\n' >&2
    if ! rollback_installation; then
      printf 'WARNING: worker rollback was incomplete; inspect the errors above before retrying.\n' >&2
    fi
  fi
  if [ -n "$TEMP_DIRECTORY" ] && [ -d "$TEMP_DIRECTORY" ]; then
    rm -rf "$TEMP_DIRECTORY"
  fi
  [ -z "$STAGED_BINARY" ] || rm -f "$STAGED_BINARY"
  [ -z "$STAGED_UNIT" ] || rm -f "$STAGED_UNIT"

  return "$cleanup_status"
}

trap cleanup 0
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      [ "$#" -ge 2 ] || die "--version requires a value"
      [ -z "$VERSION" ] || die "--version may only be specified once"
      VERSION=$2
      shift 2
      ;;
    --skip-attestation)
      [ "$SKIP_ATTESTATION" = false ] ||
        die "--skip-attestation may only be specified once"
      SKIP_ATTESTATION=true
      shift
      ;;
    --bootstrap)
      [ "$BOOTSTRAP_MODE" = false ] ||
        die "--bootstrap may only be specified once"
      BOOTSTRAP_MODE=true
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

require_command grep
[ -z "$VERSION" ] || is_release_version "$VERSION" ||
  die "--version must have the form vX.Y.Z"
[ "$(id -u)" -eq 0 ] || die "this installer must be run as root"
[ "$(uname -s)" = "Linux" ] || die "only Linux is supported"
if [ ! -d /run/systemd/system ]; then
  die "systemd is not active; use a persistent systemd-based Linux host or VM, not a container or Docker Desktop internal VM"
fi

case "$(uname -m)" in
  x86_64 | amd64)
    ARCHITECTURE="amd64"
    ;;
  aarch64 | arm64)
    ARCHITECTURE="arm64"
    ;;
  *)
    die "unsupported architecture: $(uname -m); expected amd64 or aarch64"
    ;;
esac

for required_command in awk cp docker getent groupadd id install mktemp mv \
  rm rmdir sha256sum systemctl tar useradd usermod wc wget; do
  require_command "$required_command"
done

getent group docker >/dev/null 2>&1 ||
  die "the Docker group does not exist; install Docker Engine first"

if [ "$SKIP_ATTESTATION" = false ]; then
  require_command gh
  attestation_help=$(gh attestation verify --help 2>&1) ||
    die "this GitHub CLI does not support attestation verification; upgrade gh, or explicitly use --skip-attestation"
  for required_option in \
    --bundle \
    --deny-self-hosted-runners \
    --hostname \
    --repo \
    --signer-workflow \
    --source-ref; do
    printf '%s\n' "$attestation_help" | grep -Fq -e "$required_option" ||
      die "this GitHub CLI does not support the required offline attestation policy (${required_option}); upgrade gh, or explicitly use --skip-attestation"
  done
fi

if systemctl is-active --quiet rsctf-worker-agent.service >/dev/null 2>&1; then
  SERVICE_WAS_ACTIVE=true
fi
if systemctl is-enabled --quiet rsctf-worker-agent.service >/dev/null 2>&1; then
  SERVICE_WAS_ENABLED=true
fi

ASSET="rsctf-worker-agent-linux-${ARCHITECTURE}.tar.gz"
CHECKSUM_ASSET="SHA256SUMS"
BUNDLE_ASSET="rsctf-worker-agent-attestation.json"
RELEASE_BASE="https://github.com/${REPOSITORY}/releases"

TEMP_DIRECTORY=$(mktemp -d /tmp/rsctf-worker-install.XXXXXX)
if [ -z "$VERSION" ]; then
  latest_url=$(resolve_latest_release "${RELEASE_BASE}/latest")
  latest_prefix="${RELEASE_BASE}/tag/"
  case "$latest_url" in
    "${latest_prefix}"*) VERSION=${latest_url#"$latest_prefix"} ;;
    *) die "the latest release redirected to an unexpected URL" ;;
  esac
  is_release_version "$VERSION" ||
    die "the latest worker release does not have a strict vX.Y.Z tag"
fi

DOWNLOAD_BASE="${RELEASE_BASE}/download/${VERSION}"
SOURCE_REF="refs/tags/${VERSION}"
ARCHIVE_PATH="${TEMP_DIRECTORY}/${ASSET}"
CHECKSUM_PATH="${TEMP_DIRECTORY}/${CHECKSUM_ASSET}"
BUNDLE_PATH="${TEMP_DIRECTORY}/${BUNDLE_ASSET}"
EXTRACT_DIRECTORY="${TEMP_DIRECTORY}/extract"
mkdir -m 0700 "$EXTRACT_DIRECTORY"

printf 'Downloading %s from %s...\n' "$ASSET" "$VERSION"
download "${DOWNLOAD_BASE}/${ASSET}" "$ARCHIVE_PATH" 134217728
download "${DOWNLOAD_BASE}/${CHECKSUM_ASSET}" "$CHECKSUM_PATH" 1048576
if [ "$SKIP_ATTESTATION" = false ]; then
  download "${DOWNLOAD_BASE}/${BUNDLE_ASSET}" "$BUNDLE_PATH" 16777216
fi

archive_checksum=$(checksum_for "$CHECKSUM_PATH" "$ASSET")
checksum_matches=${archive_checksum%%:*}
expected_hash=${archive_checksum#*:}
[ "$checksum_matches" -eq 1 ] ||
  die "SHA256SUMS must contain exactly one checksum for ${ASSET}"

actual_hash=$(sha256sum "$ARCHIVE_PATH" | awk '{ print tolower($1) }')
[ "$actual_hash" = "$expected_hash" ] ||
  die "SHA-256 verification failed for ${ASSET}"
printf 'Verified SHA-256 checksum for %s.\n' "$ASSET"

if [ "$SKIP_ATTESTATION" = false ]; then
  printf 'Verifying GitHub artifact attestation...\n'
  gh attestation verify \
    "$ARCHIVE_PATH" \
    --bundle "$BUNDLE_PATH" \
    --hostname github.com \
    --repo "$REPOSITORY" \
    --signer-workflow "${REPOSITORY}/.github/workflows/worker-agent-release.yml" \
    --source-ref "$SOURCE_REF" \
    --deny-self-hosted-runners \
    >/dev/null ||
    die "GitHub artifact attestation verification failed; the release artifact, bundle, or provenance policy did not validate"
  printf 'Verified GitHub artifact attestation.\n'
else
  printf 'WARNING: GitHub artifact attestation verification was explicitly skipped; only HTTPS and SHA-256 were verified.\n' >&2
fi

ARCHIVE_LIST="${TEMP_DIRECTORY}/archive-list.txt"
ARCHIVE_VERBOSE_LIST="${TEMP_DIRECTORY}/archive-verbose-list.txt"
tar -tzf "$ARCHIVE_PATH" >"$ARCHIVE_LIST" ||
  die "release archive is not a valid gzip-compressed tar archive"
LC_ALL=C tar -tvzf "$ARCHIVE_PATH" >"$ARCHIVE_VERBOSE_LIST" ||
  die "release archive metadata cannot be read"

while IFS= read -r verbose_line || [ -n "$verbose_line" ]; do
  case "$verbose_line" in
    [-d]*) ;;
    *) die "release archive contains a link or other unsupported entry type" ;;
  esac
done <"$ARCHIVE_VERBOSE_LIST"

[ -s "$ARCHIVE_LIST" ] || die "release archive is empty"
archive_prefix=""
binary_entries=0
while IFS= read -r entry || [ -n "$entry" ]; do
  case "$entry" in
    rsctf-worker-agent)
      candidate_prefix=""
      ;;
    */rsctf-worker-agent)
      candidate_prefix=${entry%/rsctf-worker-agent}
      case "$candidate_prefix" in
        "" | "." | ".." | *[!A-Za-z0-9._-]*)
          die "release archive has an unsafe directory layout"
          ;;
      esac
      ;;
    *)
      continue
      ;;
  esac

  if [ "$binary_entries" -eq 0 ]; then
    archive_prefix=$candidate_prefix
  elif [ "$archive_prefix" != "$candidate_prefix" ]; then
    die "release archive contains conflicting binary paths"
  fi
  binary_entries=$((binary_entries + 1))
done <"$ARCHIVE_LIST"

[ "$binary_entries" -eq 1 ] ||
  die "release archive must contain exactly one rsctf-worker-agent binary"

if [ -n "$archive_prefix" ]; then
  archive_root="${archive_prefix}/"
else
  archive_root=""
fi

directory_entries=0
binary_file_entries=0
unit_file_entries=0
license_file_entries=0
notice_file_entries=0

while IFS= read -r entry || [ -n "$entry" ]; do
  case "$entry" in
    "$archive_root")
      [ -n "$archive_root" ] ||
        die "release archive contains an empty path"
      directory_entries=$((directory_entries + 1))
      ;;
    "${archive_root}rsctf-worker-agent")
      binary_file_entries=$((binary_file_entries + 1))
      ;;
    "${archive_root}rsctf-worker-agent.service")
      unit_file_entries=$((unit_file_entries + 1))
      ;;
    "${archive_root}LICENSE.txt")
      license_file_entries=$((license_file_entries + 1))
      ;;
    "${archive_root}NOTICE")
      notice_file_entries=$((notice_file_entries + 1))
      ;;
    *)
      die "release archive contains an unexpected path: $entry"
      ;;
  esac
done <"$ARCHIVE_LIST"

[ "$directory_entries" -le 1 ] ||
  die "release archive contains a duplicate top-level directory"
[ "$binary_file_entries" -eq 1 ] ||
  die "release archive is missing or duplicates the worker binary"
[ "$unit_file_entries" -eq 1 ] ||
  die "release archive is missing or duplicates the systemd unit"
[ "$license_file_entries" -eq 1 ] ||
  die "release archive is missing or duplicates LICENSE.txt"
[ "$notice_file_entries" -eq 1 ] ||
  die "release archive is missing or duplicates NOTICE"

tar -xzf "$ARCHIVE_PATH" -C "$EXTRACT_DIRECTORY" ||
  die "release archive could not be extracted"

if [ -n "$archive_prefix" ]; then
  EXTRACTED_ROOT="${EXTRACT_DIRECTORY}/${archive_prefix}"
else
  EXTRACTED_ROOT="$EXTRACT_DIRECTORY"
fi
EXTRACTED_BINARY="${EXTRACTED_ROOT}/rsctf-worker-agent"
EXTRACTED_UNIT="${EXTRACTED_ROOT}/rsctf-worker-agent.service"
ROLLBACK_DIRECTORY="${TEMP_DIRECTORY}/rollback"
ROLLBACK_BINARY="${ROLLBACK_DIRECTORY}/rsctf-worker-agent"
ROLLBACK_UNIT="${ROLLBACK_DIRECTORY}/rsctf-worker-agent.service"
ROLLBACK_LICENSE="${ROLLBACK_DIRECTORY}/LICENSE.txt"
ROLLBACK_NOTICE="${ROLLBACK_DIRECTORY}/NOTICE"

for expected_file in "$EXTRACTED_BINARY" "$EXTRACTED_UNIT" \
  "${EXTRACTED_ROOT}/LICENSE.txt" "${EXTRACTED_ROOT}/NOTICE"; do
  [ -f "$expected_file" ] && [ ! -L "$expected_file" ] ||
    die "archive did not extract a regular expected file: $expected_file"
  [ -s "$expected_file" ] ||
    die "archive contains an empty expected file: $expected_file"
done
[ -x "$EXTRACTED_BINARY" ] ||
  die "the extracted worker binary is not executable"

snapshot_installed_file() {
  snapshot_installed_path=$1
  snapshot_backup_path=$2

  if [ -e "$snapshot_installed_path" ] || [ -L "$snapshot_installed_path" ]; then
    [ -f "$snapshot_installed_path" ] && [ ! -L "$snapshot_installed_path" ] ||
      die "${snapshot_installed_path} must be a regular file, not a link or another file type"
    cp -p "$snapshot_installed_path" "$snapshot_backup_path" ||
      die "could not retain the existing worker installation at ${snapshot_installed_path}"
    return 0
  fi

  return 1
}

restore_installed_file() {
  restore_installed_path=$1
  restore_backup_path=$2
  restore_was_present=$3
  restore_path="${restore_installed_path}.rsctf-rollback.$$"

  if [ "$restore_was_present" = true ]; then
    rm -f "$restore_path" || return 1
    if ! cp -p "$restore_backup_path" "$restore_path"; then
      rm -f "$restore_path"
      return 1
    fi
    if ! mv -f -T "$restore_path" "$restore_installed_path"; then
      rm -f "$restore_path"
      return 1
    fi
  else
    rm -f "$restore_installed_path" || return 1
  fi
}

rollback_installation() {
  rollback_failed=false

  # Disable automatic cleanup rollback before performing the rollback explicitly.
  INSTALL_TRANSACTION_ACTIVE=false
  printf 'Restoring the previous RSCTF worker files and service state...\n' >&2

  if ! restore_installed_file "$BINARY_PATH" "$ROLLBACK_BINARY" "$BINARY_WAS_PRESENT"; then
    printf 'error: could not restore %s\n' "$BINARY_PATH" >&2
    rollback_failed=true
  fi
  if ! restore_installed_file "$UNIT_PATH" "$ROLLBACK_UNIT" "$UNIT_WAS_PRESENT"; then
    printf 'error: could not restore %s\n' "$UNIT_PATH" >&2
    rollback_failed=true
  fi
  if ! restore_installed_file "$DOCUMENTATION_DIRECTORY/LICENSE.txt" \
    "$ROLLBACK_LICENSE" "$LICENSE_WAS_PRESENT"; then
    printf 'error: could not restore %s\n' \
      "$DOCUMENTATION_DIRECTORY/LICENSE.txt" >&2
    rollback_failed=true
  fi
  if ! restore_installed_file "$DOCUMENTATION_DIRECTORY/NOTICE" \
    "$ROLLBACK_NOTICE" "$NOTICE_WAS_PRESENT"; then
    printf 'error: could not restore %s\n' \
      "$DOCUMENTATION_DIRECTORY/NOTICE" >&2
    rollback_failed=true
  fi
  if [ "$DOCUMENTATION_DIRECTORY_WAS_PRESENT" = false ] &&
    [ -d "$DOCUMENTATION_DIRECTORY" ]; then
    if ! rmdir "$DOCUMENTATION_DIRECTORY"; then
      printf 'error: could not remove the newly created non-empty documentation directory %s\n' \
        "$DOCUMENTATION_DIRECTORY" >&2
      rollback_failed=true
    fi
  fi
  if ! systemctl daemon-reload; then
    printf 'error: systemd could not reload the restored worker service\n' >&2
    rollback_failed=true
  fi

  if [ "$SERVICE_WAS_ENABLED" = true ]; then
    if ! systemctl enable rsctf-worker-agent.service >/dev/null; then
      printf 'error: systemd could not restore the worker service enabled state\n' >&2
      rollback_failed=true
    fi
  elif [ "$SERVICE_ENABLE_SUCCEEDED" = true ] ||
    systemctl is-enabled --quiet rsctf-worker-agent.service >/dev/null 2>&1; then
    if ! systemctl disable rsctf-worker-agent.service >/dev/null; then
      printf 'error: systemd could not restore the worker service disabled state\n' >&2
      rollback_failed=true
    fi
  fi

  if [ "$SERVICE_WAS_ACTIVE" = true ] &&
    [ "$SERVICE_RESTART_ATTEMPTED" = true ]; then
    if ! systemctl restart rsctf-worker-agent.service; then
      printf 'error: systemd could not restart the restored worker service\n' >&2
      rollback_failed=true
    fi
  fi

  if [ "$WORKER_GROUP_CREATED" = true ] ||
    [ "$WORKER_USER_CREATED" = true ] ||
    [ "$WORKER_DOCKER_MEMBERSHIP_ADDED" = true ] ||
    [ "$STATE_DIRECTORY_CREATED" = true ]; then
    printf 'NOTE: the idempotent worker identity, Docker-group membership, and state directory are retained for a safe retry. Remove them manually only after confirming they are unused.\n' >&2
  fi

  [ "$rollback_failed" = false ]
}

fail_install_transaction() {
  failure_reason=$1

  if rollback_installation; then
    die "${failure_reason}; the previous worker installation was restored"
  fi
  die "${failure_reason}; rollback was incomplete and requires administrator recovery"
}

if ! getent group "$WORKER_GROUP" >/dev/null 2>&1; then
  groupadd --system "$WORKER_GROUP"
  WORKER_GROUP_CREATED=true
fi

worker_group_record=$(getent group "$WORKER_GROUP") ||
  die "could not resolve the ${WORKER_GROUP} group after creating it"
IFS=: read -r resolved_worker_group _ worker_group_gid _ <<EOF
$worker_group_record
EOF
if [ "$resolved_worker_group" != "$WORKER_GROUP" ] ||
  ! is_unsigned_integer "$worker_group_gid" ||
  [ "$worker_group_gid" -eq 0 ]; then
  die "the ${WORKER_GROUP} group has an unsafe identity"
fi

nologin_shell=$(command -v nologin || true)
[ -n "$nologin_shell" ] || nologin_shell="/usr/sbin/nologin"

if ! getent passwd "$WORKER_USER" >/dev/null 2>&1; then
  useradd \
    --system \
    --gid "$WORKER_GROUP" \
    --groups docker \
    --home-dir "$STATE_DIRECTORY" \
    --no-create-home \
    --shell "$nologin_shell" \
    "$WORKER_USER"
  WORKER_USER_CREATED=true
else
  worker_record=$(getent passwd "$WORKER_USER") ||
    die "could not resolve the existing ${WORKER_USER} account"
  IFS=: read -r resolved_worker_user _ worker_uid worker_gid _ worker_home worker_shell <<EOF
$worker_record
EOF
  if [ "$resolved_worker_user" != "$WORKER_USER" ] ||
    ! is_unsigned_integer "$worker_uid" ||
    [ "$worker_uid" -eq 0 ] ||
    [ "$worker_gid" != "$worker_group_gid" ] ||
    [ "$worker_home" != "$STATE_DIRECTORY" ] ||
    [ "$worker_shell" != "$nologin_shell" ]; then
    die "existing ${WORKER_USER} account does not match the required non-login service identity; refusing to grant Docker access"
  fi
  if ! id -nG "$WORKER_USER" | grep -qw docker; then
    usermod \
      --append \
      --groups docker \
      "$WORKER_USER"
    WORKER_DOCKER_MEMBERSHIP_ADDED=true
  fi
fi

if [ -L "$STATE_DIRECTORY" ] ||
  { [ -e "$STATE_DIRECTORY" ] && [ ! -d "$STATE_DIRECTORY" ]; }; then
  die "${STATE_DIRECTORY} must be a real directory, not a link or another file type"
fi
if [ ! -d "$STATE_DIRECTORY" ]; then
  STATE_DIRECTORY_CREATED=true
fi
install -d -m 0700 -o "$WORKER_USER" -g "$WORKER_GROUP" "$STATE_DIRECTORY"
mkdir -m 0700 "$ROLLBACK_DIRECTORY"
if snapshot_installed_file "$BINARY_PATH" "$ROLLBACK_BINARY"; then
  BINARY_WAS_PRESENT=true
fi
if snapshot_installed_file "$UNIT_PATH" "$ROLLBACK_UNIT"; then
  UNIT_WAS_PRESENT=true
fi
if [ -L "$DOCUMENTATION_DIRECTORY" ] ||
  { [ -e "$DOCUMENTATION_DIRECTORY" ] &&
    [ ! -d "$DOCUMENTATION_DIRECTORY" ]; }; then
  die "${DOCUMENTATION_DIRECTORY} must be a real directory, not a link or another file type"
fi
if [ -d "$DOCUMENTATION_DIRECTORY" ]; then
  DOCUMENTATION_DIRECTORY_WAS_PRESENT=true
fi
if snapshot_installed_file "$DOCUMENTATION_DIRECTORY/LICENSE.txt" "$ROLLBACK_LICENSE"; then
  LICENSE_WAS_PRESENT=true
fi
if snapshot_installed_file "$DOCUMENTATION_DIRECTORY/NOTICE" "$ROLLBACK_NOTICE"; then
  NOTICE_WAS_PRESENT=true
fi
INSTALL_TRANSACTION_ACTIVE=true

if [ "$DOCUMENTATION_DIRECTORY_WAS_PRESENT" = false ]; then
  install -d -m 0755 -o root -g root "$DOCUMENTATION_DIRECTORY" ||
    fail_install_transaction "could not install the worker documentation directory"
fi

STAGED_BINARY="${BINARY_PATH}.rsctf-install.$$"
STAGED_UNIT="${UNIT_PATH}.rsctf-install.$$"
rm -f "$STAGED_BINARY" "$STAGED_UNIT"
install -m 0755 -o root -g root "$EXTRACTED_BINARY" "$STAGED_BINARY" ||
  fail_install_transaction "could not install the worker binary"
install -m 0644 -o root -g root "$EXTRACTED_UNIT" "$STAGED_UNIT" ||
  fail_install_transaction "could not install the worker service unit"
mv -f -T "$STAGED_BINARY" "$BINARY_PATH" ||
  fail_install_transaction "could not activate the worker binary"
mv -f -T "$STAGED_UNIT" "$UNIT_PATH" ||
  fail_install_transaction "could not activate the worker service unit"
install -m 0644 -o root -g root \
  "${EXTRACTED_ROOT}/LICENSE.txt" \
  "${EXTRACTED_ROOT}/NOTICE" \
  "$DOCUMENTATION_DIRECTORY/" ||
  fail_install_transaction "could not install the worker documentation"

systemctl daemon-reload ||
  fail_install_transaction "systemd could not reload the installed worker service"
systemctl enable rsctf-worker-agent.service >/dev/null ||
  fail_install_transaction "systemd could not enable the worker service"
SERVICE_ENABLE_SUCCEEDED=true

if [ "$SERVICE_WAS_ACTIVE" = true ]; then
  SERVICE_RESTART_ATTEMPTED=true
  systemctl restart rsctf-worker-agent.service ||
    fail_install_transaction "systemd could not restart the upgraded worker service"
  INSTALL_TRANSACTION_ACTIVE=false
  printf '\nRSCTF worker agent updated at %s.\n' "$BINARY_PATH"
  printf 'The previously active service was restarted successfully.\n'
else
  INSTALL_TRANSACTION_ACTIVE=false
  printf '\nRSCTF worker agent installed at %s.\n' "$BINARY_PATH"
  printf 'The service is enabled but has not been started.\n\n'
  if [ "$BOOTSTRAP_MODE" = true ]; then
    printf 'The verified bootstrap will now validate Docker and enroll this worker.\n'
    exit 0
  fi
  cat <<'EOF'
Enroll this worker (replace the URL and enter the one-time token when prompted):

  sudo -v
  printf 'One-time enrollment token: '
  old_tty=$(stty -g); stty -echo
  IFS= read -r ONE_TIME_TOKEN
  stty "$old_tty"; printf '\n'
  printf '%s\n' "$ONE_TIME_TOKEN" | sudo -n -u rsctf-worker -- \
    /usr/local/bin/rsctf-worker-agent enroll \
      --server-url https://ctf.example \
      --token-stdin \
      --state-dir /var/lib/rsctf-worker
  unset ONE_TIME_TOKEN old_tty

After enrollment succeeds, start the service:

  sudo systemctl start rsctf-worker-agent.service
  sudo systemctl status rsctf-worker-agent.service
EOF
fi
