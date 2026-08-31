#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

bash -n scripts/bounded-frontend.sh
output="$(RSCTF_BOUNDED_FRONTEND_DRY_RUN=1 scripts/bounded-frontend.sh check)"
node_dir="$(dirname "$(command -v node)")"

grep -Fq 'rsctf-build.lock' <<<"$output"
grep -Fq 'cpu_quota=150%' <<<"$output"
grep -Fq 'memory_max=8G' <<<"$output"
grep -Fq 'workers=2' <<<"$output"
grep -Fq "PATH=${node_dir}" <<<"$output"
grep -Fq 'GOMAXPROCS=2' <<<"$output"
grep -Fq 'UV_THREADPOOL_SIZE=2' <<<"$output"
grep -Eq '/pnpm check' <<<"$output"
grep -Fq -- '--scope' scripts/bounded-frontend.sh
grep -Fq 'cd "$repo_root/web"' scripts/bounded-frontend.sh

probe="$(
  RSCTF_BOUNDED_FRONTEND_PROBE=retained \
    scripts/bounded-frontend.sh exec node -e \
    "process.stdout.write('__rsctf_probe_' + (process.env.RSCTF_BOUNDED_FRONTEND_PROBE || 'missing') + '__')"
)"
[[ "$probe" = *'__rsctf_probe_retained__'* ]] || {
  echo 'bounded-frontend dropped caller environment inside its resource scope' >&2
  exit 1
}

if RSCTF_BOUNDED_FRONTEND_DRY_RUN=1 RSCTF_FRONTEND_WORKERS=8 \
  scripts/bounded-frontend.sh check >/dev/null 2>&1; then
  echo 'bounded-frontend accepted an unsafe worker count' >&2
  exit 1
fi

echo 'bounded-frontend contract: ok'
