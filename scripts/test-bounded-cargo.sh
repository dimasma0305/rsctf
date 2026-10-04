#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

bash -n scripts/bounded-cargo.sh
output="$(RSCTF_BOUNDED_CARGO_DRY_RUN=1 scripts/bounded-cargo.sh check --all-targets)"

grep -Fq 'cpu_quota=200%' <<<"$output"
grep -Fq 'rsctf-build.lock' <<<"$output"
grep -Fq 'memory_max=12G' <<<"$output"
grep -Fq 'jobs=2' <<<"$output"
grep -Fq '/rsctf-target' <<<"$output"
grep -Eq '/cargo check --all-targets' <<<"$output"
grep -Fq -- '--scope' scripts/bounded-cargo.sh
grep -Fq 'flock -E 75 -w "$lock_wait" "$lock_path"' scripts/bounded-cargo.sh
if grep -Fq -- '--pipe' scripts/bounded-cargo.sh; then
  echo 'bounded-cargo service mode can discard caller environment variables' >&2
  exit 1
fi

if RSCTF_BOUNDED_CARGO_DRY_RUN=1 RSCTF_CARGO_JOBS=8 \
  scripts/bounded-cargo.sh check >/dev/null 2>&1; then
  echo 'bounded-cargo accepted an unsafe job count' >&2
  exit 1
fi

if RSCTF_BOUNDED_CARGO_DRY_RUN=1 RSCTF_CARGO_TARGET_DIR=relative \
  scripts/bounded-cargo.sh check >/dev/null 2>&1; then
  echo 'bounded-cargo accepted a relative shared target directory' >&2
  exit 1
fi

# Coverage tools select their own rustc wrapper. An explicitly empty value is
# Cargo's supported way to disable wrappers and must also remain untouched.
for wrapper in /bounded-cargo-test/coverage-wrapper ''; do
  output="$(
    # Make the conflict reproducible even on hosts without sccache installed.
    sccache() { return 99; }
    export -f sccache
    RUSTC_WRAPPER="$wrapper" RSCTF_BOUNDED_CARGO_DRY_RUN=1 \
      scripts/bounded-cargo.sh check --all-targets
  )"
  if grep -Fq 'RUSTC_WRAPPER=' <<<"$output"; then
    echo 'bounded-cargo replaced an explicitly selected rustc wrapper' >&2
    exit 1
  fi
  grep -Fq 'cpu_quota=200%' <<<"$output"
  grep -Fq 'memory_max=12G' <<<"$output"
  grep -Fq 'rsctf-build.lock' <<<"$output"
done

output="$(
  sccache() { return 99; }
  export -f sccache
  unset RUSTC_WRAPPER
  RSCTF_BOUNDED_CARGO_DRY_RUN=1 scripts/bounded-cargo.sh check
)"
grep -Fq 'RUSTC_WRAPPER=sccache' <<<"$output"

echo 'bounded-cargo contract: ok'
