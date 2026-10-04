#!/usr/bin/env bash
# Bounded audit of Docker daemon load and long-lived API clients.
#
# The 2026-09-02 host incident reached roughly 93% aggregate CPU while rsctf
# used about 2%: dockerd spent several cores re-reading JSON logs at EOF for
# seven abandoned `docker logs` clients, some attached to deleted containers.
# This script only observes. It never kills a client and never restarts Docker;
# recovery is a deliberate operator action described in
# docs/reference/troubleshooting.md ("Docker daemon busy with abandoned clients").
#
# Usage: scripts/docker-client-audit.sh [--cpu-warn PERCENT] [--client-warn SECONDS]
set -Eeuo pipefail

cpu_warn=150   # dockerd %CPU across all threads that warrants attention
client_warn=600 # seconds a docker CLI client may stay attached before it is suspicious
while (($# > 0)); do
  case "$1" in
    --cpu-warn) cpu_warn="$2"; shift 2 ;;
    --client-warn) client_warn="$2"; shift 2 ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done
[[ "$cpu_warn" =~ ^[0-9]+$ && "$client_warn" =~ ^[0-9]+$ ]] || {
  echo "thresholds must be non-negative integers" >&2
  exit 2
}

warnings=0
daemon_pid="$(pgrep -o -x dockerd || true)"
if [[ -z "$daemon_pid" ]]; then
  echo "dockerd: not running (or not visible from this namespace)"
  exit 3
fi

# 1. Daemon CPU, sampled over two seconds so a one-off spike does not alert.
cpu_sample() { ps -o %cpu= -p "$daemon_pid" | tr -d ' ' | cut -d. -f1; }
cpu_a="$(cpu_sample)"; sleep 2; cpu_b="$(cpu_sample)"
daemon_cpu=$(( (cpu_a + cpu_b) / 2 ))
echo "dockerd pid ${daemon_pid}: ~${daemon_cpu}% CPU (ps %cpu average of two samples)"
if (( daemon_cpu >= cpu_warn )); then
  echo "  WARN: dockerd CPU is at or above ${cpu_warn}%"
  warnings=$((warnings + 1))
fi

# 2. Long-lived docker CLI clients. `docker logs` without --tail/--since,
#    `docker stats`, `docker events` and `docker attach` are the usual culprits.
echo "docker CLI clients older than ${client_warn}s:"
found=0
while read -r pid etimes args; do
  [[ -z "$pid" ]] && continue
  if (( etimes >= client_warn )); then
    found=$((found + 1))
    printf '  pid %s  age %ss  %s\n' "$pid" "$etimes" "${args:0:140}"
  fi
done < <(ps -eo pid=,etimes=,args= | awk '$3 ~ /(^|\/)docker$/ && ($4 ~ /^(logs|stats|events|attach|wait)$/)' || true)
if (( found > 0 )); then
  echo "  WARN: ${found} long-lived client(s); confirm each owner before stopping it"
  warnings=$((warnings + 1))
else
  echo "  none"
fi

# 3. Deleted-container log descriptors still held open by the daemon.
deleted="$(ls -l "/proc/${daemon_pid}/fd" 2>/dev/null | grep -c -- '-json.log (deleted)' || true)"
echo "deleted container log files still open by dockerd: ${deleted}"
if (( deleted > 0 )); then
  echo "  WARN: a log reader is attached to a removed container; find its client above"
  warnings=$((warnings + 1))
fi

# 4. Unix-socket connections to the daemon, as a coarse client count.
sockets="$(ss -xp 2>/dev/null | grep -c 'docker.sock' || true)"
echo "open docker.sock connections: ${sockets}"

if (( warnings > 0 )); then
  echo "result: ${warnings} warning(s); see docs/reference/troubleshooting.md"
  exit 1
fi
echo "result: ok"
