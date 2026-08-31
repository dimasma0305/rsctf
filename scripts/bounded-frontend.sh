#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: scripts/bounded-frontend.sh <pnpm arguments...>" >&2
  exit 2
}

[[ $# -gt 0 ]] || usage

repo_root="$(git rev-parse --show-toplevel)"
git_common_dir="$(git rev-parse --path-format=absolute --git-common-dir)"
[[ -n "$repo_root" && -d "$repo_root/web" ]] || {
  echo "bounded-frontend: could not resolve the repository web workspace" >&2
  exit 2
}
[[ -n "$git_common_dir" && -d "$git_common_dir" ]] || {
  echo "bounded-frontend: could not resolve the shared Git directory" >&2
  exit 2
}

workers="${RSCTF_FRONTEND_WORKERS:-2}"
cpu_quota="${RSCTF_FRONTEND_CPU_QUOTA:-150%}"
memory_max="${RSCTF_FRONTEND_MEMORY_MAX:-8G}"
lock_wait="${RSCTF_BUILD_LOCK_WAIT_SECONDS:-3600}"

[[ "$workers" =~ ^[1-4]$ ]] || {
  echo "bounded-frontend: RSCTF_FRONTEND_WORKERS must be an integer from 1 through 4" >&2
  exit 2
}
[[ "$cpu_quota" =~ ^([1-9][0-9]{0,2}|[1-3][0-9]{3}|4000)%$ ]] || {
  echo "bounded-frontend: RSCTF_FRONTEND_CPU_QUOTA must be 1% through 4000%" >&2
  exit 2
}
[[ "$memory_max" =~ ^[1-9][0-9]*(M|G)$ ]] || {
  echo "bounded-frontend: RSCTF_FRONTEND_MEMORY_MAX must be a positive M/G systemd size" >&2
  exit 2
}
[[ "$lock_wait" =~ ^[1-9][0-9]*$ ]] || {
  echo "bounded-frontend: RSCTF_BUILD_LOCK_WAIT_SECONDS must be a positive integer" >&2
  exit 2
}

pnpm_bin="$(command -v pnpm)"
[[ "$pnpm_bin" = /* && -x "$pnpm_bin" ]] || {
  echo "bounded-frontend: pnpm is not available as an executable absolute path" >&2
  exit 2
}
node_bin="$(command -v node)"
[[ "$node_bin" = /* && -x "$node_bin" ]] || {
  echo "bounded-frontend: node is not available as an executable absolute path" >&2
  exit 2
}
runtime_path="$(dirname "$node_bin"):$(dirname "$pnpm_bin"):/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
command=(
  env
  "PATH=${runtime_path}"
  "GOMAXPROCS=${workers}"
  "UV_THREADPOOL_SIZE=${workers}"
  "NODE_OPTIONS=--max-old-space-size=4096"
  "$pnpm_bin"
  "$@"
)
lock_path="${git_common_dir}/rsctf-build.lock"

if [[ "${RSCTF_BOUNDED_FRONTEND_DRY_RUN:-0}" == "1" ]]; then
  printf 'repo_root=%s\n' "$repo_root"
  printf 'lock=%s\n' "$lock_path"
  printf 'cpu_quota=%s\n' "$cpu_quota"
  printf 'memory_max=%s\n' "$memory_max"
  printf 'workers=%s\n' "$workers"
  printf 'command='
  printf '%q ' "${command[@]}"
  printf '\n'
  exit 0
fi

exec 9>"$lock_path"
if ! flock -w "$lock_wait" 9; then
  echo "bounded-frontend: another RSCTF build still owns the shared compile slot" >&2
  exit 75
fi

cd "$repo_root/web"
if command -v systemd-run >/dev/null 2>&1 \
  && [[ -d /run/systemd/system ]] \
  && systemctl show-environment >/dev/null 2>&1; then
  unit="rsctf-frontend-$BASHPID-$(date +%s)"
  systemd-run \
    --quiet \
    --wait \
    --collect \
    --pipe \
    --working-directory "$repo_root/web" \
    --unit "$unit" \
    --property Type=exec \
    --property "CPUQuota=${cpu_quota}" \
    --property CPUWeight=20 \
    --property "MemoryMax=${memory_max}" \
    --property IOWeight=20 \
    --property Nice=10 \
    --property TasksMax=256 \
    --property OOMPolicy=stop \
    "${command[@]}"
else
  echo "bounded-frontend: systemd unavailable; using soft nice/ionice limits" >&2
  nice -n 10 ionice -c 2 -n 7 "${command[@]}"
fi
