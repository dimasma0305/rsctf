#!/usr/bin/env bash

set -euo pipefail

REPOSITORY_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly REPOSITORY_ROOT
readonly TEST_IMAGE="${RSCTF_INSTALLER_TEST_IMAGE:-ubuntu@sha256:4fbb8e6a8395de5a7550b33509421a2bafbc0aab6c06ba2cef9ebffbc7092d90}"

TEMP_DIRECTORY="$(mktemp -d)"
trap 'rm -rf -- "$TEMP_DIRECTORY"' EXIT

# A non-root POSIX-shell invocation must re-exec through an available privilege
# tool without putting enrollment credentials in that command line.
docker run --rm \
  --user 65534:65534 \
  --volume "$REPOSITORY_ROOT/scripts/bootstrap-worker.sh:/bootstrap.sh:ro" \
  --volume "$REPOSITORY_ROOT/scripts/test-worker-installer-shim.sh:/usr/local/sbin/sudo:ro" \
  "$TEST_IMAGE" \
  bash -ceu '
    status=0
    dash /bootstrap.sh --server-url https://ctf.example --version v0.1.0 ||
      status=$?
    test "$status" -eq 73
    grep -qx "sh /bootstrap.sh --server-url https://ctf.example --version v0.1.0" \
      /tmp/sudo.log
  '

run_uninstall_fixture() {
  local managed_containers="$1"
  local assertions="$2"

  docker run --rm \
    --env "RSCTF_TEST_MANAGED_CONTAINERS=$managed_containers" \
    --env RSCTF_TEST_OWNER_VOLUME=1 \
    --volume "$REPOSITORY_ROOT/scripts/bootstrap-worker.sh:/bootstrap.sh:ro" \
    --volume "$REPOSITORY_ROOT/scripts/test-worker-installer-shim.sh:/usr/local/sbin/docker:ro" \
    --volume "$REPOSITORY_ROOT/scripts/test-worker-installer-shim.sh:/usr/local/sbin/systemctl:ro" \
    "$TEST_IMAGE" \
    bash -ceu "$assertions"
}

# The quoted bodies are evaluated by Bash inside disposable containers.
# shellcheck disable=SC2016
run_uninstall_fixture 0 '
  groupadd --system rsctf-worker
  useradd --system --gid rsctf-worker --home-dir /var/lib/rsctf-worker \
    --no-create-home --shell /usr/sbin/nologin rsctf-worker
  mkdir -p /var/lib/rsctf-worker /usr/local/bin /etc/systemd/system \
    /usr/local/share/doc/rsctf-worker-agent
  touch /var/lib/rsctf-worker/worker.json \
    /usr/local/bin/rsctf-worker-agent \
    /etc/systemd/system/rsctf-worker-agent.service \
    /usr/local/share/doc/rsctf-worker-agent/LICENSE.txt

  printf "REMOVE\n" | script -qec "dash /bootstrap.sh --uninstall" /dev/null \
    >/tmp/uninstall-output 2>&1

  test ! -e /var/lib/rsctf-worker
  test ! -e /usr/local/bin/rsctf-worker-agent
  test ! -e /etc/systemd/system/rsctf-worker-agent.service
  test ! -e /usr/local/share/doc/rsctf-worker-agent
  ! getent passwd rsctf-worker >/dev/null
  ! getent group rsctf-worker >/dev/null
  grep -qx "volume rm rsctf-worker-owner" /tmp/docker.log
  grep -q "local identity were removed" /tmp/uninstall-output
'

# shellcheck disable=SC2016
run_uninstall_fixture 1 '
  mkdir -p /var/lib/rsctf-worker /usr/local/bin
  touch /var/lib/rsctf-worker/worker.json /usr/local/bin/rsctf-worker-agent

  if script -qec "dash /bootstrap.sh --uninstall" /dev/null \
      >/tmp/uninstall-output 2>&1; then
    printf "uninstall accepted a host with a managed workload\n" >&2
    exit 1
  fi
  test -e /var/lib/rsctf-worker/worker.json
  test -e /usr/local/bin/rsctf-worker-agent
  grep -q "managed containers or networks still exist" /tmp/uninstall-output
'

docker run --rm \
  --env RSCTF_TEST_HEALTHY=0 \
  --volume "$REPOSITORY_ROOT/scripts/bootstrap-worker.sh:/bootstrap.sh:ro" \
  --volume "$REPOSITORY_ROOT/scripts/test-worker-installer-shim.sh:/usr/local/sbin/wget:ro" \
  "$TEST_IMAGE" \
  bash -ceu '
    if dash /bootstrap.sh --server-url https://ctf.example --version v0.1.0 \
        >/tmp/bootstrap-output 2>&1; then
      printf "bootstrap accepted an unhealthy RSCTF server\n" >&2
      exit 1
    fi
    grep -q "download failed" /tmp/bootstrap-output
    test "$(wc -l < /tmp/wget.log)" -eq 1
    grep -q "https://ctf.example/healthz$" /tmp/wget.log
  '

# BusyBox-style wget flags must get far enough to produce the intended
# unsupported-host diagnostic in a minimal container. The internal container
# shell is not itself a persistent worker host.
docker run --rm \
  --env RSCTF_TEST_HEALTHY=1 \
  --volume "$REPOSITORY_ROOT/scripts/bootstrap-worker.sh:/bootstrap.sh:ro" \
  --volume "$REPOSITORY_ROOT/scripts/test-worker-installer-shim.sh:/usr/local/sbin/wget:ro" \
  "$TEST_IMAGE" \
  bash -ceu '
    if dash /bootstrap.sh --server-url https://ctf.example --version v0.1.0 \
        >/tmp/bootstrap-output 2>&1; then
      printf "bootstrap accepted a container without systemd\n" >&2
      exit 1
    fi
    grep -q "systemd is not active" /tmp/bootstrap-output
    grep -q "not a container or Docker Desktop internal VM" /tmp/bootstrap-output
    grep -q "^-q -S -T 30 -O " /tmp/wget.log
    ! grep -Eq -- "--https-only|--secure-protocol|--output-document" /tmp/wget.log
  '

readonly BOOTSTRAP_PACKAGE="$TEMP_DIRECTORY/package"
readonly BOOTSTRAP_FIXTURE="$TEMP_DIRECTORY/fixture"
mkdir -p "$BOOTSTRAP_PACKAGE/rsctf-worker-agent" "$BOOTSTRAP_FIXTURE"
printf '#!/usr/bin/env sh\nexit 0\n' > \
  "$BOOTSTRAP_PACKAGE/rsctf-worker-agent/rsctf-worker-agent"
chmod 0755 "$BOOTSTRAP_PACKAGE/rsctf-worker-agent/rsctf-worker-agent"
install -m 0644 \
  "$REPOSITORY_ROOT/agents/worker-agent/rsctf-worker-agent.service" \
  "$BOOTSTRAP_PACKAGE/rsctf-worker-agent/rsctf-worker-agent.service"
install -m 0644 "$REPOSITORY_ROOT/LICENSE.txt" \
  "$BOOTSTRAP_PACKAGE/rsctf-worker-agent/LICENSE.txt"
install -m 0644 "$REPOSITORY_ROOT/NOTICE" \
  "$BOOTSTRAP_PACKAGE/rsctf-worker-agent/NOTICE"
tar -C "$BOOTSTRAP_PACKAGE" -czf \
  "$BOOTSTRAP_FIXTURE/rsctf-worker-agent-linux-amd64.tar.gz" \
  rsctf-worker-agent
install -m 0755 "$REPOSITORY_ROOT/scripts/install-worker.sh" \
  "$BOOTSTRAP_FIXTURE/install-worker.sh"
(
  cd "$BOOTSTRAP_FIXTURE"
  sha256sum install-worker.sh rsctf-worker-agent-linux-amd64.tar.gz > SHA256SUMS
)

run_connection_fixture() {
  local connected="$1"
  local service_active="$2"
  local assertions="$3"

  docker run --rm \
    --env "RSCTF_TEST_CONTROL_CONNECTED=$connected" \
    --env "RSCTF_TEST_SERVICE_ACTIVE=$service_active" \
    --env RSCTF_TEST_WORKER_DIAGNOSTIC="fixture control connection refused" \
    --volume "$REPOSITORY_ROOT/scripts/bootstrap-worker.sh:/bootstrap.sh:ro" \
    --volume "$BOOTSTRAP_FIXTURE:/fixture:ro" \
    --volume "$REPOSITORY_ROOT/scripts/test-worker-installer-shim.sh:/usr/local/sbin/docker:ro" \
    --volume "$REPOSITORY_ROOT/scripts/test-worker-installer-shim.sh:/usr/local/sbin/journalctl:ro" \
    --volume "$REPOSITORY_ROOT/scripts/test-worker-installer-shim.sh:/usr/local/sbin/systemctl:ro" \
    --volume "$REPOSITORY_ROOT/scripts/test-worker-installer-shim.sh:/usr/local/sbin/wget:ro" \
    "$TEST_IMAGE" \
    bash -ceu "$assertions"
}

readonly PREPARE_ENROLLED_WORKER='
  mkdir -p /run/systemd/system /var/lib/rsctf-worker
  groupadd --system docker
  groupadd --system rsctf-worker
  useradd --system --gid rsctf-worker --groups docker \
    --home-dir /var/lib/rsctf-worker --no-create-home \
    --shell /usr/sbin/nologin rsctf-worker
  touch /var/lib/rsctf-worker/worker-key.pem \
    /var/lib/rsctf-worker/worker-cert.pem \
    /var/lib/rsctf-worker/worker-ca.pem \
    /var/lib/rsctf-worker/worker.json
  chown -R rsctf-worker:rsctf-worker /var/lib/rsctf-worker
  chmod 0700 /var/lib/rsctf-worker
  chmod 0600 /var/lib/rsctf-worker/*
'

# The bootstrap must not report success until the agent proves that the server
# accepted its mTLS control session and that state remains stable.
# shellcheck disable=SC2016
run_connection_fixture 1 1 "$PREPARE_ENROLLED_WORKER"'
  RSCTF_WORKER_CONNECTION_TIMEOUT_SECONDS=6 \
    dash /bootstrap.sh --server-url https://ctf.example --version v0.1.0 \
    >/tmp/bootstrap-output 2>&1
  grep -q "Worker health check passed: mTLS control session accepted" \
    /tmp/bootstrap-output
  grep -q "updated and restarted" /tmp/bootstrap-output
  test -f /run/rsctf-worker-agent/connected
  test "$(grep -c "^restart rsctf-worker-agent.service$" /tmp/systemctl.log)" -ge 1
'

# A live process without a server-accepted control session remains offline and
# makes the one-line installer fail with bounded diagnostics.
# shellcheck disable=SC2016
run_connection_fixture 0 1 "$PREPARE_ENROLLED_WORKER"'
  if RSCTF_WORKER_CONNECTION_TIMEOUT_SECONDS=1 \
      dash /bootstrap.sh --server-url https://ctf.example --version v0.1.0 \
      >/tmp/bootstrap-output 2>&1; then
    printf "bootstrap accepted a worker without an mTLS control session\n" >&2
    exit 1
  fi
  grep -q "health check timed out" /tmp/bootstrap-output
  grep -q "worker remains installed but offline" /tmp/bootstrap-output
  grep -q "fixture control connection refused" /tmp/bootstrap-output
'

# A process that exits shortly after systemd starts it must not be mistaken for
# a healthy worker.
# shellcheck disable=SC2016
run_connection_fixture 0 0 "$PREPARE_ENROLLED_WORKER"'
  if RSCTF_WORKER_CONNECTION_TIMEOUT_SECONDS=5 \
      dash /bootstrap.sh --server-url https://ctf.example --version v0.1.0 \
      >/tmp/bootstrap-output 2>&1; then
    printf "bootstrap accepted a stopped worker service\n" >&2
    exit 1
  fi
  grep -q "service stopped before connecting" /tmp/bootstrap-output
  grep -q "fixture control connection refused" /tmp/bootstrap-output
'

printf 'Worker bootstrap lifecycle tests passed.\n'
