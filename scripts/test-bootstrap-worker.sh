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

# Docker-supervised uninstall works without systemd and removes only objects
# carrying the exact RSCTF ownership labels.
docker run --rm \
  --env RSCTF_TEST_MANAGED_CONTAINERS=0 \
  --env RSCTF_TEST_OWNER_VOLUME=1 \
  --env RSCTF_TEST_STATE_VOLUME=1 \
  --env RSCTF_TEST_EXISTING_AGENT_CONTAINER=1 \
  --env RSCTF_TEST_WORKER_IMAGES=1 \
  --volume "$REPOSITORY_ROOT/scripts/bootstrap-worker.sh:/bootstrap.sh:ro" \
  --volume "$REPOSITORY_ROOT/scripts/test-worker-installer-shim.sh:/usr/local/sbin/docker:ro" \
  "$TEST_IMAGE" \
  bash -ceu '
    test ! -d /run/systemd/system
    printf "REMOVE\n" | script -qec "dash /bootstrap.sh --uninstall" /dev/null \
      >/tmp/uninstall-output 2>&1

    grep -qx "container rm --force rsctf-worker-agent" /tmp/docker.log
    grep -qx "volume rm rsctf-worker-state" /tmp/docker.log
    grep -qx "volume rm rsctf-worker-owner" /tmp/docker.log
    grep -q "^image rm sha256:aa" /tmp/docker.log
    grep -q "local identity were removed" /tmp/uninstall-output
  '

# A same-name foreign container is never deleted.
docker run --rm \
  --env RSCTF_TEST_MANAGED_CONTAINERS=0 \
  --env RSCTF_TEST_EXISTING_AGENT_CONTAINER=1 \
  --env RSCTF_TEST_AGENT_LABEL=false \
  --volume "$REPOSITORY_ROOT/scripts/bootstrap-worker.sh:/bootstrap.sh:ro" \
  --volume "$REPOSITORY_ROOT/scripts/test-worker-installer-shim.sh:/usr/local/sbin/docker:ro" \
  "$TEST_IMAGE" \
  bash -ceu '
    if script -qec "dash /bootstrap.sh --uninstall" /dev/null \
        >/tmp/uninstall-output 2>&1; then
      printf "uninstall removed an unlabeled container collision\n" >&2
      exit 1
    fi
    grep -q "without the RSCTF agent label" /tmp/uninstall-output
    ! grep -q "^container rm " /tmp/docker.log
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

# BusyBox-style wget flags must work before the bootstrap reports a genuinely
# missing runtime dependency in a minimal container.
docker run --rm \
  --env RSCTF_TEST_HEALTHY=1 \
  --volume "$REPOSITORY_ROOT/scripts/bootstrap-worker.sh:/bootstrap.sh:ro" \
  --volume "$REPOSITORY_ROOT/scripts/test-worker-installer-shim.sh:/usr/local/sbin/wget:ro" \
  "$TEST_IMAGE" \
  bash -ceu '
    if dash /bootstrap.sh --server-url https://ctf.example --version v0.1.0 \
        >/tmp/bootstrap-output 2>&1; then
      printf "bootstrap accepted a host without Docker\n" >&2
      exit 1
    fi
    grep -q "required command is missing: docker" /tmp/bootstrap-output
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

run_docker_connection_fixture() {
  local installation_status="$1"
  local connected="$2"
  local container_running="$3"
  local assertions="$4"

  docker run --rm \
    --env "RSCTF_TEST_INSTALLATION_STATUS=$installation_status" \
    --env "RSCTF_TEST_CONTROL_CONNECTED=$connected" \
    --env "RSCTF_TEST_CONTAINER_RUNNING=$container_running" \
    --env RSCTF_TEST_DOCTOR_SUCCESS=1 \
    --env RSCTF_TEST_WORKER_DIAGNOSTIC="fixture Docker control connection refused" \
    --volume "$REPOSITORY_ROOT/scripts/bootstrap-worker.sh:/bootstrap.sh:ro" \
    --volume "$BOOTSTRAP_FIXTURE:/fixture:ro" \
    --volume "$REPOSITORY_ROOT/scripts/test-worker-installer-shim.sh:/usr/local/sbin/docker:ro" \
    --volume "$REPOSITORY_ROOT/scripts/test-worker-installer-shim.sh:/usr/local/sbin/wget:ro" \
    "$TEST_IMAGE" \
    bash -ceu "$assertions"
}

# Without systemd, Docker provides restart supervision and the worker identity
# remains in a labeled named volume. The health gate still requires a stable,
# server-accepted mTLS session.
# shellcheck disable=SC2016
run_docker_connection_fixture enrolled 1 1 '
  test ! -d /run/systemd/system
  RSCTF_WORKER_CONNECTION_TIMEOUT_SECONDS=6 \
    dash /bootstrap.sh --server-url https://ctf.example --version v0.1.0 \
    >/tmp/bootstrap-output 2>&1

  grep -q "Selected docker worker service mode" /tmp/bootstrap-output
  grep -q "Worker health check passed: mTLS control session accepted" \
    /tmp/bootstrap-output
  grep -q "updated and restarted" /tmp/bootstrap-output
  grep -q "^import " /tmp/docker.log
  grep -q "^volume create --label io.rsctf.worker.state=true rsctf-worker-state$" \
    /tmp/docker.log
  grep -q -- "--restart unless-stopped" /tmp/docker.log
  grep -q -- "--network host" /tmp/docker.log
  grep -q -- "--cap-drop ALL" /tmp/docker.log
  grep -q -- "--security-opt no-new-privileges:true" /tmp/docker.log
  grep -q "src=rsctf-worker-state,dst=/var/lib/rsctf-worker" /tmp/docker.log
  grep -q "src=/var/run/docker.sock,dst=/var/run/docker.sock" /tmp/docker.log
  grep -q "src=/var/lib/docker,dst=/var/lib/docker,readonly" /tmp/docker.log
  test -f /tmp/rsctf-test-worker-state-volume
  test -f /tmp/rsctf-test-container-rsctf-worker-agent
  test ! -e /etc/systemd/system/rsctf-worker-agent.service
  test ! -e /usr/local/bin/rsctf-worker-agent
'

# A fresh Docker-supervised installation reads the token only from the
# controlling terminal, passes it on stdin, and never puts it in Docker argv.
# shellcheck disable=SC2016
run_docker_connection_fixture empty 1 1 '
  printf "DEDICATED\nfixture-secret-token\n" |
    script -qec \
      "RSCTF_WORKER_CONNECTION_TIMEOUT_SECONDS=6 dash /bootstrap.sh --server-url https://ctf.example --version v0.1.0" \
      /dev/null >/tmp/bootstrap-output 2>&1

  grep -q "installed, enrolled, and started successfully" /tmp/bootstrap-output
  grep -q " enroll --server-url https://ctf.example --token-stdin " /tmp/docker.log
  ! grep -q "fixture-secret-token" /tmp/docker.log
'

# A failed upgrade restores the previously supervised container instead of
# leaving the worker on a known-bad image.
# shellcheck disable=SC2016
run_docker_connection_fixture enrolled 0 1 '
  touch /tmp/rsctf-test-container-rsctf-worker-agent
  if RSCTF_WORKER_CONNECTION_TIMEOUT_SECONDS=1 \
      dash /bootstrap.sh --server-url https://ctf.example --version v0.1.0 \
      >/tmp/bootstrap-output 2>&1; then
    printf "bootstrap accepted an offline Docker-supervised worker\n" >&2
    exit 1
  fi

  grep -q "previous container was restored" /tmp/bootstrap-output
  grep -q "^rename rsctf-worker-agent rsctf-worker-agent-rollback-" \
    /tmp/docker.log
  grep -q "^rename rsctf-worker-agent-rollback-.* rsctf-worker-agent$" \
    /tmp/docker.log
  grep -q "fixture Docker control connection refused" /tmp/bootstrap-output
  test -f /tmp/rsctf-test-container-rsctf-worker-agent
'

printf 'Worker bootstrap lifecycle tests passed.\n'
