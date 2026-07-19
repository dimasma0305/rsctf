#!/usr/bin/env bash

set -euo pipefail

REPOSITORY_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly REPOSITORY_ROOT
TEMP_DIRECTORY="$(mktemp -d)"
readonly TEMP_DIRECTORY
trap 'rm -rf -- "$TEMP_DIRECTORY"' EXIT

readonly PINNED_IMAGE="ghcr.io/dimasma0305/rsctf@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
readonly FIXTURE_BASE="https://fixture.example/releases/download/v1.2.3"
readonly SHIM="$REPOSITORY_ROOT/scripts/test-install-shim.sh"
readonly BOOTSTRAP="$TEMP_DIRECTORY/bootstrap/install.sh"
readonly TEST_BIN="$TEMP_DIRECTORY/bin"

mkdir -p "${BOOTSTRAP%/*}" "$TEST_BIN"
install -m 0755 "$REPOSITORY_ROOT/scripts/install.sh" "$BOOTSTRAP"
for command in curl docker gh; do
  ln -s "$SHIM" "$TEST_BIN/$command"
done

make_fixture() {
  local fixture=$1 package="$TEMP_DIRECTORY/package"
  rm -rf -- "$package"
  mkdir -p "$fixture" "$package/rsctf/deploy/postgres/init" "$package/rsctf/scripts"
  install -m 0644 "$REPOSITORY_ROOT"/deploy/compose*.yml "$package/rsctf/deploy/"
  install -m 0644 \
    "$REPOSITORY_ROOT/deploy/Caddyfile" \
    "$REPOSITORY_ROOT/deploy/README.md" \
    "$REPOSITORY_ROOT/deploy/.env.example" \
    "$REPOSITORY_ROOT/deploy/.gitignore" \
    "$package/rsctf/deploy/"
  install -m 0644 \
    "$REPOSITORY_ROOT/deploy/postgres/init/00-pg-stat-statements.sql" \
    "$package/rsctf/deploy/postgres/init/"
  install -m 0755 "$REPOSITORY_ROOT/scripts/install.sh" "$package/rsctf/scripts/install.sh"
  printf 'RSCTF_RELEASE_VERSION=v1.2.3\nRSCTF_RELEASE_IMAGE=%s\n' "$PINNED_IMAGE" \
    > "$package/rsctf/deploy/release.env"
  tar -C "$package" -czf "$fixture/rsctf-deployment-bundle.tar.gz" rsctf
  (
    cd "$fixture"
    sha256sum rsctf-deployment-bundle.tar.gz > SHA256SUMS
    printf '{}\n' > rsctf-worker-agent-attestation.json
  )
}

run_installer() {
  local fixture=$1 target=$2 output=$3
  shift 3
  : > "$TEMP_DIRECTORY/curl.log"
  : > "$TEMP_DIRECTORY/gh.log"
  env \
    "${RSCTF_TEST_ENV_ARGS[@]}" \
    PATH="$TEST_BIN:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
    RSCTF_INSTALLER_FIXTURE="$fixture" \
    RSCTF_TEST_CURL_LOG="$TEMP_DIRECTORY/curl.log" \
    RSCTF_TEST_GH_LOG="$TEMP_DIRECTORY/gh.log" \
    bash "$BOOTSTRAP" \
      --install-dir "$target" \
      --mode local \
      --without-docker \
      --non-interactive \
      "$@" \
      > "$output" 2>&1
}

readonly VALID_FIXTURE="$TEMP_DIRECTORY/valid-fixture"
make_fixture "$VALID_FIXTURE"

local_checkout="$TEMP_DIRECTORY/local-checkout"
mkdir -p "$local_checkout/scripts"
cp -a "$REPOSITORY_ROOT/deploy" "$local_checkout/deploy"
install -m 0755 "$REPOSITORY_ROOT/scripts/install.sh" "$local_checkout/scripts/install.sh"
: > "$TEMP_DIRECTORY/local-curl.log"
env \
  PATH="$TEST_BIN:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
  RSCTF_INSTALLER_FIXTURE="$VALID_FIXTURE" \
  RSCTF_TEST_CURL_LOG="$TEMP_DIRECTORY/local-curl.log" \
  RSCTF_TEST_GH_LOG="$TEMP_DIRECTORY/gh.log" \
  bash "$local_checkout/scripts/install.sh" \
    --image "$PINNED_IMAGE" \
    --mode local \
    --without-docker \
    --non-interactive \
    --configure-only \
    > "$TEMP_DIRECTORY/local.out" 2>&1
grep -qx "RSCTF_IMAGE=${PINNED_IMAGE}" "$local_checkout/deploy/.env"
test ! -s "$TEMP_DIRECTORY/local-curl.log"
local_token="$(sed -n 's/^RSCTF_BOOTSTRAP_TOKEN=//p' "$local_checkout/deploy/.env")"
test "${#local_token}" -eq 64
! grep -Fq -- "$local_token" "$TEMP_DIRECTORY/local.out"
grep -Fq 'The first-administrator setup token is stored only in' \
  "$TEMP_DIRECTORY/local.out"

output="$TEMP_DIRECTORY/success.out"
target="$TEMP_DIRECTORY/installed-rsctf"
RSCTF_TEST_ENV_ARGS=()
run_installer "$VALID_FIXTURE" "$target" "$output" \
  --ref v1.2.3 \
  --bundle-url "$FIXTURE_BASE" \
  --skip-attestation
grep -qx "RSCTF_IMAGE=${PINNED_IMAGE}" "$target/deploy/.env"
test "$(stat -c %a "$target/deploy/.env")" = 600
token="$(sed -n 's/^RSCTF_BOOTSTRAP_TOKEN=//p' "$target/deploy/.env")"
test "${#token}" -eq 64
! grep -Fq -- "$token" "$output"
grep -Fq 'The first-administrator setup token is stored only in' "$output"
grep -Fq "sed -n 's/^RSCTF_BOOTSTRAP_TOKEN=//p'" "$output"
grep -Fq 'attestation verification was explicitly skipped' "$output"
test "$(wc -l < "$TEMP_DIRECTORY/curl.log")" -eq 2
grep -c -- "--proto =https .*--proto-redir =https .*--tlsv1.2 .*--max-time 300 .*--retry-max-time 300" \
  "$TEMP_DIRECTORY/curl.log" | grep -qx 2
! find "$TEMP_DIRECTORY" -maxdepth 1 -name '.rsctf-bootstrap.*' -print -quit | grep -q .

output="$TEMP_DIRECTORY/attested.out"
target="$TEMP_DIRECTORY/attested-rsctf"
RSCTF_TEST_ENV_ARGS=(RSCTF_TEST_GH_VERIFY=1)
run_installer "$VALID_FIXTURE" "$target" "$output" \
  --ref v1.2.3 \
  --bundle-url "$FIXTURE_BASE"
grep -Fq -- '--repo dimasma0305/rsctf' "$TEMP_DIRECTORY/gh.log"
grep -Fq -- '--signer-workflow dimasma0305/rsctf/.github/workflows/worker-agent-release.yml' \
  "$TEMP_DIRECTORY/gh.log"
grep -Fq -- '--source-ref refs/tags/v1.2.3' "$TEMP_DIRECTORY/gh.log"
grep -Fq -- '--deny-self-hosted-runners' "$TEMP_DIRECTORY/gh.log"
test "$(wc -l < "$TEMP_DIRECTORY/curl.log")" -eq 3

output="$TEMP_DIRECTORY/latest.out"
target="$TEMP_DIRECTORY/latest-rsctf"
mkdir "$target"
RSCTF_TEST_ENV_ARGS=(RSCTF_TEST_GH_VERIFY=1 RSCTF_TEST_LATEST_TAG=v1.2.3)
run_installer "$VALID_FIXTURE" "$target" "$output"
test -f "$target/deploy/compose.yml"
grep -Fq 'releases/latest' "$TEMP_DIRECTORY/curl.log"

bad_target="$TEMP_DIRECTORY/bad-latest"
RSCTF_TEST_ENV_ARGS=(RSCTF_TEST_GH_VERIFY=1 RSCTF_TEST_LATEST_TAG=main)
if run_installer "$VALID_FIXTURE" "$bad_target" "$TEMP_DIRECTORY/bad-latest.out"; then
  printf 'installer accepted a mutable latest-release redirect\n' >&2
  exit 1
fi
test ! -e "$bad_target"

readonly DUPLICATE_FIXTURE="$TEMP_DIRECTORY/duplicate-fixture"
cp -a "$VALID_FIXTURE" "$DUPLICATE_FIXTURE"
checksum_line="$(< "$DUPLICATE_FIXTURE/SHA256SUMS")"
printf '%s\n' "$checksum_line" >> "$DUPLICATE_FIXTURE/SHA256SUMS"
RSCTF_TEST_ENV_ARGS=()
if run_installer "$DUPLICATE_FIXTURE" "$TEMP_DIRECTORY/duplicate-target" \
  "$TEMP_DIRECTORY/duplicate.out" --ref v1.2.3 --bundle-url "$FIXTURE_BASE" --skip-attestation; then
  printf 'installer accepted duplicate deployment checksums\n' >&2
  exit 1
fi
test ! -e "$TEMP_DIRECTORY/duplicate-target"

readonly MANY_ENTRIES_FIXTURE="$TEMP_DIRECTORY/many-entries-fixture"
many_entries_package="$TEMP_DIRECTORY/many-entries-package"
mkdir -p "$MANY_ENTRIES_FIXTURE" "$many_entries_package"
tar -xzf "$VALID_FIXTURE/rsctf-deployment-bundle.tar.gz" -C "$many_entries_package"
for ((entry_index = 0; entry_index < 1100; entry_index += 1)); do
  : > "$many_entries_package/rsctf/deploy/empty-${entry_index}"
done
tar -C "$many_entries_package" \
  -czf "$MANY_ENTRIES_FIXTURE/rsctf-deployment-bundle.tar.gz" rsctf
(
  cd "$MANY_ENTRIES_FIXTURE"
  sha256sum rsctf-deployment-bundle.tar.gz > SHA256SUMS
)
if run_installer "$MANY_ENTRIES_FIXTURE" "$TEMP_DIRECTORY/many-entries-target" \
  "$TEMP_DIRECTORY/many-entries.out" --ref v1.2.3 --bundle-url "$FIXTURE_BASE" --skip-attestation; then
  printf 'installer accepted an archive with excessive zero-size entries\n' >&2
  exit 1
fi
grep -Fq 'contains more than 1024 entries' "$TEMP_DIRECTORY/many-entries.out"
test ! -e "$TEMP_DIRECTORY/many-entries-target"

readonly LARGE_STREAM_FIXTURE="$TEMP_DIRECTORY/large-stream-fixture"
large_stream_package="$TEMP_DIRECTORY/large-stream-package"
mkdir -p "$LARGE_STREAM_FIXTURE" "$large_stream_package"
tar -xzf "$VALID_FIXTURE/rsctf-deployment-bundle.tar.gz" -C "$large_stream_package"
truncate -s 135000000 "$large_stream_package/rsctf/deploy/oversized-zero-file"
tar -C "$large_stream_package" \
  -czf "$LARGE_STREAM_FIXTURE/rsctf-deployment-bundle.tar.gz" rsctf
(
  cd "$LARGE_STREAM_FIXTURE"
  sha256sum rsctf-deployment-bundle.tar.gz > SHA256SUMS
)
if run_installer "$LARGE_STREAM_FIXTURE" "$TEMP_DIRECTORY/large-stream-target" \
  "$TEMP_DIRECTORY/large-stream.out" --ref v1.2.3 --bundle-url "$FIXTURE_BASE" --skip-attestation; then
  printf 'installer accepted an oversized decompressed tar stream\n' >&2
  exit 1
fi
grep -Fq 'exceeds 128 MiB as a decompressed tar stream' \
  "$TEMP_DIRECTORY/large-stream.out"
test ! -e "$TEMP_DIRECTORY/large-stream-target"

readonly LINK_FIXTURE="$TEMP_DIRECTORY/link-fixture"
make_fixture "$LINK_FIXTURE"
link_package="$TEMP_DIRECTORY/link-package"
mkdir -p "$link_package/rsctf/deploy" "$link_package/rsctf/scripts"
ln -s /etc/passwd "$link_package/rsctf/deploy/release.env"
install -m 0644 "$REPOSITORY_ROOT/deploy/compose.yml" "$link_package/rsctf/deploy/compose.yml"
install -m 0755 "$REPOSITORY_ROOT/scripts/install.sh" "$link_package/rsctf/scripts/install.sh"
tar -C "$link_package" -czf "$LINK_FIXTURE/rsctf-deployment-bundle.tar.gz" rsctf
(
  cd "$LINK_FIXTURE"
  sha256sum rsctf-deployment-bundle.tar.gz > SHA256SUMS
)
if run_installer "$LINK_FIXTURE" "$TEMP_DIRECTORY/link-target" \
  "$TEMP_DIRECTORY/link.out" --ref v1.2.3 --bundle-url "$FIXTURE_BASE" --skip-attestation; then
  printf 'installer accepted a symlink in the deployment archive\n' >&2
  exit 1
fi
test ! -e "$TEMP_DIRECTORY/link-target"

readonly WRONG_ROOT_FIXTURE="$TEMP_DIRECTORY/wrong-root-fixture"
make_fixture "$WRONG_ROOT_FIXTURE"
wrong_package="$TEMP_DIRECTORY/wrong-package"
mkdir -p "$wrong_package/not-rsctf/deploy" "$wrong_package/not-rsctf/scripts"
install -m 0644 "$REPOSITORY_ROOT/deploy/compose.yml" \
  "$wrong_package/not-rsctf/deploy/compose.yml"
install -m 0755 "$REPOSITORY_ROOT/scripts/install.sh" \
  "$wrong_package/not-rsctf/scripts/install.sh"
printf 'RSCTF_RELEASE_VERSION=v1.2.3\nRSCTF_RELEASE_IMAGE=%s\n' "$PINNED_IMAGE" \
  > "$wrong_package/not-rsctf/deploy/release.env"
tar -C "$wrong_package" -czf "$WRONG_ROOT_FIXTURE/rsctf-deployment-bundle.tar.gz" \
  not-rsctf
(
  cd "$WRONG_ROOT_FIXTURE"
  sha256sum rsctf-deployment-bundle.tar.gz > SHA256SUMS
)
if run_installer "$WRONG_ROOT_FIXTURE" "$TEMP_DIRECTORY/wrong-root-target" \
  "$TEMP_DIRECTORY/wrong-root.out" --ref v1.2.3 --bundle-url "$FIXTURE_BASE" --skip-attestation; then
  printf 'installer accepted an archive outside the rsctf root\n' >&2
  exit 1
fi
test ! -e "$TEMP_DIRECTORY/wrong-root-target"

mkdir "$TEMP_DIRECTORY/nonempty-target"
printf 'preserve me\n' > "$TEMP_DIRECTORY/nonempty-target/user-file"
if run_installer "$VALID_FIXTURE" "$TEMP_DIRECTORY/nonempty-target" \
  "$TEMP_DIRECTORY/nonempty.out" --ref v1.2.3 --bundle-url "$FIXTURE_BASE" --skip-attestation; then
  printf 'installer overwrote a non-empty target\n' >&2
  exit 1
fi
grep -qx 'preserve me' "$TEMP_DIRECTORY/nonempty-target/user-file"

RSCTF_TEST_ENV_ARGS=(RSCTF_TEST_GH_VERIFY=0)
if run_installer "$VALID_FIXTURE" "$TEMP_DIRECTORY/unattested-target" \
  "$TEMP_DIRECTORY/unattested.out" --ref v1.2.3 --bundle-url "$FIXTURE_BASE"; then
  printf 'installer accepted a failed artifact attestation\n' >&2
  exit 1
fi
test ! -e "$TEMP_DIRECTORY/unattested-target"

printf 'Deployment installer bootstrap tests passed.\n'
