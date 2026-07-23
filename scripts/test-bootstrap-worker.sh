#!/usr/bin/env bash

set -euo pipefail

REPOSITORY_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly REPOSITORY_ROOT
readonly TEST_IMAGE="${RSCTF_INSTALLER_TEST_IMAGE:-ubuntu@sha256:4fbb8e6a8395de5a7550b33509421a2bafbc0aab6c06ba2cef9ebffbc7092d90}"

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

  printf "REMOVE\n" | script -qec "bash /bootstrap.sh --uninstall" /dev/null \
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

  if script -qec "bash /bootstrap.sh --uninstall" /dev/null \
      >/tmp/uninstall-output 2>&1; then
    printf "uninstall accepted a host with a managed workload\n" >&2
    exit 1
  fi
  test -e /var/lib/rsctf-worker/worker.json
  test -e /usr/local/bin/rsctf-worker-agent
  grep -q "managed containers or networks still exist" /tmp/uninstall-output
'

printf 'Worker bootstrap uninstall tests passed.\n'
