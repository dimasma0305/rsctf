#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: scripts/bounded-cargo.sh <cargo arguments...>" >&2
  exit 2
}

[[ $# -gt 0 ]] || usage

repo_root="$(git rev-parse --show-toplevel)"
git_common_dir="$(git rev-parse --path-format=absolute --git-common-dir)"
[[ -n "$repo_root" && -d "$repo_root" ]] || {
  echo "bounded-cargo: could not resolve the repository root" >&2
  exit 2
}
[[ -n "$git_common_dir" && -d "$git_common_dir" ]] || {
  echo "bounded-cargo: could not resolve the shared Git directory" >&2
  exit 2
}

jobs="${RSCTF_CARGO_JOBS:-2}"
cpu_quota="${RSCTF_CARGO_CPU_QUOTA:-200%}"
memory_max="${RSCTF_CARGO_MEMORY_MAX:-12G}"
lock_wait="${RSCTF_CARGO_LOCK_WAIT_SECONDS:-3600}"
target_dir="${RSCTF_CARGO_TARGET_DIR:-${git_common_dir}/rsctf-target}"

[[ "$jobs" =~ ^[1-4]$ ]] || {
  echo "bounded-cargo: RSCTF_CARGO_JOBS must be an integer from 1 through 4" >&2
  exit 2
}
[[ "$cpu_quota" =~ ^([1-9][0-9]{0,2}|[1-3][0-9]{3}|4000)%$ ]] || {
  echo "bounded-cargo: RSCTF_CARGO_CPU_QUOTA must be 1% through 4000%" >&2
  exit 2
}
[[ "$memory_max" =~ ^[1-9][0-9]*(M|G)$ ]] || {
  echo "bounded-cargo: RSCTF_CARGO_MEMORY_MAX must be a positive M/G systemd size" >&2
  exit 2
}
[[ "$lock_wait" =~ ^[1-9][0-9]*$ ]] || {
  echo "bounded-cargo: RSCTF_CARGO_LOCK_WAIT_SECONDS must be a positive integer" >&2
  exit 2
}
[[ "$target_dir" = /* ]] || {
  echo "bounded-cargo: RSCTF_CARGO_TARGET_DIR must be an absolute path" >&2
  exit 2
}

command=(
  env
  "CARGO_BUILD_JOBS=${jobs}"
  "CARGO_TARGET_DIR=${target_dir}"
  "RAYON_NUM_THREADS=${jobs}"
)
cargo_bin="$(command -v cargo)"
[[ "$cargo_bin" = /* && -x "$cargo_bin" ]] || {
  echo "bounded-cargo: cargo is not available as an executable absolute path" >&2
  exit 2
}
if command -v sccache >/dev/null 2>&1; then
  command+=(
    "RUSTC_WRAPPER=$(command -v sccache)"
    "SCCACHE_CACHE_SIZE=${RSCTF_SCCACHE_SIZE:-20G}"
    "SCCACHE_DIR=${git_common_dir}/rsctf-sccache"
  )
fi
command+=("$cargo_bin" "$@")

if [[ "${RSCTF_BOUNDED_CARGO_DRY_RUN:-0}" == "1" ]]; then
  printf 'repo_root=%s\n' "$repo_root"
  printf 'lock=%s\n' "${git_common_dir}/rsctf-build.lock"
  printf 'cpu_quota=%s\n' "$cpu_quota"
  printf 'memory_max=%s\n' "$memory_max"
  printf 'jobs=%s\n' "$jobs"
  printf 'target_dir=%s\n' "$target_dir"
  printf 'command='
  printf '%q ' "${command[@]}"
  printf '\n'
  exit 0
fi

mkdir -p -- "$target_dir"
lock_path="${git_common_dir}/rsctf-build.lock"
cd "$repo_root"
if command -v systemd-run >/dev/null 2>&1 \
  && [[ -d /run/systemd/system ]] \
  && systemctl show-environment >/dev/null 2>&1; then
  unit="rsctf-cargo-$BASHPID-$(date +%s)"
  # A scope keeps the caller environment (including disposable test service
  # URLs) without copying credentials into systemd-run command arguments.
  systemd-run \
    --quiet \
    --scope \
    --collect \
    --unit "$unit" \
    --property "CPUQuota=${cpu_quota}" \
    --property CPUWeight=20 \
    --property "MemoryMax=${memory_max}" \
    --property IOWeight=20 \
    --property TasksMax=512 \
    --property OOMPolicy=stop \
    flock -E 75 -w "$lock_wait" "$lock_path" \
      nice -n 10 ionice -c 2 -n 7 "${command[@]}"
else
  echo "bounded-cargo: systemd unavailable; using soft nice/ionice limits" >&2
  flock -E 75 -w "$lock_wait" "$lock_path" \
    nice -n 10 ionice -c 2 -n 7 "${command[@]}"
fi
