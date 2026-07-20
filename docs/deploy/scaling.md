# Scale the single binary

rsctf remains one executable and one container image. `RSCTF_ROLE` only selects
which responsibilities a copy starts; it does not create a second application,
protocol, or repository.

Keep `RSCTF_ROLE=all` for the normal one-replica installation. Move to roles
only after the single process is healthy and measurements show that HTTP polling
or round execution needs more capacity.

## Supported roles

| Role | Replicas | Responsibilities |
| --- | ---: | --- |
| `all` | Exactly 1 | API, React client, hubs, maintenance, all A&D/KotH rounds, VPN/BYOC/SSH, packet capture, and migrations |
| `web` | 1 or more | Public API, React client, and hubs; no scheduled/background ownership |
| `control` | Exactly 1 | Stateful BYOC/container-exec API, maintenance, all round processing, VPN/BYOC/SSH, packet capture; no migrations |
| `engine` | 1 or more | Health endpoints, maintenance election, and managed-container A&D/KotH rounds |
| `network` | Exactly 1 | Stateful BYOC/container-exec API, VPN/BYOC/SSH, packet capture, and rounds for games containing BYOC services |
| `migrate` | One-shot | Apply PostgreSQL migrations and exit |

`control` is the simple split companion for `web`. The advanced topology replaces
it with `engine` plus `network`; never run `control` alongside those roles.
PostgreSQL locks, unique constraints, and durable round leases allow engine
workers to be active-active. The network role remains singleton because
WireGuard, firewall rules, SSH listeners, and live yamux tunnels are process-local.

Scheduler ownership is per game, not per service. If any A&D service in a game
is BYOC/self-hosted, the network role owns that whole game's rounds; engine
replicas do not split the managed services from that same game. Put independently
scalable managed and self-hosted services in separate games.

```text
small                         split                       advanced

all x1             web xN ─────────────┐        web xN ─────────────┐
                       control x1 ──────┼─ PostgreSQL     engine xN ─┼─ PostgreSQL
                                      ├─ Redis          network x1 ─┼─ Redis
                                      └─ shared files               └─ shared files
```

## Cluster invariants

Every long-running split role must share:

- one exact, reviewed rsctf image build; never deploy split roles from `latest`;
- one authoritative PostgreSQL database;
- one Redis service for L2 cache, realtime fanout, maintenance election, and
  distributed API rate limits;
- the same JWT secret and public URL;
- one storage namespace and configuration;
- the same container backend and challenge namespace/daemon view.

Treat those resources as one installation boundary. Independent rsctf
installations must not share a Redis logical database or Kubernetes
control/challenge namespace pair: cache keys, channels, and namespace resources
can collide. They may share one Docker daemon only when each installation has a
different `RSCTF_DOCKER_SCOPE`, A&D network name, and non-overlapping A&D CIDRs.
The scope is shared by replicas and prevents orphan cleanup or crash recovery
from adopting or deleting another installation's challenge containers.

Redis Pub/Sub events are best-effort UI notifications. A Redis interruption can
drop a live notification, but it cannot change scores or round correctness;
clients recover through their normal polling reads. A split role reports not
ready while configured Redis is unavailable.

Each binary derives a build/protocol fingerprint from its Rust source,
dependency lockfile, and the runtime-role coordination protocol. Heartbeats only
satisfy another role's readiness when that fingerprint matches. This turns an
accidental mixed-image rollout into an explicit `runtime role build mismatch`
instead of a healthy-looking, partially compatible topology.

Live A&D packet capture is also singleton-owned. Every `all`, `control`, or
`network` process is eligible, but a PostgreSQL session lock selects one owner.
The owner rebuilds its process-local libpcap threads from `AdTeamServices` after
restart. Mutations advance a durable generation, and teardown waits for that
generation to be acknowledged after obsolete threads are joined; this prevents
an old IP/port filter from observing a replacement container. Collection is
Docker-only today and requires `CAP_NET_RAW` plus visibility of the selected
`RSCTF_CAPTURE_DEVICE`; all roles still need the shared files mount to serve the
resulting pcaps.

Local `RSCTF_STORAGE_ROOT` contains more than downloadable blobs: repository
worktrees, checker files, packet captures, and snapshots also use it. Docker
replicas on one host share the same `files_data` named volume because every role
manifest mounts it. Kubernetes or multi-host
deployments need an RWX filesystem. `RSCTF_STORAGE_BACKEND=s3` moves
content-addressed blobs to S3, but does not remove the shared-filesystem
requirement for those working paths.
Split binaries also require `RSCTF_SHARED_STORAGE=true` as an explicit operator
acknowledgement. This is a fail-fast guard against accidentally starting a
healthy-looking replica on node-local disk; it does not replace verifying the
RWX mount itself.

The RWX filesystem must preserve atomic same-directory rename and POSIX
advisory file locks (`flock`) across clients. Checker revisions are published by
rename and held under a shared execution lock while maintenance takes an
exclusive lock for deletion. A volume that only shares bytes, but not those
semantics, is not safe for split roles.

Changing an existing installation from local blobs to S3 does not copy old
objects and there is no dual-read fallback. Copy every hash from the local
sharded store to the matching S3 key prefix before changing the backend, verify
the object count, and keep the local backup through the rollback window. Startup
and `/healthz` verify access to the selected backend using a small
`.rsctf-health` object; that proves credentials and reachability, not that a
manual historical copy was complete.

## Container backend constraint

Roles are a startup boundary, not an internal RPC boundary. Public API handlers
still perform synchronous container operations. Consequently, each API role that
serves those endpoints must be able to address the same backend:

- On one Docker host, replicas may share the same Docker socket.
- A remote `DOCKER_HOST` may be shared if it is secured and all containers are
  created by that one daemon.
- Every replica in one installation uses the same `RSCTF_DOCKER_SCOPE`. Give
  independent installations distinct scopes; the default derives from their
  JWT secrets, while an explicit value survives secret rotation.
- Independent node-local Docker daemons are unsupported because persisted
  container IDs do not currently carry daemon ownership.
- Kubernetes is naturally shared through its API and is the preferred backend
  for managed challenge replicas when application replicas span nodes.
- The in-process BYOC yamux relay is not yet supported by the Kubernetes
  backend.

Persisted challenge build archives are a narrower Docker-only seam: the build
uses a tag as a staging name, then records and provisions only Docker's immutable
image ID. A split role therefore refuses to run an archive build unless the
resolved backend is Docker and
`RSCTF_SHARED_DOCKER_DAEMON=true`. Set that acknowledgement only when every web
replica and every container owner addresses the same daemon; the maintained
single-host Compose Docker override does so. Kubernetes and independent Docker
daemons must use images built and pushed outside rsctf. A mutable registry tag
can be resolved to a repository digest only when the build role can reach Docker;
otherwise configure `registry/name@sha256:...` directly. Runtime Pods and
containers always receive the persisted digest, never the mutable source tag.
The Admin build-registry settings do not currently push inline archive builds,
so they do not relax this guard. Upgrades queue legacy container rows that lack
a digest; rebuild them before provisioning. Definitions already written as a
repository digest are adopted without rewriting that identity. Complete this
upgrade and rebuild pass before official KotH scoring starts, because its hill
image snapshot is intentionally immutable for the event.

## Migration ownership

The `all` role performs the normal startup migration. Split roles never migrate.
The first move from a pre-role rsctf release is a maintenance-window upgrade:
drain and stop the old application, take the database and file backup, run one
`migrate` process from the pinned new image, and require it to succeed before
starting any new long-running role. Do not run these migrations underneath the
old binary. Immutable-image constraints and the exact migration-ledger check do
not promise restart-safe overlap with a pre-role release.

Use the same rule for a later release that changes migrations or the role
protocol unless that release explicitly documents mixed-build compatibility.
The heartbeat fingerprint deliberately keeps incompatible required roles out of
readiness. Once every role is already on one schema, protocol, and pinned build,
ordinary scale up/down of `web` and `engine` replicas remains online.

## Docker Compose: web plus control

The optional `compose.roles.yml` changes the base `rsctf` service into `web` and
adds one `rsctf-control`. It uses Compose's `!reset` tag to remove the base
service's fixed host port, so it requires Docker Compose v2.24 or newer and a
Caddy/other load balancer on the Compose network. Start dependencies, migrate,
and then scale the web service:

```bash
cd deploy
export RSCTF_IMAGE=ghcr.io/dimasma0305/rsctf@sha256:<release-digest>
export COMPOSE_FILE=compose.yml:compose.roles.yml
docker compose up -d db redis
docker compose run --rm --no-deps \
  -e RSCTF_ROLE=migrate -e RSCTF_MIGRATE=1 rsctf
docker compose up -d --scale rsctf=3
```

For a shared Docker challenge backend, include both overrides so web API replicas
and the control owner reach the same daemon:

```dotenv
COMPOSE_FILE=compose.yml:compose.roles.yml:compose.docker.yml:compose.roles.docker.yml
```

For Docker A&D with WireGuard, use:

```dotenv
COMPOSE_FILE=compose.yml:compose.roles.yml:compose.docker.yml:compose.roles.ad-vpn.yml:compose.caddy.yml
```

The role-aware A&D override grants TUN only to control. Control already has the
narrow checker capability set and also uses `NET_ADMIN` for WireGuard plus
`NET_RAW` for the iptables ipset matcher; web replicas receive no Linux
capabilities. The web replicas retain Docker access
because their API routes may create or destroy player containers.

Scale only the public web service:

```bash
docker compose up -d --scale rsctf=5
docker compose up -d --scale rsctf=2
```

Keep `rsctf-control=1`. Compose does not provide a useful multi-host control
plane; use Kubernetes when the replicas need to span machines.

## Route stateful connections to the network owner

Ordinary API, frontend, and hub traffic goes to the web pool. These paths must
go to `control` or `network`, because they consume process-local tunnel state:

```text
/api/stateful/Game/{gameId}/Ad/Byoc/Agent/...  # long-lived agent WebSocket
/api/stateful/Game/{gameId}/Ad/Byoc/Image/...  # image exported by the owner
/hub/containerExec
/hub/containerExec/negotiate          # BYOC/admin terminal hub
```

The control/network HTTP listener serves only these stateful endpoints plus
health probes. Direct requests for account, admin, edit, or ordinary game APIs
are rejected there; they must enter through the web service.

The historical `/api/Game/{gameId}/Ad/Byoc/{Agent,Image}/...` paths remain
available for previously downloaded bundles. The included Caddy configuration
routes both forms. New bundles use the fixed `/api/stateful` namespace so a
portable Kubernetes Prefix rule does not capture unrelated game APIs.

Authorization mutations publish an immediate cross-replica disconnect hint,
but PostgreSQL remains authoritative. Every established BYOC tunnel revalidates
its team, game window, challenge, service, and token at least every 15 seconds;
therefore a lost Redis hint can delay revocation by no more than that lease
interval. Size operational response procedures around this explicit bound.

The included Compose files move the stable `rsctf-network` DNS alias from the
all-in-one process to `rsctf-control` when the role overlay is enabled. Caddy
resolves both that singleton alias and the web service dynamically, so recreated
or scaled containers do not leave stale addresses behind. The shipped Caddyfile
refreshes Docker's embedded DNS every second and bounds a retired-container dial
to 500 milliseconds, leaving enough of the three-second retry budget to select a
surviving replica. For another proxy,
use an equivalent regex/path rule, preserve WebSocket upgrades, refresh service
discovery during scale changes, and give agent connections a long idle timeout.

## Helm: one release per role

The chart's `runtimeRole` selects one role per release. This keeps scaling and
rollbacks independent without building another image. A production split uses
external/shared PostgreSQL and Redis, a pre-created Secret, an externally owned
challenge namespace when using Kubernetes, and one explicitly named existing
RWX claim, even when blobs use S3. The shared Secret must contain the configured
`database-url`, `redis-url`, `jwt-secret`, and `bootstrap-token` keys. The
bootstrap token is consulted only while the shared user table is empty. The
chart rejects bundled PostgreSQL or Redis,
`image.tag=latest`, or generated Secrets for every long-running split role; a
Kubernetes split also rejects a release-owned challenge namespace. The normal `runtimeRole=all` development
defaults remain available.

Use `strategy.type: Recreate` for the singleton `all`, `control`, and `network`
roles. Their network lease is deliberately fail-fast, not a hot standby: during
a one-replica `RollingUpdate`, the replacement cannot become ready while the
old Pod holds the lease, and the old Pod is then never removed. Scalable `web`
and `engine` releases may use `RollingUpdate` when storage supports overlap.

Run the migration hook first:

```bash
export RSCTF_VERSION=1.2.3
kubectl create namespace rsctf-challenges --dry-run=client -o yaml | kubectl apply -f -
helm upgrade --install rsctf-migrate ./charts/rsctf \
  --namespace rsctf-system --create-namespace \
  --set runtimeRole=migrate \
  --set replicaCount=1 \
  --set postgresql.enabled=false \
  --set redis.enabled=false \
  --set existingSecret.name=rsctf-shared \
  --set-string image.tag="$RSCTF_VERSION" \
  --wait
```

Install and wait for the singleton `control` release (or the `network` release
plus at least one `engine`) before exposing web traffic. The challenge namespace
must be pre-created outside every role release and every role must keep
`kubernetes.createChallengeNamespace: false`. This prevents uninstalling or
rolling back one role release from deleting all live challenge workloads.
A web release can then use values such as:

```yaml
runtimeRole: web
replicaCount: 3

image:
  tag: "1.2.3"

existingSecret:
  name: rsctf-shared
postgresql:
  enabled: false
redis:
  enabled: false

config:
  dbMaxConnections: 26

persistence:
  enabled: true
  existingClaim: rsctf-files-rwx
  accessModes: [ReadWriteMany]

ingress:
  enabled: true
  className: nginx
  annotations:
    nginx.ingress.kubernetes.io/proxy-read-timeout: "3600"
  statefulRoutes:
    enabled: true
    # The Service created by the singleton control/network release.
    serviceName: rsctf-control
    servicePort: 8080

containerBackend: kubernetes
trafficCapture:
  enabled: false
kubernetes:
  challengeNamespace: rsctf-challenges
  createChallengeNamespace: false

strategy:
  type: RollingUpdate
```

Install one `control` release for the simple topology, or install `engine` and
`network` releases for the advanced topology. Give every release the same
Secret, PVC, challenge namespace, image tag, and backend configuration. Set
`kubernetes.createChallengeNamespace: false` on every role; no role release owns
the namespace resource. At the default concurrency settings, use
`config.dbMaxConnections: 26` for each web replica and `14` for each engine;
the example value `20` keeps headroom above the control/network minimum of 16
without VPN or 19 with it.

When the deployment uses the A&D VPN, set `vpn.enabled: true` on every role so
web/engine mutations participate in the durable network-policy acknowledgement.
The chart grants `/dev/net/tun`, forwarding sysctls, and the UDP Service only to
`all`, `control`, or `network`; that owner also receives `NET_RAW` for the
iptables ipset matcher. Scalable engine Pods receive `NET_ADMIN` only for
their process-checker firewall; web Pods receive the shared intent configuration
without kernel privileges.

For any Kubernetes backend, set the same real cluster Service CIDR in
`kubernetes.adServiceCidr` on every non-migration release, including `web`,
even when VPN is disabled. Provisioning builds A&D/KotH policy synchronously,
and checker owners use that CIDR as their target allowlist; startup rejects an
empty value instead of waiting for the first custom checker or container.

For Helm deployments using the Docker backend, `trafficCapture.enabled: true`
grants `NET_RAW` only to the singleton `all`/`control`/`network` owner. Set
`trafficCapture.device` to the interface that sees the A&D service network, or
disable capture explicitly when the event does not use it. Kubernetes-backed
live capture remains unsupported.

Capture ownership stays beside the network role because libpcap must observe
the interface that forwards player traffic; scalable engine replicas do not
receive `NET_RAW` or pretend they can capture another Pod's network namespace.
This does not make route safety depend on a healthy capture task. The owner
heartbeats a 12-second PostgreSQL lease and publishes an acknowledgement for
the exact service id, container id, host, port, and owner epoch only after
libpcap startup succeeds. VPN policy admits a capture-required endpoint only
when that exact acknowledgement belongs to the current non-draining lease.

The network owner refreshes a kernel `ipset` every three seconds. The set of
capture-required endpoints persists, while each live acknowledgement has a
15-second kernel timeout. A stopped userspace watchdog therefore cannot keep a
route open: the live member expires in the kernel. Graceful shutdown marks the
owner draining and waits for the resulting network generation before stopping
capture threads or releasing ownership. A replacement starts with a new epoch,
fences stale acknowledgements, starts every desired capture, and only then
reopens the corresponding routes.

If that graceful fence fails, the replica drops readiness and first attempts a
database-independent empty-live-set fence. Capture threads remain running for
the remaining fixed safety window when either fence is uncertain: 16 seconds
when only kernel expiry is needed, or 28 seconds when the old 12-second owner
lease could still refresh a 15-second kernel member. The required worker then
exits and releases its advisory session so process supervision can replace it;
there is no unbounded drain state that can retain capture ownership.
Forced cancellation also aborts the detached heartbeat and signals every pcap
thread. If the surrounding network namespace somehow survives until supervisor
shutdown, the old lease plus final kernel member has a 27-second worst case;
the implementation's 28-second safety window includes one second of margin.

For a complete `all`/`control`/`network` container or Pod crash, the WireGuard
socket and its isolated network namespace disappear with that workload. On a
bare-host service where the kernel namespace survives the process, the
installed rules remain fail-closed after the last live-set member times out (at
most 15 seconds after its final refresh). PostgreSQL expiry plus the
three-second watchdog provides an additional database-policy cutoff while the
network owner is alive. These guarantees cover the managed VPN and internal
A&D service network; directly publishing challenge-container ports outside
that boundary bypasses rsctf policy and is unsupported for mandatory capture.

PostgreSQL generations are the acknowledgement boundary; notifications only
reduce wake-up latency. The owner polls pending generations every five seconds
and also recomputes the complete policy fingerprint every 30 seconds without
advancing a ticket. That safety audit bounds recovery when a process crashes in
the narrow gap after committing policy intent but before creating its ticket.

Expose Ingress only from the web release and name the singleton control/network
Service there:

```yaml
runtimeRole: web
ingress:
  enabled: true
  className: nginx
  annotations:
    nginx.ingress.kubernetes.io/proxy-read-timeout: "3600"
  statefulRoutes:
    enabled: true
    serviceName: rsctf-network
    servicePort: 8080
  hosts:
    - host: ctf.example.org
      paths:
        - path: /
          pathType: Prefix
```

The chart emits portable Prefix routes for `/api/stateful` and
`/hub/containerExec` before the web route. Hot scoreboard, state, targets, and
submit requests remain on the scalable web pool. Previously downloaded bundles
that still use the historical `/api/Game/.../Byoc/...` path must be regenerated,
or your ingress controller must add an exact/regex compatibility route to the
singleton. Inspect the rendered Ingress and test a real agent connection before
the event. The UDP VPN Service must also select the singleton network/control
release.

Scale only roles designed for it:

```bash
helm upgrade rsctf-web ./charts/rsctf --reuse-values --set replicaCount=6
helm upgrade rsctf-engine ./charts/rsctf --reuse-values --set replicaCount=4
```

## Database connection budget

`RSCTF_DB_MAX_CONNECTIONS` is per process. Budget the whole deployment:

```text
total application ceiling = sum(role replicas x role pool limit)
```

For example, four web replicas at 26 connections, two engines at 14, and one
network owner at 20 can open 152 connections. Leave capacity for migration,
administration, PostgreSQL workers, and failure overlap during rolling updates.
Include every temporary `maxSurge` web/engine Pod in that rollout ceiling, not
only the steady-state replica count.
Use PgBouncer or smaller role-specific pools before increasing PostgreSQL's
limit blindly.

Pool validation accounts for connections retained across nested operations. Let
`R=RSCTF_REPO_SCAN_CONCURRENCY` and
`P=RSCTF_PROVISIONING_CONCURRENCY`: use at least `5R+2P+1` for `engine`;
one checker-bearing scan can briefly retain four guards plus its model-write
checkout. Web needs `5R+2P+13`, reserving eight connections for the bounded
roster/account-lifecycle paths and four for runtime transitions. A non-VPN
`control`/`network` process needs `5R+2P+3` because network/BYOC and
traffic-capture ownership each retain one session and another checkout must
remain available for progress; with VPN enabled it needs `5R+2P+6`. The
monolithic `all` role serves both surfaces and therefore needs `5R+2P+15`
without VPN or `5R+2P+18` with it. The one-shot migration role needs two
connections. At the defaults (`R=1`, `P=4`), those floors are 14 for engine, 26
for web, 16/19 for control or network, and 28/31 for `all`; the Compose control
example uses 20 for headroom.

## Graceful scale-down

On `SIGTERM`, a long-running role first makes `/healthz` return `503 shutting
down`, stops claiming new work, and drains HTTP connections. Background workers
receive the shutdown signal. An `all`/`engine` owned round pipeline may finish
for up to 250 seconds; split `control`/`network` drain workers for at most 30
seconds so singleton ownership can move promptly. The listener remains available
for a five-second endpoint-removal window after readiness drops, then long-lived
HTTP/WebSocket connections receive up to 20 seconds to drain. Remaining worker
work is aborted and recovered from its durable PostgreSQL lease by another owner.
`/livez` remains a process-only probe. Readiness responses include `x-rsctf-role` and
`x-rsctf-capabilities` headers for routing diagnostics.

Each long-running split process registers a unique PostgreSQL heartbeat every
five seconds with its exact build/protocol fingerprint. Presence expires after
roughly 15 seconds, and an incompatible build never counts as a peer. `web` requires both
`(control or engine)` and `(control or network)`; a split `engine` additionally
requires `(control or network)` when VPN is enabled. An unexpected required
worker exit drops readiness and terminates that replica instead of leaving a
partially functioning process in service.

Allow five minutes for engine, control, or network roles so the orchestrator
does not cut off that bounded drain; the supplied Compose and Helm defaults do.
WebSockets reconnect to a ready replica. Never scale network below one while
VPN/BYOC is in use, and never scale every engine/control owner to zero during a
live A&D/KotH event.
