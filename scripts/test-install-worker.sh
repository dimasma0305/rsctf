#!/usr/bin/env bash

set -euo pipefail

REPOSITORY_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly REPOSITORY_ROOT
readonly TEST_IMAGE="${RSCTF_INSTALLER_TEST_IMAGE:-ubuntu@sha256:4fbb8e6a8395de5a7550b33509421a2bafbc0aab6c06ba2cef9ebffbc7092d90}"
readonly ASSET="rsctf-worker-agent-linux-amd64.tar.gz"

TEMP_DIRECTORY="$(mktemp -d)"
trap 'rm -rf -- "$TEMP_DIRECTORY"' EXIT

make_package() {
  local package_directory="$1"

  mkdir -p "$package_directory/rsctf-worker-agent"
  printf '#!/usr/bin/env sh\nexit 0\n' > \
    "$package_directory/rsctf-worker-agent/rsctf-worker-agent"
  chmod 0755 "$package_directory/rsctf-worker-agent/rsctf-worker-agent"
  install -m 0644 \
    "$REPOSITORY_ROOT/agents/worker-agent/rsctf-worker-agent.service" \
    "$package_directory/rsctf-worker-agent/rsctf-worker-agent.service"
  install -m 0644 "$REPOSITORY_ROOT/LICENSE.txt" \
    "$package_directory/rsctf-worker-agent/LICENSE.txt"
  install -m 0644 "$REPOSITORY_ROOT/NOTICE" \
    "$package_directory/rsctf-worker-agent/NOTICE"
}

make_fixture() {
  local fixture_directory="$1"
  local package_directory="$2"

  mkdir -p "$fixture_directory"
  tar -C "$package_directory" -czf "$fixture_directory/$ASSET" rsctf-worker-agent
  (
    cd "$fixture_directory"
    sha256sum "$ASSET" > SHA256SUMS
    printf '{}\n' > rsctf-worker-agent-attestation.json
  )
}

run_fixture() {
  local fixture_directory="$1"
  local service_active="$2"
  local assertions="$3"

  docker run --rm \
    --env "RSCTF_TEST_SERVICE_ACTIVE=$service_active" \
    --env "RSCTF_TEST_SERVICE_ENABLED=${RSCTF_TEST_SERVICE_ENABLED:-0}" \
    --env "RSCTF_TEST_ATTESTATION_SUCCESS=${RSCTF_TEST_ATTESTATION_SUCCESS:-0}" \
    --env "RSCTF_TEST_FAIL_DAEMON_RELOADS=${RSCTF_TEST_FAIL_DAEMON_RELOADS:-0}" \
    --env "RSCTF_TEST_FAIL_ENABLES=${RSCTF_TEST_FAIL_ENABLES:-0}" \
    --env "RSCTF_TEST_FAIL_RESTARTS=${RSCTF_TEST_FAIL_RESTARTS:-0}" \
    --volume "$REPOSITORY_ROOT/scripts/install-worker.sh:/installer.sh:ro" \
    --volume "$fixture_directory:/fixture:ro" \
    --volume "$REPOSITORY_ROOT/scripts/test-worker-installer-shim.sh:/usr/local/sbin/wget:ro" \
    --volume "$REPOSITORY_ROOT/scripts/test-worker-installer-shim.sh:/usr/local/sbin/docker:ro" \
    --volume "$REPOSITORY_ROOT/scripts/test-worker-installer-shim.sh:/usr/local/sbin/gh:ro" \
    --volume "$REPOSITORY_ROOT/scripts/test-worker-installer-shim.sh:/usr/local/sbin/systemctl:ro" \
    "$TEST_IMAGE" \
    bash -ceu "$assertions"
}

readonly VALID_PACKAGE="$TEMP_DIRECTORY/valid-package"
readonly VALID_FIXTURE="$TEMP_DIRECTORY/valid-fixture"
make_package "$VALID_PACKAGE"
make_fixture "$VALID_FIXTURE" "$VALID_PACKAGE"

# On a Linux Docker host without systemd, auto mode imports the same verified
# static binary as a minimal local image. It must not create host accounts,
# service files, or a plaintext host state directory.
# shellcheck disable=SC2016
run_fixture "$VALID_FIXTURE" 0 '
  test ! -d /run/systemd/system
  dash /installer.sh --version v0.1.0 --skip-attestation --bootstrap \
    >/tmp/installer-output 2>&1

  grep -q "image imported as rsctf-worker-agent-local:0.1.0" \
    /tmp/installer-output
  grep -q "Docker-supervised service" /tmp/installer-output
  grep -q "^import " /tmp/docker.log
  grep -q "io.rsctf.worker.agent.image=true" /tmp/docker.log
  grep -q "rsctf-worker-agent-local:0.1.0$" /tmp/docker.log
  ! getent passwd rsctf-worker >/dev/null
  test ! -e /var/lib/rsctf-worker
  test ! -e /usr/local/bin/rsctf-worker-agent
  test ! -e /etc/systemd/system/rsctf-worker-agent.service
'

# The quoted body is evaluated by Bash inside the disposable container.
# shellcheck disable=SC2016
run_fixture "$VALID_FIXTURE" 0 '
  mkdir -p /run/systemd/system
  groupadd --system docker
  dash /installer.sh --version v0.1.0 --skip-attestation >/tmp/installer-output 2>&1

  worker_record="$(getent passwd rsctf-worker)"
  IFS=: read -r worker _ uid gid _ home shell <<< "$worker_record"
  group_record="$(getent group rsctf-worker)"
  IFS=: read -r _ _ group_gid _ <<< "$group_record"
  test "$worker" = rsctf-worker
  test "$uid" -ne 0
  test "$gid" = "$group_gid"
  test "$home" = /var/lib/rsctf-worker
  test "$shell" = /usr/sbin/nologin
  grep -qw docker <<< "$(id -nG rsctf-worker)"
  test "$(stat -c %a /var/lib/rsctf-worker)" = 700
  test "$(stat -c %a /usr/local/bin/rsctf-worker-agent)" = 755
  test "$(stat -c %a /etc/systemd/system/rsctf-worker-agent.service)" = 644
  ! compgen -G "/usr/local/bin/rsctf-worker-agent.rsctf-*" >/dev/null
  ! compgen -G "/etc/systemd/system/rsctf-worker-agent.service.rsctf-*" >/dev/null
  grep -q "^Wants=docker.service network-online.target$" /etc/systemd/system/rsctf-worker-agent.service
  grep -qx "enable rsctf-worker-agent.service" /tmp/systemctl.log
  ! grep -q "^restart " /tmp/systemctl.log
  test "$(wc -l < /tmp/wget.log)" -eq 2
  grep -c "^-q -S -T 30 -O " /tmp/wget.log | grep -qx 2
  ! grep -Eq -- "--https-only|--secure-protocol|--output-document" /tmp/wget.log
  grep -q -- "https://github.com/dimasma0305/rsctf/releases/download/v0.1.0/rsctf-worker-agent-linux-amd64.tar.gz$" /tmp/wget.log
  grep -q -- "https://github.com/dimasma0305/rsctf/releases/download/v0.1.0/SHA256SUMS$" /tmp/wget.log
'

# The latest-release lookup uses only the --spider and -S options supported by
# BusyBox wget, and accepts only the expected HTTPS GitHub tag redirect.
# shellcheck disable=SC2016
run_fixture "$VALID_FIXTURE" 0 '
  mkdir -p /run/systemd/system
  groupadd --system docker
  dash /installer.sh --skip-attestation >/tmp/installer-output 2>&1
  grep -qx -- "-q -S -T 30 --spider https://github.com/dimasma0305/rsctf/releases/latest" \
    /tmp/wget.log
  grep -q "Downloading rsctf-worker-agent-linux-amd64.tar.gz from v0.1.0" \
    /tmp/installer-output
'

# Public bootstrap mode retains verification and installation output without
# printing obsolete manual enrollment commands in the middle of the one-line flow.
# shellcheck disable=SC2016
run_fixture "$VALID_FIXTURE" 0 '
  mkdir -p /run/systemd/system
  groupadd --system docker
  dash /installer.sh --version v0.1.0 --skip-attestation --bootstrap \
    >/tmp/installer-output 2>&1
  grep -q "bootstrap will now validate Docker and enroll" /tmp/installer-output
  ! grep -q "Enroll this worker" /tmp/installer-output
  test -x /usr/local/bin/rsctf-worker-agent
'

# shellcheck disable=SC2016
RSCTF_TEST_ATTESTATION_SUCCESS=1 run_fixture "$VALID_FIXTURE" 0 '
  mkdir -p /run/systemd/system
  groupadd --system docker
  dash /installer.sh --version v0.1.0 >/tmp/installer-output 2>&1
  grep -q -- "--bundle .*rsctf-worker-agent-attestation.json" /tmp/gh.log
  grep -q -- "--hostname github.com" /tmp/gh.log
  grep -q -- "--repo dimasma0305/rsctf" /tmp/gh.log
  grep -q -- "--signer-workflow dimasma0305/rsctf/.github/workflows/worker-agent-release.yml" /tmp/gh.log
  grep -q -- "--source-ref refs/tags/v0.1.0" /tmp/gh.log
  grep -q -- "--deny-self-hosted-runners" /tmp/gh.log
  test "$(wc -l < /tmp/wget.log)" -eq 3
  grep -q -- "https://github.com/dimasma0305/rsctf/releases/download/v0.1.0/rsctf-worker-agent-attestation.json$" /tmp/wget.log
  test -x /usr/local/bin/rsctf-worker-agent
'

# shellcheck disable=SC2016
run_fixture "$VALID_FIXTURE" 1 '
  mkdir -p /run/systemd/system
  groupadd --system docker
  groupadd --system rsctf-worker
  useradd --system --gid rsctf-worker --home-dir /var/lib/rsctf-worker \
    --no-create-home --shell /usr/sbin/nologin rsctf-worker
  before="$(getent passwd rsctf-worker)"

  dash /installer.sh --version v0.1.0 --skip-attestation >/tmp/installer-output 2>&1

  test "$(getent passwd rsctf-worker)" = "$before"
  grep -qw docker <<< "$(id -nG rsctf-worker)"
  grep -qx "restart rsctf-worker-agent.service" /tmp/systemctl.log
'

assert_upgrade_rollback() {
  local failure_variable="$1"
  local service_was_enabled="${2:-1}"

  export RSCTF_TEST_SERVICE_ENABLED="$service_was_enabled"
  export "$failure_variable=1"
  # shellcheck disable=SC2016
  run_fixture "$VALID_FIXTURE" 1 '
    mkdir -p /run/systemd/system /usr/local/bin /etc/systemd/system \
      /usr/local/share/doc/rsctf-worker-agent
    groupadd --system docker
    groupadd --system rsctf-worker
    useradd --system --gid rsctf-worker --home-dir /var/lib/rsctf-worker \
      --no-create-home --shell /usr/sbin/nologin rsctf-worker
    printf "#!/usr/bin/env sh\necho old-worker\n" > /usr/local/bin/rsctf-worker-agent
    printf "old worker unit\n" > /etc/systemd/system/rsctf-worker-agent.service
    printf "old license\n" > /usr/local/share/doc/rsctf-worker-agent/LICENSE.txt
    printf "old notice\n" > /usr/local/share/doc/rsctf-worker-agent/NOTICE
    chmod 0750 /usr/local/bin/rsctf-worker-agent
    chmod 0600 /etc/systemd/system/rsctf-worker-agent.service
    chmod 0640 /usr/local/share/doc/rsctf-worker-agent/LICENSE.txt
    chmod 0600 /usr/local/share/doc/rsctf-worker-agent/NOTICE
    binary_before="$(sha256sum /usr/local/bin/rsctf-worker-agent)"
    unit_before="$(sha256sum /etc/systemd/system/rsctf-worker-agent.service)"
    license_before="$(sha256sum /usr/local/share/doc/rsctf-worker-agent/LICENSE.txt)"
    notice_before="$(sha256sum /usr/local/share/doc/rsctf-worker-agent/NOTICE)"

    if dash /installer.sh --version v0.1.0 --skip-attestation >/tmp/installer-output 2>&1; then
      printf "installer accepted a failed systemd activation\n" >&2
      exit 1
    fi

    grep -q "previous worker installation was restored" /tmp/installer-output
    test "$(sha256sum /usr/local/bin/rsctf-worker-agent)" = "$binary_before"
    test "$(sha256sum /etc/systemd/system/rsctf-worker-agent.service)" = "$unit_before"
    test "$(sha256sum /usr/local/share/doc/rsctf-worker-agent/LICENSE.txt)" = "$license_before"
    test "$(sha256sum /usr/local/share/doc/rsctf-worker-agent/NOTICE)" = "$notice_before"
    test "$(stat -c %a /usr/local/bin/rsctf-worker-agent)" = 750
    test "$(stat -c %a /etc/systemd/system/rsctf-worker-agent.service)" = 600
    test "$(stat -c %a /usr/local/share/doc/rsctf-worker-agent/LICENSE.txt)" = 640
    test "$(stat -c %a /usr/local/share/doc/rsctf-worker-agent/NOTICE)" = 600
    ! compgen -G "/usr/local/bin/rsctf-worker-agent.rsctf-*" >/dev/null
    ! compgen -G "/etc/systemd/system/rsctf-worker-agent.service.rsctf-*" >/dev/null
    grep -q "^daemon-reload$" /tmp/systemctl.log
    grep -q "^enable rsctf-worker-agent.service$" /tmp/systemctl.log
    if [[ "${RSCTF_TEST_FAIL_RESTARTS:-0}" == 1 ]]; then
      test "$(grep -c "^restart rsctf-worker-agent.service$" /tmp/systemctl.log)" -eq 2
      test "$(/usr/local/bin/rsctf-worker-agent)" = old-worker
      if [[ "${RSCTF_TEST_SERVICE_ENABLED:-0}" == 0 ]]; then
        grep -qx "disable rsctf-worker-agent.service" /tmp/systemctl.log
      fi
    fi
  '
  unset "$failure_variable"
  unset RSCTF_TEST_SERVICE_ENABLED
}

assert_upgrade_rollback RSCTF_TEST_FAIL_DAEMON_RELOADS
assert_upgrade_rollback RSCTF_TEST_FAIL_ENABLES
assert_upgrade_rollback RSCTF_TEST_FAIL_RESTARTS
assert_upgrade_rollback RSCTF_TEST_FAIL_RESTARTS 0

# A failed fresh activation leaves only the deliberately idempotent service
# identity/state for retry; every installed file and the new documentation
# directory are rolled back.
# shellcheck disable=SC2016
RSCTF_TEST_FAIL_ENABLES=1 run_fixture "$VALID_FIXTURE" 0 '
  mkdir -p /run/systemd/system
  groupadd --system docker

  if dash /installer.sh --version v0.1.0 --skip-attestation >/tmp/installer-output 2>&1; then
    printf "installer accepted a failed fresh activation\n" >&2
    exit 1
  fi

  grep -q "worker identity, Docker-group membership, and state directory are retained" /tmp/installer-output
  getent passwd rsctf-worker >/dev/null
  grep -qw docker <<< "$(id -nG rsctf-worker)"
  test -d /var/lib/rsctf-worker
  test ! -e /usr/local/bin/rsctf-worker-agent
  test ! -e /etc/systemd/system/rsctf-worker-agent.service
  test ! -e /usr/local/share/doc/rsctf-worker-agent
'

# shellcheck disable=SC2016
run_fixture "$VALID_FIXTURE" 0 '
  mkdir -p /run/systemd/system
  groupadd --system docker
  groupadd --system rsctf-worker
  useradd --system --gid rsctf-worker --home-dir /home/name-collision \
    --no-create-home --shell /bin/bash rsctf-worker
  before="$(getent passwd rsctf-worker)"

  if dash /installer.sh --version v0.1.0 --skip-attestation >/tmp/installer-output 2>&1; then
    printf "installer accepted a mismatched pre-existing account\n" >&2
    exit 1
  fi

  grep -q "refusing to grant Docker access" /tmp/installer-output
  test "$(getent passwd rsctf-worker)" = "$before"
  ! grep -qw docker <<< "$(id -nG rsctf-worker)"
  test ! -e /usr/local/bin/rsctf-worker-agent
'

readonly BAD_CHECKSUM_FIXTURE="$TEMP_DIRECTORY/bad-checksum-fixture"
mkdir -p "$BAD_CHECKSUM_FIXTURE"
install -m 0644 "$VALID_FIXTURE/$ASSET" "$BAD_CHECKSUM_FIXTURE/$ASSET"
printf '%064d  %s\n' 0 "$ASSET" > "$BAD_CHECKSUM_FIXTURE/SHA256SUMS"

run_fixture "$BAD_CHECKSUM_FIXTURE" 0 '
  mkdir -p /run/systemd/system
  groupadd --system docker
  if dash /installer.sh --version v0.1.0 --skip-attestation >/tmp/installer-output 2>&1; then
    printf "installer accepted a bad checksum\n" >&2
    exit 1
  fi
  grep -q "SHA-256 verification failed" /tmp/installer-output
  test ! -e /usr/local/bin/rsctf-worker-agent
'

run_fixture "$VALID_FIXTURE" 0 '
  mkdir -p /run/systemd/system
  groupadd --system docker
  if dash /installer.sh --version v0.1.0 >/tmp/installer-output 2>&1; then
    printf "installer accepted a failed artifact attestation\n" >&2
    exit 1
  fi
  grep -q "attestation verification failed" /tmp/installer-output
  test ! -e /usr/local/bin/rsctf-worker-agent
'

readonly LINK_PACKAGE="$TEMP_DIRECTORY/link-package"
readonly LINK_FIXTURE="$TEMP_DIRECTORY/link-fixture"
make_package "$LINK_PACKAGE"
rm "$LINK_PACKAGE/rsctf-worker-agent/NOTICE"
ln -s /etc/passwd "$LINK_PACKAGE/rsctf-worker-agent/NOTICE"
make_fixture "$LINK_FIXTURE" "$LINK_PACKAGE"

run_fixture "$LINK_FIXTURE" 0 '
  mkdir -p /run/systemd/system
  groupadd --system docker
  if dash /installer.sh --version v0.1.0 --skip-attestation >/tmp/installer-output 2>&1; then
    printf "installer accepted a link in the release archive\n" >&2
    exit 1
  fi
  grep -q "link or other unsupported entry type" /tmp/installer-output
  test ! -e /usr/local/bin/rsctf-worker-agent
'

printf 'Worker installer fixture tests passed.\n'
