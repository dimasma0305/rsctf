# Private Linux workers

RSCTF workers run challenge containers on Docker hosts that cannot accept
inbound Internet connections. The native agent opens long-lived, mutually
authenticated TLS 1.3 connections to the singleton RSCTF network owner. A
Windows PC behind NAT can host a dedicated Linux VM for the worker; neither the
PC nor VM needs a public IP, port forward, VPN, or shared private network with
the server.

Workers are trusted infrastructure, not team-owned BYOC agents. The agent's
Docker socket access (normally through membership in the host's `docker` group)
is host-root-equivalent: compromise of the agent can become compromise of the
entire Docker host despite the systemd sandbox. Every production worker must
therefore be a dedicated event host or VM with no unrelated workloads or
secrets. Never expose the Docker Unix socket or Docker TCP API to RSCTF or the
Internet.

## Supported runtime and topology

The worker agent manages Linux containers on a Linux Docker Engine. Tagged
releases publish static Linux binaries for AMD64 and ARM64; there is no native
Windows artifact or runtime. Docker's Windows network backend cannot enforce
the same internal-network boundary used by the Linux runtime. On a Windows PC,
run the Linux agent in a dedicated Linux VM instead. The RSCTF server itself
may run as a single binary under Docker Compose or Kubernetes, but the worker
agent does **not** schedule Kubernetes Pods and does not use the Kubernetes API.
One agent represents one Docker daemon. If the agent is containerized, it must
still run beside a Docker-capable host with deliberate access to that daemon;
the native binary is the preferred installation.

A Jeopardy worker workload can contain multiple services. Multiple replicas are
allowed only for explicitly stateless Jeopardy services. The protocol already
constrains any future Attack/Defense or King-of-the-Hill integration to exactly
one service and one replica, and it does not version or otherwise change their
scoring. The current server adapter supports Jeopardy workloads only; see the
limitations below.

An all-in-one server can nevertheless run a deliberate hybrid: keep
`RSCTF_CONTAINER_BACKEND=worker` for Jeopardy and set
`RSCTF_WORKER_LOCAL_BACKEND=docker` or `kubernetes` for the existing local
A&D/KotH data plane. `none` is the default and mounts no local runtime. This
does not send A&D/KotH to agents; it only selects where those existing local
containers run. Split worker roles must remain pure remote for now because
web-facing lifecycle requests are not yet delegated to the singleton local
runtime owner.

Remote workloads always use RSCTF's authenticated proxy. Worker addresses and
Docker host ports are never returned to players. A repository digest is
portable between compatible workers; a worker-local image is pinned to one
exact machine.

## Create the private worker PKI

Use a dedicated CA that signs only worker-plane identities. The TLS server
certificate must be signed by this CA and include `RSCTF_WORKER_SERVER_NAME` in
its Subject Alternative Name. The following example creates an ECDSA CA and a
server certificate for `workers.ctf.example.com`; adjust the DNS name before
running it.

```bash
mkdir -p worker-secrets
chmod 700 worker-secrets

openssl genpkey -algorithm EC \
  -pkeyopt ec_paramgen_curve:P-256 \
  -out worker-secrets/worker-ca.key
openssl req -x509 -new -sha256 -days 3650 \
  -key worker-secrets/worker-ca.key \
  -subj '/CN=RSCTF worker CA' \
  -addext 'basicConstraints=critical,CA:TRUE' \
  -addext 'keyUsage=critical,keyCertSign,cRLSign' \
  -out worker-secrets/worker-ca.crt

openssl genpkey -algorithm EC \
  -pkeyopt ec_paramgen_curve:P-256 \
  -out worker-secrets/worker-server.key
openssl req -new -sha256 \
  -key worker-secrets/worker-server.key \
  -subj '/CN=workers.ctf.example.com' \
  -out worker-secrets/worker-server.csr
printf '%s\n' \
  'basicConstraints=critical,CA:FALSE' \
  'keyUsage=critical,digitalSignature,keyEncipherment' \
  'extendedKeyUsage=serverAuth' \
  'subjectAltName=DNS:workers.ctf.example.com' \
  > worker-secrets/worker-server.ext
openssl x509 -req -sha256 -days 397 \
  -in worker-secrets/worker-server.csr \
  -CA worker-secrets/worker-ca.crt \
  -CAkey worker-secrets/worker-ca.key \
  -CAcreateserial \
  -extfile worker-secrets/worker-server.ext \
  -out worker-secrets/worker-server.crt
rm worker-secrets/worker-server.csr worker-secrets/worker-server.ext
chmod 600 worker-secrets/*.key
```

Use `subjectAltName=IP:203.0.113.10` when workers connect by IP and set the
server name to that IP. Keep the CA key offline when the worker plane is not in
use and readable only by the singleton RSCTF network owner while it is active.

## Docker Compose server

The optional overlay mounts all four PKI files read-only, selects the durable
worker backend, defaults to a pure remote mode with control-host packet capture
disabled, and publishes raw TCP port 9443.

Set these values in `deploy/.env` (host paths are relative to `deploy/`):

```dotenv
RSCTF_WORKER_PUBLIC_ENDPOINT=workers.ctf.example.com:9443
RSCTF_WORKER_SERVER_NAME=workers.ctf.example.com
RSCTF_WORKER_BIND_IP=0.0.0.0
RSCTF_WORKER_PORT=9443
RSCTF_WORKER_LOCAL_BACKEND=none
RSCTF_WORKER_DEFAULT_OS=linux
RSCTF_WORKER_DEFAULT_ARCH=amd64
RSCTF_WORKER_CA_CERT_HOST=./worker-secrets/worker-ca.crt
RSCTF_WORKER_CA_KEY_HOST=./worker-secrets/worker-ca.key
RSCTF_WORKER_SERVER_CERT_HOST=./worker-secrets/worker-server.crt
RSCTF_WORKER_SERVER_KEY_HOST=./worker-secrets/worker-server.key
```

For the normal all-in-one deployment:

```dotenv
COMPOSE_FILE=compose.yml:compose.workers.yml
```

For a split web/control deployment, use the role-specific overlay so only the
singleton control process receives the listener and CA key:

```dotenv
COMPOSE_FILE=compose.yml:compose.roles.yml:compose.roles.workers.yml
```

Those two forms are pure remote mode. The split form must stay pure remote;
do not combine `compose.roles.workers.yml` with `compose.roles.docker.yml`,
`compose.roles.ad-vpn.yml`, or another local-runtime overlay. Although the
socket can be isolated from web replicas, local lifecycle routes still execute
on web and are not delegated to control. Giving web replicas the socket is not
an acceptable workaround.

To keep local Docker A&D/KotH while placing Jeopardy on workers in an
all-in-one deployment, set `RSCTF_WORKER_LOCAL_BACKEND=docker` and merge the
Docker socket overlay before the worker overlay. The worker overlay must be
last because it restores `RSCTF_CONTAINER_BACKEND=worker` after the Docker
overlay contributes the root-owned socket:

```dotenv
# all-in-one, local Docker without VPN
COMPOSE_FILE=compose.yml:compose.docker.yml:compose.workers.yml
```

An all-in-one hybrid can also retain local Docker A&D with WireGuard:

```dotenv
COMPOSE_FILE=compose.yml:compose.ad-vpn.yml:compose.workers.yml
```

The VPN companion grants `NET_RAW` to that singleton owner because its
iptables ipset matcher requires a raw socket; it does not enable packet capture.

If that hybrid also requires local packet capture, add the capture overlay
last. It enables capture and grants `NET_RAW` as one operation:

```dotenv
COMPOSE_FILE=compose.yml:compose.docker.yml:compose.workers.yml:compose.capture.yml
```

Do not combine any local-runtime or VPN overlay with the split worker overlay.
In addition to lifecycle delegation, the web tier must participate in durable
VPN policy request/acknowledgement but cannot initialize a worker-only backend
without a local runtime. Disabling VPN policy there would break fail-closed
coordination. Helm rejects split hybrid worker releases; the split Compose
overlay pins both services to `RSCTF_WORKER_LOCAL_BACKEND=none` and capture off.

Do not enable `RSCTF_TRAFFIC_CAPTURE_ENABLED` without the matching capture
overlay. Pure remote and non-capturing, non-VPN hybrid configurations keep both
capture and `NET_RAW` disabled. The Docker overlays explicitly run rsctf as root
because access to the daemon socket is already root-equivalent. Compose does
not provision Kubernetes ServiceAccounts or RBAC; use the all-in-one Helm
configuration below for a Kubernetes local backend.

Non-backend overlays, such as a host-specific live file, may follow only when
they do not change `RSCTF_CONTAINER_BACKEND`. Validate the merged result before
starting:

```bash
cd deploy
docker compose config --quiet
docker compose up -d
```

The overlay supplies the following variables inside the server container:

```dotenv
RSCTF_WORKER_LISTEN=0.0.0.0:9443
RSCTF_WORKER_PUBLIC_ENDPOINT=workers.ctf.example.com:9443
RSCTF_WORKER_SERVER_NAME=workers.ctf.example.com
RSCTF_WORKER_CA_CERT=/run/secrets/rsctf-worker/worker-ca.crt
RSCTF_WORKER_CA_KEY=/run/secrets/rsctf-worker/worker-ca.key
RSCTF_WORKER_SERVER_CERT=/run/secrets/rsctf-worker/worker-server.crt
RSCTF_WORKER_SERVER_KEY=/run/secrets/rsctf-worker/worker-server.key
RSCTF_WORKER_DEFAULT_OS=linux
RSCTF_WORKER_DEFAULT_ARCH=amd64
RSCTF_WORKER_LOCAL_BACKEND=none
```

`PUBLIC_ENDPOINT` is the address written into newly enrolled agents. It can be
different from `LISTEN` when a public TCP load balancer forwards another port.
Use raw TCP or TLS/SNI passthrough; do not terminate worker mTLS at Caddy,
Traefik, an HTTP Ingress, or a CDN.

The architecture setting is the placement default, not an emulation request.
For an ARM64-only fleet, set `RSCTF_WORKER_DEFAULT_ARCH=arm64` in Compose or
`workerBackend.defaultArchitecture=arm64` in Helm. In a mixed AMD64/ARM64 fleet,
set each workload's `workloadSpec.platform.architecture` explicitly so it lands
only on a compatible worker.

## Kubernetes server

Create the PKI Secret in the singleton release namespace:

```bash
kubectl -n rsctf create secret generic rsctf-worker-tls \
  --from-file=worker-ca.crt=worker-secrets/worker-ca.crt \
  --from-file=worker-ca.key=worker-secrets/worker-ca.key \
  --from-file=worker-server.crt=worker-secrets/worker-server.crt \
  --from-file=worker-server.key=worker-secrets/worker-server.key
```

Enable the listener on an `all`, `control`, or `network` release:

```bash
helm upgrade --install rsctf charts/rsctf \
  --set-string secrets.jwtSecret="$RSCTF_JWT_SECRET" \
  --set containerBackend=worker \
  --set workerBackend.localBackend=none \
  --set trafficCapture.enabled=false \
  --set workerBackend.defaultOs=linux \
  --set workerBackend.defaultArchitecture=amd64 \
  --set workerPlane.enabled=true \
  --set workerPlane.existingSecret.name=rsctf-worker-tls \
  --set workerPlane.publicEndpoint=workers.ctf.example.com:9443 \
  --set workerPlane.serverName=workers.ctf.example.com
```

The chart creates a separate `<release>-rsctf-workers` raw TCP Service, a
`LoadBalancer` on port 9443 by default. It never adds this socket to the HTTP
Ingress. With a split `engine`/`network` topology, enable `workerPlane` only on
the one `network` release; other roles can select `containerBackend=worker`
without mounting the CA key. A TCP ingress/controller must use TLS passthrough
to the worker Service.

With `workerBackend.localBackend=none`, Kubernetes hosts the RSCTF control
plane only. Worker workloads still run through Docker on enrolled Linux
machines.

To retain the existing Kubernetes-backed A&D/KotH data plane while Jeopardy
runs on enrolled workers, set:

```yaml
workerBackend:
  localBackend: kubernetes
kubernetes:
  adServiceCidr: 10.96.0.0/12
```

On an all-in-one `runtimeRole=all` release, the chart then mounts the Pod's
ServiceAccount token, creates the same
namespace-scoped Role and RoleBinding used by `containerBackend=kubernetes`,
and emits all `RSCTF_K8S_*` settings. The monolith may let Helm create its
isolated challenge namespace. The chart rejects a non-`none` worker local
backend on every split role until local lifecycle requests can be delegated
from web to a singleton owner. Hybrid VPN is likewise supported only by the
all-in-one `runtimeRole=all` release.

Docker-local hybrid mode is also available in Helm:

```yaml
workerBackend:
  localBackend: docker
docker:
  socket:
    enabled: true
```

That mode runs the rsctf container as root and mounts the host Docker socket.
It permits the existing local packet-capture option. VPN is also supported on
an all-in-one `runtimeRole=all` release; split worker+VPN is rejected as
described above. Treat the socket as root access to the Kubernetes node and use
a dedicated node.

## Prepare the Docker worker

Use a dedicated Linux host or VM; sharing a general-purpose host is unsupported
for production. Docker's `internal` bridge blocks routed
container egress to the LAN and Internet, but a container can still address the
bridge's host-side gateway. The agent therefore refuses to start without
`--accept-host-network-boundary`. That flag is an acknowledgement, not a
firewall: keep secrets and unrelated services off the worker and restrict
host-gateway traffic with the VM/host firewall. The agent never publishes
challenge ports and never modifies the host firewall.

Production startup also requires an enforceable writable-layer quota. Use
`overlay2` with Docker's data root on XFS mounted with project quotas (`pquota`),
or another driver for which Docker supports the per-container `size` option.
Confirm `docker info` reports `Storage Driver: overlay2` and `Backing Filesystem:
xfs`. `--allow-unbounded-storage` is only for trusted disposable development
fixtures; the free-space watchdog cannot make an unbounded hostile workload
safe.

Give Docker an explicit, bounded address pool sized for the event. For example,
`/etc/docker/daemon.json` can reserve 256 isolated `/24` workload networks:

```json
{
  "default-address-pools": [
    { "base": "172.30.0.0/16", "size": 24 }
  ]
}
```

Restart Docker after changing its data root, mount, or address pools. The agent
caps advertised workload-network slots by the detected pool, advertises a
separate per-workload replica limit from the pool's endpoint capacity, and
rejects an operator slot override above the network limit. It also creates the
persistent `rsctf-worker-owner` Docker
volume as an atomic daemon-ownership sentinel; two worker identities cannot
share one daemon. To replace an identity, drain it, remove its managed workloads
and networks, then deliberately remove the sentinel before enrolling the new
identity.

## Install and enroll a worker

Beginning with tagged releases, the
[worker installer](https://github.com/dimasma0305/rsctf/blob/main/scripts/install-worker.sh)
detects Linux AMD64 or ARM64 and downloads the latest tagged archive from
[GitHub Releases](https://github.com/dimasma0305/rsctf/releases), verifies its
SHA-256 checksum and GitHub build attestation, creates the dedicated
`rsctf-worker` account, state directory, and systemd unit, then enables the unit
without starting it. Install a current system-wide GitHub CLI with
`gh attestation verify` support first. Download the installer release asset and
verify its provenance before running it as root. The local attestation bundle
means the worker does not need a GitHub login or token:

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
  sudo bash "$tmp/install-worker.sh" --version "$version"
)
```

On upgrade, an already-active worker is restarted after the verified binary and
unit are replaced. The final install phase is transactional: if the systemd
reload, enable, or restart fails, the installer restores the previous binary,
unit, documentation files, and enabled state, then restarts the restored release
when needed. It exits with an error if that recovery is incomplete. The
idempotent nologin service identity, Docker-group membership, and state directory
are retained for a safe retry; remove them manually only after confirming they
are unused. A fresh installation remains stopped until enrollment succeeds.

To pin a production installation, select the release explicitly, verify its
installer, inspect it, and pass the same tag to the installer:

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
  sudo bash "$tmp/install-worker.sh" --version "$VERSION"
)
```

An untagged checkout does not have corresponding downloadable assets. Releases
contain `install-worker.sh`, both Linux worker archives, the Docker deployment
`install.sh` and `rsctf-deployment-bundle.tar.gz`, their shared `SHA256SUMS`,
and `rsctf-worker-agent-attestation.json`. The exact asset set is checked before
publication. The explicit
`--skip-attestation` installer option falls back to co-hosted HTTPS and checksum
verification; reserve that weaker mode for controlled recovery or development.

On a trusted admin workstation, create a worker through the RSCTF admin API.
The returned enrollment token is nested at `.enrollment.token`, is shown once,
and expires after 15 minutes:

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

Copy only that one-time token to the worker through a secure operator channel;
never copy or configure `RSCTF_ADMIN_TOKEN` on the worker. On the worker, cache
sudo authorization before connecting the secret to the agent's standard input.
The non-interactive `sudo -n` prevents a sudo password prompt from consuming
the enrollment token:

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
sudo systemctl status --no-pager rsctf-worker-agent
```

The agent creates its private key and CSR locally; the key is never uploaded.
Run enrollment as `rsctf-worker`, as above. If provisioning must enroll as
root, also pass `--unix-service-uid "$(id -u rsctf-worker)"` so the state
directory and every identity file are transferred to the service account. Do
not leave a root-owned identity for a service that runs as `rsctf-worker`.
`--token-stdin` keeps the secret out of the local process list. A protected
temporary `--token-file` is also supported; delete it immediately afterward.

The server consumes a token when it signs the certificate. If the response is
lost or local identity-file persistence fails afterward, issue a new token from
the admin API and repeat enrollment; the new certificate supersedes the old
one and disconnects any old session.

For suspected key compromise, rotate without briefly reviving the old
credential: set the worker to `Disabled`, stop its service, issue a replacement
token, and enroll the same worker into a fresh state directory owned by
`rsctf-worker` (or use `--unix-service-uid` during root provisioning). The
replacement certificate is accepted for enrollment while the node remains
disabled, but neither old nor new certificate can open a worker session in that
state. Point the service at the new `worker.json`, explicitly set the worker to
`Enabled`, and then start it. Enrollment intentionally uses create-new files;
do not overwrite the old identity directory in place. Remove it only after the
new session and inventory are healthy.

The enrollment request uses the normal HTTPS API. After enrollment, control
and proxied data use the separate address returned by the server, such as
`workers.ctf.example.com:9443`. Allow outbound TCP from the worker to both
addresses.

`--accept-host-network-boundary` is mandatory for every production run. Keep
the default 512 MiB writable-layer limit and 5 GiB free-space floor unless the
event's capacity model calls for stricter values.

For long-running Linux use, keep the systemd service under its dedicated
account and protect `/var/lib/rsctf-worker`: it contains the worker's mTLS
private key. Follow logs with
`sudo journalctl -u rsctf-worker-agent --follow`.

### Verify or build manually

The installer performs checksum verification automatically. For a manual
download, select the asset matching `uname -m` (`amd64` for `x86_64`, `arm64`
for `aarch64`), download it and `SHA256SUMS` from the same tag, then verify:

```bash
VERSION=vX.Y.Z
ARCH=amd64
ASSET="rsctf-worker-agent-linux-${ARCH}.tar.gz"
BUNDLE=rsctf-worker-agent-attestation.json
BASE="https://github.com/dimasma0305/rsctf/releases/download/${VERSION}"

curl --disable -fLO "${BASE}/${ASSET}"
curl --disable -fLO "${BASE}/SHA256SUMS"
curl --disable -fLO "${BASE}/${BUNDLE}"
awk -v asset="$ASSET" '$2 == asset { print }' SHA256SUMS | sha256sum --check -

# Independent provenance verification with GitHub CLI:
gh attestation verify "$ASSET" \
  --bundle "$BUNDLE" \
  --hostname github.com \
  --repo dimasma0305/rsctf \
  --signer-workflow dimasma0305/rsctf/.github/workflows/worker-agent-release.yml \
  --source-ref "refs/tags/$VERSION" \
  --deny-self-hosted-runners
```

To build instead, install Rust and a musl toolchain, clone the repository, and
choose the target matching the Linux host:

```bash
# AMD64: x86_64-unknown-linux-musl
# ARM64: aarch64-unknown-linux-musl
TARGET=x86_64-unknown-linux-musl
rustup target add "$TARGET"
cargo build --manifest-path agents/worker-agent/Cargo.toml \
  --release --target "$TARGET" --locked
sudo install -m 0755 \
  "agents/worker-agent/target/${TARGET}/release/rsctf-worker-agent" \
  /usr/local/bin/rsctf-worker-agent
```

The source build changes only how the binary is obtained. Use the same
dedicated account, state ownership, enrollment, and systemd configuration as a
release installation.

### Windows PC through a Linux VM

V1 does not execute Windows containers. On a Windows PC, create a dedicated
Linux VM (for example with Hyper-V), install Docker Engine and the Linux worker
binary inside it, and use the Linux enrollment/run commands above. The VM may
use ordinary NAT: it only needs outbound access to the HTTPS enrollment URL and
worker TCP endpoint. Do not forward an inbound port to the VM.

Keep the VM dedicated to challenge workloads. Docker Desktop's hidden utility
VM is not a supported installation target because the agent needs a durable
service identity, state directory, Docker socket, storage quotas, and an
operator-controlled host firewall. Native Windows support requires a separate
HNS/VFP isolation backend and will not be advertised until that boundary has
its own integration and adversarial tests.

## Capacity and placement labels

The agent detects Docker host capacity. Operators can reserve headroom or
define placement constraints with run flags or environment variables:

```bash
sudo -u rsctf-worker /usr/local/bin/rsctf-worker-agent run \
  --config /var/lib/rsctf-worker/worker.json \
  --accept-host-network-boundary \
  --cpu-millis 12000 \
  --memory-bytes 25769803776 \
  --slots 120 \
  --label region=event-a \
  --label trust=dedicated
```

The corresponding variables are `RSCTF_WORKER_CPU_MILLIS`,
`RSCTF_WORKER_MEMORY_BYTES`, `RSCTF_WORKER_SLOTS`, and the comma-separated
`RSCTF_WORKER_LABELS`. `RSCTF_WORKER_DOCKER_ENDPOINT` overrides `local` with a
Unix socket path. Capacity overrides may reserve headroom but cannot exceed the
detected safe CPU, memory, or slot capacity. Do not point the agent at an
unauthenticated TCP Docker API. `--slots` counts isolated workload networks,
not containers or replicas.

## Images

Portable workloads use an immutable repository digest:

```text
registry.example/ctf/service@sha256:…
```

A locally built image can be pinned to one exact worker without a hosted
registry:

```text
worker://018f3c6a-d79b-7cc0-8f68-8fdbad0f57bb/sha256:…
```

The worker-local form cannot fail over to another machine. Use a portable OCI
repository digest when multiple workers must be eligible for a workload.

## Configure a multi-service challenge

The organizer API accepts `workloadSpec` on the existing challenge update
endpoint. The example below runs three stateless application replicas beside
one Redis service; the primary endpoint is the application HTTP port.

```json
{
  "workloadSpec": {
    "gameKind": "jeopardy",
    "platform": {
      "operatingSystem": "linux",
      "architecture": "amd64"
    },
    "services": [
      {
        "name": "app",
        "image": {
          "type": "registryDigest",
          "repository": "registry.example/ctf/app",
          "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        },
        "resources": { "cpuMillis": 500, "memoryBytes": 268435456 },
        "replicas": 3,
        "stateless": true,
        "environment": { "CACHE_HOST": "cache" },
        "ports": [
          { "name": "http", "containerPort": 8080, "protocol": "tcp" }
        ]
      },
      {
        "name": "cache",
        "image": {
          "type": "registryDigest",
          "repository": "registry.example/library/redis",
          "digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        },
        "resources": { "cpuMillis": 250, "memoryBytes": 134217728 },
        "replicas": 1,
        "stateless": false,
        "environment": {},
        "ports": [
          { "name": "redis", "containerPort": 6379, "protocol": "tcp" }
        ]
      }
    ],
    "primaryEndpoint": { "service": "app", "port": "http" },
    "flagTarget": { "service": "app", "path": "/flag" }
  }
}
```

Send that object with `PUT /api/edit/games/{gameId}/challenges/{challengeId}`.
Omit `workloadSpec` to preserve the current setting; send
`{"workloadSpec":null}` to return to the legacy single-container definition.
Only StaticContainer and DynamicContainer challenges accept this field.

Saving changes defines future launches but does not silently replace running
instances. Use **Save and roll out** in the editor, or call
`POST /api/edit/games/{gameId}/challenges/{challengeId}/workload/rollout`, to
apply the saved service/replica shape to active trusted-worker instances. The
response reports matched, updated, stale, incompatible, capacity-rejected, and
failed counts. The editor fences the action to the definition returned by its
save; a concurrent edit returns `409` instead of rolling out another editor's
definition. The endpoint immediately publishes the new generation, workload
identity, and primary port as a fence in container bookkeeping, then waits up
to 90 seconds for that exact generation to report Ready. The proxy keeps the
instance unreachable until that Ready observation; a generation that finishes
after the request timeout becomes routable without leaving stale bookkeeping.
Only a generation observed Ready during the wait is counted as updated.
Each successful workload preserves the per-instance team ID and flag, creates
missing replicas, and removes surplus replicas. This generation is a lifecycle
fence only; it is not A&D/KotH score versioning.

Live rollout replaces the workload generation, so it is accepted only when
every service explicitly sets `stateless: true`. A definition containing a
stateful service, such as the Redis service in the example above, can still be
saved for new launches but must be drained and recreated deliberately; RSCTF
will not silently discard its local state during scale-up or scale-down.

Whenever RSCTF supplies a runtime flag, `flagTarget.service` is mandatory and
must select exactly one service. V1 injects the value as `RSCTF_FLAG` only into
that service. `flagTarget.path` is validated and reserved for future competitive
mode rotation, but the current Jeopardy adapter does not write that file. RSCTF
rejects the launch instead of guessing a service when the target is absent. Do
not place secrets in the base `environment` map because the complete desired
workload is persisted for reconciliation.

## Scaling, maintenance, and failure behavior

RSCTF reserves aggregate CPU and memory for every replica plus one isolated
network slot per workload. Placement also requires the worker's advertised
per-network replica limit to cover the complete workload. All services and
replicas belonging to one workload stay on the selected Docker worker. Replicas
improve per-instance throughput but do not provide worker-level high
availability. Add or remove worker hosts to change placement capacity; use
portable repository digests so new workloads can select any compatible worker,
and use workload replicas only for stateless Jeopardy services.

Set a worker to `Draining` before planned shutdown. Draining prevents new
placements but preserves current routes. An offline worker keeps its durable
assignments; RSCTF does not silently move stateful A&D or KotH workloads.

On reconnect, the agent inventories labelled Docker objects and adopts only
the matching worker, assignment, generation, and specification hash. Old
sessions and delayed commands cannot publish a route or delete a replacement.

- A missed worker lease removes routes; it does not score a service as down.
- Workloads continue running during a temporary control-plane disconnect.
- Reconnect adopts matching labelled resources instead of creating duplicates.
- Destroy releases capacity only after the current assignment reports `Absent`.
- Disabling a worker fences its current certificate session immediately.

## Current limitations

- The agent runtime is Docker only. It does not manage Kubernetes Pods.
- Native Windows-container execution is disabled and no Windows worker artifact
  is released. A Windows PC can host the supported Linux agent in a dedicated
  Linux VM with outbound-only NAT.
- The existing one-container compatibility path launches Jeopardy workloads.
  Remote-worker A&D/KotH networking is not enabled; configure
  `workerBackend.localBackend`/`RSCTF_WORKER_LOCAL_BACKEND` as Docker or
  Kubernetes on an all-in-one server to keep those modes local. Split hybrid
  mode is rejected until lifecycle requests can be delegated safely. With
  `none`, A&D/KotH containers are unavailable. The shared protocol already
  rejects replicas or multiple services for those game modes, so this does not
  alter constant scoring.
- Registry authentication and image builds are not implemented in the agent.
  Use a registry it can pull without credentials or preload an immutable image
  and use a worker-local reference. Server-side archive builds remain usable
  for local Docker A&D/KotH in all-in-one hybrid mode, but a Jeopardy challenge
  routed to workers must use one of those portable or explicitly worker-scoped
  image forms.
- A workload reports `Ready` only after every declared TCP port on every running
  replica accepts a connection. Successful container probes are cached to keep
  reconciliation cheap; a failed proxy dial invalidates that container's cached
  result so the next inventory pass probes it again. This is bounded TCP
  reachability for startup and recovery, not continuous or configurable
  application-level health monitoring.
- Interactive container exec and automatic worker-certificate renewal are not
  implemented. Re-enroll deliberately during a maintenance window when an
  identity must be replaced.
- Every workload receives a dedicated Docker `internal` network. The agent
  reaches named service ports directly on that network; no host port is
  published, and routed egress to the LAN or Internet is blocked. On Linux,
  Docker still makes the bridge's host-side gateway addressable from a
  container. The mandatory acknowledgement and dedicated firewalled host/VM are
  therefore part of the security boundary. Registry images are pulled through
  the host Docker daemon before containers start.
