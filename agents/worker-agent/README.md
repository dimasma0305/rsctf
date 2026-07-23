# RSCTF trusted worker agent

The worker agent connects a dedicated Docker host or VM outbound to RSCTF. It
does not open a public listener and does not expose the Docker API through the
tunnel. Tagged releases publish static Linux AMD64/ARM64 archives and a Windows
AMD64 archive for native Windows-container hosts.

The service account's access to the Docker socket (normally through the
`docker` group) is host-root-equivalent. A compromised agent can control the
entire Docker host despite the systemd sandbox, so every production worker must
be a dedicated event host or VM with no unrelated workloads or secrets.

## Install

After creating a worker in `/admin/workers`, the admin page shows a public
bootstrap command and the separate one-use enrollment token. The command
installs the verified release and reads the token from a hidden terminal
prompt, so the secret never enters the URL, shell history, or process list:

```sh
(t=$(mktemp) || exit 1; trap 'rm -f "$t"' 0 HUP INT TERM; wget -q -T 30 -O "$t" https://ctf.example/install/worker && sh "$t" --server-url https://ctf.example)
```

On a native Windows-container host, use Administrator PowerShell:

```powershell
& ([scriptblock]::Create((Invoke-RestMethod https://ctf.example/install/worker.ps1))) -ServerUrl https://ctf.example
```

Downloading or running the bootstrap grants no worker access without a valid
15-minute enrollment token. The command requires a tagged release, Docker, and
a dedicated worker host. Linux additionally requires systemd. Its installer
runs with POSIX `sh`, uses only the download flags shared by GNU and BusyBox
`wget`, and elevates through `sudo` or `doas` when needed. Neither one-line
bootstrap requires the GitHub CLI. A fresh enrollment requires typing
`DEDICATED` before the hidden token prompt; do not bypass this boundary on a
daily-use computer or a machine containing unrelated secrets.

The one-command bootstrap downloads the latest tagged release over HTTPS and
verifies the worker archive against that release's `SHA256SUMS`. For an
independent GitHub Actions provenance check, use the advanced flow below from a
trusted admin workstation with a current `gh attestation verify`; no GitHub
login or token is needed because the release carries its attestation bundle:

```bash
(
  set -euo pipefail
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  curl_args=(--disable --fail --silent --show-error --location \
    --proto '=https' --proto-redir '=https' --tlsv1.2 --connect-timeout 15 \
    --max-time 300 --retry 5 --retry-all-errors --retry-max-time 300 \
    --speed-limit 1024 --speed-time 30)
  version="$(curl "${curl_args[@]}" --max-filesize 1048576 \
    -o /dev/null -w '%{url_effective}' \
    https://github.com/dimasma0305/rsctf/releases/latest)"
  version="${version##*/}"
  [[ "$version" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]
  base="https://github.com/dimasma0305/rsctf/releases/download/${version}"
  curl "${curl_args[@]}" --max-filesize 1048576 \
    -o "$tmp/install-worker.sh" "$base/install-worker.sh"
  curl "${curl_args[@]}" --max-filesize 16777216 \
    -o "$tmp/attestation.json" \
    "$base/rsctf-worker-agent-attestation.json"
  gh attestation verify "$tmp/install-worker.sh" \
    --bundle "$tmp/attestation.json" \
    --hostname github.com \
    --repo dimasma0305/rsctf \
    --signer-workflow dimasma0305/rsctf/.github/workflows/worker-agent-release.yml \
    --source-ref "refs/tags/$version" \
    --deny-self-hosted-runners
  sudo sh "$tmp/install-worker.sh" --version "$version"
)
```

On a later upgrade, the installer restarts the service only when it was already
active; a fresh installation remains stopped until enrollment succeeds.

For a reproducibly pinned installation, download and attest the installer from
the same release as the binary it will install:

```bash
VERSION=vX.Y.Z
(
  set -euo pipefail
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  curl_args=(--disable --fail --silent --show-error --location \
    --proto '=https' --proto-redir '=https' --tlsv1.2 --connect-timeout 15 \
    --max-time 300 --retry 5 --retry-all-errors --retry-max-time 300 \
    --speed-limit 1024 --speed-time 30)
  base="https://github.com/dimasma0305/rsctf/releases/download/${VERSION}"
  curl "${curl_args[@]}" --max-filesize 1048576 \
    -o "$tmp/install-worker.sh" "$base/install-worker.sh"
  curl "${curl_args[@]}" --max-filesize 16777216 \
    -o "$tmp/attestation.json" \
    "$base/rsctf-worker-agent-attestation.json"
  gh attestation verify "$tmp/install-worker.sh" \
    --bundle "$tmp/attestation.json" \
    --hostname github.com \
    --repo dimasma0305/rsctf \
    --signer-workflow dimasma0305/rsctf/.github/workflows/worker-agent-release.yml \
    --source-ref "refs/tags/$VERSION" \
    --deny-self-hosted-runners
  less "$tmp/install-worker.sh"
  sudo sh "$tmp/install-worker.sh" --version "$VERSION"
)
```

Release assets appear on the
[Releases page](https://github.com/dimasma0305/rsctf/releases); an untagged
checkout does not have downloadable binaries. The installer source is
[`scripts/install-worker.sh`](../../scripts/install-worker.sh). The explicit
`--skip-attestation` escape hatch weakens release authentication and is intended
only for controlled recovery or development environments.

## Enroll

On a trusted admin workstation, create a worker with
`POST /api/admin/workers`, then extract the one-use secret from
`.enrollment.token`:

```bash
set -o pipefail
read -rsp 'RSCTF admin token: ' RSCTF_ADMIN_TOKEN
printf '\n'
printf 'Authorization: Bearer %s\n' "$RSCTF_ADMIN_TOKEN" |
  curl --fail-with-body \
    --request POST \
    --header @- \
    --header 'Content-Type: application/json' \
    --data '{"name":"linux-event-host"}' \
    https://ctf.example/api/admin/workers |
  jq -er '.enrollment.token'
unset RSCTF_ADMIN_TOKEN
```

Copy only that short-lived token to the worker through a secure operator
channel. Never put `RSCTF_ADMIN_TOKEN` on the worker. After the installer has
created the service account, enter the token at the hidden prompt:

```bash
sudo -v
read -rsp 'One-time enrollment token: ' ONE_TIME_TOKEN
printf '\n'
printf '%s\n' "$ONE_TIME_TOKEN" |
  sudo -n -u rsctf-worker -- /usr/local/bin/rsctf-worker-agent enroll \
    --server-url https://ctf.example \
    --token-stdin \
    --state-dir /var/lib/rsctf-worker
unset ONE_TIME_TOKEN

sudo systemctl enable --now rsctf-worker-agent
```

Enrollment generates the private key on the worker and submits only a CSR. On Unix,
the key is created with mode `0600`. `--token-stdin` keeps the one-time secret out of
the process command line; `--token-file` is also available for a protected temporary
file. The enrollment token is not saved or printed. If root must perform enrollment,
pass `--unix-service-uid "$(id -u rsctf-worker)"`; otherwise root-owned state will not
be usable by the dedicated service account.

## Run

```sh
sudo -u rsctf-worker /usr/local/bin/rsctf-worker-agent run \
  --config /var/lib/rsctf-worker/worker.json \
  --accept-host-network-boundary
```

The installed systemd unit supplies those production arguments. Check it with
`systemctl status rsctf-worker-agent` or follow logs with
`journalctl -u rsctf-worker-agent --follow`. Installed Linux and Windows services
also pass a protected `--ready-file`. The agent creates that marker only after the
server accepts its mTLS control session and removes it when disconnected, allowing
the one-line installers to distinguish a truly online worker from a process that
is only running or retrying.

## Manual verification and source build

Tagged releases use the stable installer assets `install-worker.sh` and
`install-worker.ps1`, local
attestation bundle `rsctf-worker-agent-attestation.json`, and archive names
`rsctf-worker-agent-linux-amd64.tar.gz` and
`rsctf-worker-agent-linux-arm64.tar.gz`, plus
`rsctf-worker-agent-windows-amd64.zip`. Verify a downloaded archive against
the release's `SHA256SUMS` and its build provenance:

```bash
VERSION=vX.Y.Z
ASSET=rsctf-worker-agent-linux-amd64.tar.gz
BUNDLE=rsctf-worker-agent-attestation.json
awk -v asset="$ASSET" '$2 == asset { print }' SHA256SUMS | sha256sum --check -
gh attestation verify rsctf-worker-agent-linux-amd64.tar.gz \
  --bundle "$BUNDLE" \
  --hostname github.com \
  --repo dimasma0305/rsctf \
  --signer-workflow dimasma0305/rsctf/.github/workflows/worker-agent-release.yml \
  --source-ref "refs/tags/$VERSION" \
  --deny-self-hosted-runners
```

As a fallback, install Rust plus a musl toolchain and build the target matching
the Linux host:

```bash
# Use aarch64-unknown-linux-musl on ARM64.
TARGET=x86_64-unknown-linux-musl
rustup target add "$TARGET"
cargo build --manifest-path agents/worker-agent/Cargo.toml \
  --release --target "$TARGET" --locked
```

The complete deployment, checksum, account, and enrollment instructions are in
the [trusted-worker guide](../../docs/deploy/workers.md).

The default Docker transport is `/var/run/docker.sock`; a custom local Unix
socket can be set with `--docker-endpoint`. Remote unauthenticated Docker TCP
endpoints are deliberately not supported.

Capacity is detected from Docker with host headroom. It can be pinned using
`--cpu-millis`, `--memory-bytes`, and `--slots`; overrides can reserve headroom
but cannot advertise more than the detected safe CPU, memory, or isolated
workload-network capacity. Replica capacity is advertised separately from the
smallest detected Docker network endpoint budget.
Placement labels use repeated `--label key=value` arguments. Production also
requires a quota-capable writable layer (`overlay2` on XFS with `pquota` on
Linux or `windowsfilter` on Windows); `--allow-unbounded-storage` is only for
trusted disposable tests.

The agent requires TLS 1.3 mutual authentication and distinct control/data ALPNs. A
bounded control queue carries reconciliation commands. The data lane uses yamux and
accepts only server-opened, typed streams for an assigned workload, service, and named
port.

Before a workload reports `Ready`, every declared TCP port on every running replica
must accept a connection. Positive container readiness is cached rather than probed on
every inventory heartbeat. A failed proxy dial invalidates that container's cached
result, causing the next inventory pass to probe it again. This is a bounded startup
and recovery reachability check, not continuous or configurable application health
monitoring.

## Current runtime boundary

Docker execution supports immutable registry digests and worker-local image IDs,
multi-service workloads, stateless Jeopardy replicas, inventory/adoption,
generation replacement, unpublished internal-network ports, cached TCP
forwarding, and a fenced flag-write runtime primitive reserved for future
competitive-mode integration. Linux uses Docker internal networks. Windows uses
a per-workload NAT network whose DNS resolver has no external upstream, plus
HCN endpoint ACLs that allow only the workload subnet and deny every other
outbound destination. The agent applies and reads back the ACL before starting
a container and re-audits adopted endpoints after restart. A workload can still
address its Docker host-side gateway, so the mandatory acknowledgement requires
a dedicated, firewalled host or VM. Windows containers additionally use Hyper-V
isolation. Interactive exec, local image build, and Kubernetes workload
execution are not advertised.
