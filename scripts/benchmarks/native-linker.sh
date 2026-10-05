#!/usr/bin/env bash
# Runs only in a disposable, resource-bounded native release-builder container.
set -euo pipefail
[[ "$(uname -m)" == aarch64 ]] || { echo 'Native ARM64 required' >&2; exit 2; }
: "${RSCTF_LINK_BENCH_DIR:?}"
: "${RSCTF_LINK_BENCH_LLD_DIR:?}"
mkdir "$RSCTF_LINK_BENCH_DIR" "$RSCTF_LINK_BENCH_LLD_DIR"
rustc -Vv > "$RSCTF_LINK_BENCH_DIR/rustc.txt"
rustc -V | grep -q '^rustc 1\.97\.1 '
sysroot="$(rustc --print sysroot)"
bundled_lld="$sysroot/lib/rustlib/aarch64-unknown-linux-gnu/bin/rust-lld"
test -x "$bundled_lld"
ln -s "$bundled_lld" "$RSCTF_LINK_BENCH_LLD_DIR/ld.lld"
"$RSCTF_LINK_BENCH_LLD_DIR/ld.lld" --version > "$RSCTF_LINK_BENCH_DIR/lld.txt"
ld --version > "$RSCTF_LINK_BENCH_DIR/ld.txt"
sha256sum Cargo.toml Cargo.lock build.rs scripts/benchmarks/linker-driver.sh > "$RSCTF_LINK_BENCH_DIR/inputs.sha256"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="$PWD/scripts/benchmarks/linker-driver.sh"
TIMEFORMAT='%R'
{ time scripts/bounded-cargo.sh build --release --locked --bin rsctf --timings; } \
  2> "$RSCTF_LINK_BENCH_DIR/build.seconds.log"
test -d "$RSCTF_LINK_BENCH_DIR/server-links"
reference="$("$RSCTF_CARGO_TARGET_DIR/release/rsctf" challenge check --version)"
for binary in "$RSCTF_LINK_BENCH_DIR"/server-links/*.bin; do
  test "$("$binary" challenge check --version)" = "$reference"
  "$binary" challenge check --deny-warnings examples/challenge-repository > "${binary}.validation.log"
  readelf -h -l -d "$binary" > "${binary}.elf.txt"
done
printf '%s\n' "$reference" > "$RSCTF_LINK_BENCH_DIR/verified-version.txt"
for timing in "$RSCTF_LINK_BENCH_DIR"/server-links/*.seconds.log; do
  printf '%s: ' "${timing##*/}"
  tail -n 1 "$timing"
done
