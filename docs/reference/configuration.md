# Configuration reference

rsctf reads startup configuration from environment variables. Docker stores them in `deploy/.env`; Helm maps chart values and Secrets into the Pod environment.

Restart rsctf after changing a startup value. Settings changed in **Admin → Settings** are stored in PostgreSQL and usually take effect through the application UI/runtime instead.

## Core service

| Variable | Default | Purpose |
| --- | --- | --- |
| `RSCTF_ROLE` | `all` | `all`, `web`, `control`, `engine`, `network`, one-shot `migrate`, or loopback-only `development`; see the [scaling guide](../deploy/scaling) and [source-development guide](./source-development) |
| `RSCTF_BIND` | `0.0.0.0:8080` | HTTP listen address inside the process/container |
| `RSCTF_DATABASE_URL` | Local development URL | PostgreSQL connection URL; required in deployment |
| `RSCTF_DB_MAX_CONNECTIONS` | `34` | Per-process database connection cap; computed minimum described below |
| `RSCTF_REDIS_URL` | Unset | Redis cache URL; when configured, Redis is required for readiness and reconnects after an outage |
| `RSCTF_DISTRIBUTED_RATELIMIT` | `false` | Share rate limits through Redis for multiple replicas |
| `RSCTF_AD_SUBMIT_BURST_FLAGS` | `400` | Immediate per-participation A&D flag-work budget before the fixed 10 flags/s refill (`100..3200`) |
| `RSCTF_SUSPICION_RECONCILE_SECONDS` | `30` | All/control/engine full-history anti-cheat sweep interval (`1..3600` seconds); durable outbox jobs still poll once per second |
| `RSCTF_SUSPICION_FINALIZE_GRACE_SECONDS` | `360` | Pause after configured game end before the barrier-backed final anti-cheat pass (`1..3600` seconds) |
| `RSCTF_AUTH_IP_BACKSTOP_PER_MINUTE` | `120000` | High shared-source ceiling after credential validation (`12000..1000000`) |
| `RSCTF_CREDENTIAL_IP_ADMISSION_PER_MINUTE` | `30000` | Cheap shared-source ceiling before bearer verification/token lookup (`3000..1000000`) |
| `RSCTF_KOTH_CAPABILITY_IP_ADMISSION_PER_MINUTE` | `6000` | Dedicated shared-arena ceiling before managed Leaderboard KotH capability lookup (`3000..1000000`) |
| `RSCTF_JWT_SECRET` | Insecure development placeholder | Session signing secret; deployment validation requires at least 32 bytes and rejects known defaults |
| `RSCTF_IDENTITY_HASH_KEY` | Required | Dedicated 32+ byte HMAC key for pseudonymous identity evidence; keep stable across replicas, restarts, and JWT rotations |
| `RSCTF_JWT_TTL_SECS` | `604800` | Session lifetime in seconds; must be positive |
| `RSCTF_PUBLIC_URL` | Derived from request | Canonical browser-facing `http://` or `https://` origin |
| `RSCTF_COOKIE_SECURE` | `true` | Send session cookies only over HTTPS; set `false` only for local HTTP |
| `RSCTF_TRUSTED_PROXY_CIDRS` | Empty | Immediate proxies allowed to set forwarded client addresses |
| `RSCTF_STORAGE_ROOT` | `./files` | Persistent local blob directory |
| `RSCTF_STORAGE_BACKEND` | `auto` | `auto`, `local`, or `s3`; `auto` selects S3 when any S3 setting is present |
| `RSCTF_SHARED_STORAGE` | `false` | Required explicit acknowledgement for split roles that every replica mounts the same `RSCTF_STORAGE_ROOT` |
| `RSCTF_SHARED_DOCKER_DAEMON` | `false` | Allow daemon-local immutable image IDs in a split Docker role only after verifying that every builder and container owner addresses one shared Docker daemon |
| `RSCTF_STATIC_DIR` | `web/build` | Built frontend directory; the official image sets this internally |
| `RSCTF_MIGRATE` | `1` | Set `0` to skip automatic startup migrations |
| `RUST_LOG` | `info` | Rust tracing filter, such as `info,rsctf=debug` |

Split roles require Redis and `RSCTF_SHARED_STORAGE=true`; the event bus automatically uses Redis Pub/Sub to
fan best-effort hub notifications between processes. Enable distributed rate
limiting for every API-serving split role. The maintained Compose and Helm role
profiles do this automatically.

`RSCTF_SHARED_DOCKER_DAEMON` is a safety acknowledgement, not discovery. Leave
it disabled for Kubernetes and for independent node-local Docker daemons. Those
topologies must prebuild and push challenge images, then configure a concrete
`registry/name@sha256:...` reference. rsctf does not currently push archive
builds to a registry. A successful build/pull stores its immutable runtime
reference in PostgreSQL; changing `containerImage` clears that pin and queues a
new resolution.

## S3 blob storage

| Variable | Default | Purpose |
| --- | --- | --- |
| `RSCTF_S3_BUCKET` | Unset | Required bucket when S3 is selected |
| `RSCTF_S3_ACCESS_KEY` | Unset | Required access-key ID; provide through a Secret |
| `RSCTF_S3_SECRET_KEY` | Unset | Required secret key; provide through a Secret |
| `RSCTF_S3_REGION` | Provider default | Optional region |
| `RSCTF_S3_ENDPOINT` | AWS default | Optional S3-compatible endpoint, including MinIO |
| `RSCTF_S3_PREFIX` | `assets` | Object-key prefix for content-addressed blobs |
| `RSCTF_ASSET_SIGNED_URL_TTL_SECS` | `0` (disabled) | `30..3600` enables short-lived direct S3 URLs for authorized, static challenge attachments |

Once any S3 setting is present, incomplete configuration fails startup instead
of falling back to local disk. S3 stores blob assets only. Persist and share
`RSCTF_STORAGE_ROOT` as well when replicas use repository worktrees, checker
files, packet captures, or snapshots.
Startup and readiness probe a small `.rsctf-health` object. Switching an
existing local installation to S3 requires copying its existing content hashes
first; rsctf does not silently dual-read or migrate historical objects.

Direct delivery is deliberately opt-in. RSCTF still performs the live user and
participation check, records the download event, and then returns a temporary
credential that authorizes only that immutable object for the configured TTL.
The redirect itself is `private, no-store`. Dynamic per-team attachments,
writeups, branding, range requests carrying `If-Range`, and all local-storage
downloads stay on the normal RSCTF stream. Enabling the option with a non-S3
backend fails startup instead of silently changing behavior. A generated URL
must be absolute HTTPS; an HTTP/custom-endpoint URL is rejected and the request
falls back to the authenticated RSCTF stream.

Keep the bucket private and the TTL short. If a CDN fronts S3, it must validate
the S3 signature before serving a hit. Do not configure a CDN rule that ignores
the signature query string before authorization: that would turn a protected
attachment into a public cache object. The safe default (`0`) preserves the
existing proxy boundary. Uploaded objects carry `private, no-store` metadata,
so direct S3 delivery offloads bandwidth but does not silently create a shared
cache. A provider-specific cache override is safe only behind signature
validation and must retain the same short authorization window.

## Registration

| Variable | Default | Purpose |
| --- | --- | --- |
| `RSCTF_ALLOW_REGISTER` | `true` | Allow public account registration through any method; the empty-database admin bootstrap remains possible |
| `RSCTF_ALLOW_PASSWORD_REGISTRATION` | `true` | Allow public username/password account creation. Disable after configuring Google or Discord to require OAuth for new accounts; existing password login and first-admin bootstrap remain available |
| `RSCTF_BOOTSTRAP_TOKEN` | Unset | 32+ character secret required for the first administrator while the user table is empty; ignored for later registrations |
| `RSCTF_EMAIL_CONFIRM` | `false` | Require email-confirmation behavior for later accounts |
| `RSCTF_ACTIVE_ON_REGISTER` | `true` | Make later registered users active immediately |
| `RSCTF_USE_CAPTCHA` | `false` | Startup fallback for CAPTCHA enforcement. The installer writes the disabled default explicitly; a policy saved in the Admin UI takes precedence |

`RSCTF_ADMIN_CONFIRM` is loaded into the startup configuration, but the current
live registration path does not consume it. Configure account approval with the
active account policy in the Admin UI and test it with a normal account.

## Dynamic containers

| Variable | Default | Purpose |
| --- | --- | --- |
| `RSCTF_CONTAINER_BACKEND` | `auto` | `none`, `auto`, `docker`, or `kubernetes`; deployments should select explicitly |
| `RSCTF_CONTAINER_MAX_MEMORY_MB` | `4096` | Global upper bound for one challenge container |
| `RSCTF_CONTAINER_MAX_CPU_COUNT` | `8` | Global CPU-count upper bound for one challenge container |
| `RSCTF_DOCKER_PUBLIC_ENTRY` | Unset | Hostname/IP advertised for Docker-published challenge ports |
| `RSCTF_DOCKER_PROXY_BIND` | Unset outside Compose | Private IPv4 host interface used for PlatformProxy-only Docker ports; required when that mode launches a local Docker challenge |
| `RSCTF_CHALLENGE_PROXY_SUBNET` | Compose-managed private `/24` | Dedicated bridge CIDR admitted by the PlatformProxy host-firewall guard; must contain the proxy bind |
| `RSCTF_CHALLENGE_PROXY_BRIDGE` | `rsctf-proxy0` in deployment Compose | Linux bridge interface admitted by the PlatformProxy host-firewall guard; letters/digits/dot/underscore/dash, at most 15 characters |
| `RSCTF_PROXY_FIREWALL_RECONCILE_SECONDS` | `2` | Interval at which the Docker firewall sidecar validates and restores its bind-scoped `INPUT` and `DOCKER-USER` rules |
| `RSCTF_DOCKER_SCOPE` | Hash of `RSCTF_JWT_SECRET` | Stable installation identity for Docker workload labels and recovery names; use one value across replicas and a different value for every installation sharing a daemon |
| `RSCTF_K8S_NETWORK_POLICY_ENFORCED` | Required `true` for Kubernetes backend | Operator acknowledgement that a cross-Pod probe proved the cluster CNI enforces `networking.k8s.io/v1` NetworkPolicy; startup fails without it |
| `RSCTF_PROVISIONING_CONCURRENCY` | `4` | Concurrent provisioning operations |
| `RSCTF_REPO_SCAN_CONCURRENCY` | `1` | Concurrent long-lived shared checkout scans per process (`1..4`) |
| `RSCTF_TRAFFIC_CAPTURE_ENABLED` | `false` | Allow the singleton `all`/`control`/`network` worker to collect packet captures for challenges that enable it; Compose deployments must also select the matching capture overlay that grants `NET_RAW` |
| `RSCTF_CAPTURE_DEVICE` | `any` | libpcap device used by the singleton capture owner |
| `RSCTF_CAPTURE_RECONCILE_SECONDS` | `2` | Durable capture desired-state recovery interval (`1..60` seconds) |
| `DOCKER_HOST` | Local socket | Docker daemon endpoint used by the Docker backend |

### On-demand image builds and bounded cleanup

Docker installations can enable **Admin → Settings → Container policy → Build
images on first start**. This defers only rsctf-managed, archive-backed Jeopardy
`StaticContainer` and `DynamicContainer` images. A&D services, KotH hills,
checker images, external registry images, workload specifications, and any image
without a complete immutable source archive remain eager or operator-managed.
The first eligible player start performs the build; an in-process single-flight
plus a PostgreSQL image lock collapses concurrent starts across replicas into
one build. Later starts use the published immutable image ID.

Enable **Clean idle images and build cache** to run a sweep every 15 minutes.
The default image lease is 24 hours and is renewed by a successful build or an
authorized start attempt. Cleanup refuses every image used by a running or
stopped container and every challenge image whose exact archive, context,
managed tag, and published digest cannot prove that it can be rebuilt. It also
never treats A&D/checker images as disposable. Outside storage pressure, only
unused build cache older than its configured retention is pruned. Below the
configured free-space floor, rsctf first prunes all unused build cache and
dangling images, then applies the same expired-and-rebuildable image rule; it
does not force-delete recent or active images merely to satisfy the floor.

The Builds page shows free storage, private/reclaimable Docker build-cache
bytes, image leases, and container use. **Clean storage now** runs the same
bounded sweep and returns the actual reclaimed-byte report. These controls
require the Docker backend and a local Unix Docker socket; Kubernetes and
remote-daemon installations must use their registry/runtime retention policy.

If the selected explicit backend is unavailable, startup fails. `auto` can fall back to no container manager and is prohibited when the integrated VPN is enabled. rsctf hashes the Docker scope before writing it to labels. Set an explicit scope before rotating the JWT secret so already-running workloads remain discoverable; all replicas and the control owner must use the same scope.

Live packet collection currently requires the Docker backend, visibility of the
A&D service traffic on `RSCTF_CAPTURE_DEVICE`, and `CAP_NET_RAW`. PostgreSQL
generations coordinate the singleton owner across replicas; API teardown waits
for the owner to join an obsolete capture thread before destroying its container.
The owner uses a fixed 12-second database lease (refreshed every three seconds),
and the VPN's exact live-endpoint ipset uses a fixed 15-second kernel timeout.
Those safety bounds are deliberately not operator-tunable. An endpoint whose
challenge requires capture is routed only after libpcap startup has published
the exact service/container/host/port acknowledgement for the current epoch.
Failed owner cleanup keeps capture alive for the corresponding fixed expiry
window (16 seconds for kernel-only uncertainty, 28 seconds when lease expiry is
also required), then releases ownership and terminates the unhealthy replica.
Capture files remain under the shared `RSCTF_STORAGE_ROOT` even when blob storage
uses S3.

## A&D engine and VPN

| Variable | Default | Purpose |
| --- | --- | --- |
| `RSCTF_AD_VPN_ENABLED` | `false` | Enable integrated VPN policy coordination; an `all`/`control`/`network` role owns the WireGuard hub |
| `RSCTF_AD_VPN_REQUIRED` | `false` | Fail startup if VPN initialization fails; requires VPN enabled |
| `RSCTF_AD_VPN_CLIENT_CIDR` | `10.13.37.0/24` in code | Address pool for team peers; deployment templates may choose a larger non-overlapping range |
| `RSCTF_AD_VPN_SERVICES_CIDR` | `10.13.40.0/24` | Docker A&D service network |
| `RSCTF_AD_VPN_SERVICES_NETWORK` | `<Compose project>-ad` (`rsctf-ad` outside Compose) | Docker A&D service network name; keep it unique per installation sharing a daemon |
| `RSCTF_AD_VPN_EGRESS_NETWORK` | `rsctf-ad-egress` | Legacy Docker bridge name; competitive Docker egress now fails closed and never joins this shared bridge |
| `RSCTF_AD_VPN_LISTEN_PORT` | `51820` | WireGuard UDP listen port |
| `RSCTF_AD_VPN_SERVER_ENDPOINT` | Derived | Public `host:port` placed in player configurations |
| `RSCTF_AD_VPN_DNS` | `1.1.1.1` | DNS server placed in generated WireGuard profiles |
| `RSCTF_AD_VPN_ALLOWED_IPS` | Derived routes | Optional explicit routes in player profiles |
| `RSCTF_KOTH_REPORTER_BASE_URL` | Unset | Private absolute HTTP(S) origin, without a path/query/credentials, that managed Leaderboard targets use for capability exchange, context reads, and evidence submission. Configure the same value on the lifecycle-owning role and web roles that serve organizer status; web roles treat it only as a capability flag. Kubernetes requires a cross-namespace Service origin such as `http://rsctf-network.rsctf-system.svc:8080`; callback policy allows that Service port and rsctf's configured bind/target port to cover Service translation. Leaving it unset keeps legacy external reporting only. |
| `RSCTF_EVENT_VPN_CREDENTIAL_KEY` | Unset | Independent 32+ character key for event peer private-key encryption and short-lived proof signing |
| `RSCTF_EVENT_VPN_PROOF_URL` | Unset | HTTPS rsctf origin reachable only over an event split route; required before an event can enable its VPN gate |
| `RSCTF_EVENT_VPN_ALLOWED_IPS` | VPN client CIDR plus service routes | Additional narrow split-tunnel routes in event profiles; default routes are rejected |
| `RSCTF_EVENT_SENSOR_TOKEN` | Unset | Independent 32+ character bearer credential shared only by the network owner and optional sensor sidecar |
| `RSCTF_EVENT_SENSOR_API_URL` | `http://127.0.0.1:8080` | Loopback HTTP or HTTPS machine API used by the sidecar |
| `RSCTF_EVENT_SENSOR_INTERFACE` | `wg0` | Capture interface for aggregate event telemetry |
| `RSCTF_EVENT_SENSOR_ASN_FILE` | Unset | Optional local `CIDR,ASN,CLASS` prefix file; rsctf performs no remote lookup |
| `RSCTF_SOLVE_RECEIPT_ISSUER_TOKEN` | Unset | Independent 32+ character machine credential for trusted challenge verifiers |
| `RSCTF_AD_SSH_PORT` | `2222` | A&D SSH bastion listen port |
| `RSCTF_AD_SSH_PUBLIC_HOST` | Docker public entry | Host advertised for the SSH bastion |
| `RSCTF_AD_BYOC_AGENT_IMAGE` | Same-release GHCR digest in official images; none in direct source/local builds | Immutable `repository@sha256:...` relay-agent override. Tagged official server images embed the exact amd64/arm64 agent index produced by their workflow. Direct builds fail BYOC bundle generation until this points to an agent built from the same ACK-capable release. |
| `RSCTF_AD_TICK_SECONDS` | Engine/game setting | Default A&D tick timing override (`30..600` seconds); persisted round boundaries are anchored to this cadence |
| `RSCTF_AD_CHECKER_TIMEOUT_SECONDS` | `30` | Per-check timeout; set deliberately below the event tick only after checker validation |
| `RSCTF_AD_CHECKER_CONCURRENCY` | CPU-scaled, `32..128` | Maximum concurrent A&D/KotH probes (`1..256`) |
| `RSCTF_CHECKER_UID_BASE` | `60000` | First otherwise-unused numeric UID reserved for isolated checker processes; changing it requires restart |
| `RSCTF_CHECKER_PROCESS_BUDGET` | `32` | Reserved UID count and process-wide custom-checker concurrency bound (`1..256`); pool wait counts against the checker timeout |
| `RSCTF_AD_GAME_CONCURRENCY` | `4` | Maximum games whose round pipelines run concurrently (`1..16`) |
| `RSCTF_AD_FLAG_PUSH_CONCURRENCY` | `64` | Maximum concurrent managed/BYOC flag publications (`1..256`) |
| `RSCTF_AD_FLAG_PUSH_ATTEMPTS` | `3` | Bounded flag-publication attempts per service (`1..5`); attempts × timeout plus retry backoff must fit 6.5 seconds, preserving receipt-persistence time inside the seven-second phase |
| `RSCTF_AD_FLAG_PUSH_TIMEOUT_SECONDS` | `2` | Timeout for one publication attempt (`1..10`); startup rejects combinations that cannot fit the minimum 30-second tick |
| `RSCTF_AD_CHECKER_MEM_MB` | Internal default | Checker sandbox memory cap |

Flag-publication concurrency is a resource ceiling, not a guarantee that every
unresponsive service can consume a full timeout. At the defaults, the 6.15-second
work window can admit at most about 192 first attempts across three two-second admission waves if
none returns early. Services that never reach admission are recorded as
platform-attributed voids, while a started failed attempt is participant-attributed
Offline evidence. Each publication randomizes target order so database/service ID
order cannot repeatedly decide who reaches admission. Size concurrency only after
load-testing the container runtime; increasing it blindly can overload Docker.

`allowEgress: true` is supported only by the Kubernetes container backend,
which creates a per-workload NetworkPolicy. The Docker backend rejects it for
A&D and KotH because its shared bridge cannot safely exclude peer workloads,
private networks, and metadata endpoints. Keep the legacy egress-network
variable only for configuration compatibility; it does not re-enable Docker
egress.

### Checker dependency preparation

An A&D or KotH checker may place `requirements.txt` beside its `run.py`. Each
entry must be a simple, exact PyPI pin such as `httpx==0.28.1` or
`pwntools==4.15.0`. Repository Bindings and the admin approval path reject URLs,
local paths, editable installs, pip options, and unpinned or ranged versions.
Accepted packages and their dependencies are installed with pip's wheel-only
mode into the immutable checker virtual environment under
`RSCTF_STORAGE_ROOT`; a missing compatible wheel fails preparation instead of
starting a source build. Blank lines and comments are allowed; the file is
limited to 16 KiB and 32 unique package names.

The rsctf process performing the repository scan or checker approval therefore
needs outbound HTTPS access to PyPI and its package file hosts. Package
installation occurs at this trusted administration boundary, before the
checker revision is published. Review the exact repository commit and all
dependency pins before starting it; direct pins constrain top-level drift but
do not establish package trust. Checker execution never runs pip, and its
existing runtime firewall remains limited to the one resolved challenge target
and TCP port. The generated HTTP A&D starter ZIP follows this contract and
includes an exact `httpx==0.28.1` checker requirement. Its `run.py` registers
focused health and current-flag functions with `@checker`, then calls
`run_ad_checker()` to attempt the whole suite in cryptographically shuffled
order. Registered order is not execution order; every function is attempted
once even when another reports a failure. The final priority is InternalError,
Offline, Mumble, then OK. The legacy `@ad_checker` single-function form remains
supported. The platform's outer hard timeout can still terminate an overlong
checker before its suite finishes.

The official rsctf image includes the Python venv and pip support used during
preparation. A custom rsctf runtime image must provide both as well.

Database work that crosses an external Git/container operation retains advisory
lock connections while it issues nested queries. A checker-bearing repository
scan can briefly retain checkout, game-control, checker-publication, and
challenge-definition guards while its model write needs a fifth connection. Let `R` be
`RSCTF_REPO_SCAN_CONCURRENCY` and `P` be
`RSCTF_PROVISIONING_CONCURRENCY`. The per-process pool floor is:

| Process mode | Minimum `RSCTF_DB_MAX_CONNECTIONS` |
| --- | ---: |
| One-shot `migrate` | `2` |
| `engine` | `5R + 2P + 3` |
| `web` | `5R + 2P + 13` |
| Non-VPN `control` | `5R + 2P + 6` |
| Active VPN-owning `control` | `5R + 2P + 9` |
| Non-VPN `network` | `5R + 2P + 4` |
| Active VPN-owning `network` | `5R + 2P + 7` |
| Non-VPN `all` | `5R + 2P + 18` |
| Active VPN-owning `all` | `5R + 2P + 21` |

The migration role uses only the pool's two baseline connections. A network
owner retains the network/BYOC lease, the traffic-capture lease, and an isolated
capture-heartbeat connection even without VPN, plus one progress connection.
The VPN allowance additionally covers its `LISTEN` connection and nested
kernel/allocation reconciliation.
Monolithic and web roles reserve eight connections for bounded roster and
account lifecycle operations, plus four for the independently bounded runtime
transition path; each can retain a lock while issuing nested work. The
all/development/control/engine suspicion reconciler reserves one fence plus one nested
checkout. At the defaults (`R=1`, `P=4`), engine needs 16 connections, web
needs 26, control needs 19 without VPN or 22 with it, network needs 17 or 20,
`development` needs 28, and `all` needs 31 or 34. Keep additional headroom for
ordinary request bursts where practical.

Checker and flag work is bounded by the persisted round deadline. Evidence that
finishes at or after that deadline is excluded, and unresolved samples become
platform-attributed voids. Managed readiness runs before a due round is stored;
if scheduler or readiness work is late, the next round is re-anchored at its
durable preparation time. Elapsed flag windows are never replayed.

`GET /livez` checks process responsiveness without external I/O. `GET /healthz`
checks PostgreSQL, blob storage, and the selected cache backend. With no `RSCTF_REDIS_URL`, the
bounded local cache is an explicit single-process mode and remains ready. Once a
Redis URL is configured, rsctf never silently changes modes: `/healthz` returns
`503` during a Redis outage while the cache reconnects in the background. A probe
timeout caused by a short connection-pool queue may reuse a confirmed healthy
result for at most 15 seconds; explicit dependency errors fail immediately, and
timeouts cannot extend that grace window.

Split roles also publish a PostgreSQL presence heartbeat every five seconds;
peers older than roughly 15 seconds do not satisfy readiness. A `web` replica
requires both `(control or engine)` and `(control or network)`. An `engine`
requires `(control or network)` only when the integrated VPN is enabled.
`control` and `network` are self-contained gates. `all` and the one-shot
`migrate` role do not use topology heartbeats.

Authenticated API traffic is primarily limited per account. The additional
shared-source backstop is configured with
`RSCTF_AUTH_IP_BACKSTOP_PER_MINUTE` (default `120000`, valid
`12000..1000000`). `RSCTF_CREDENTIAL_IP_ADMISSION_PER_MINUTE` (default
`30000`, valid `3000..1000000`) bounds work from rotating invalid credentials
before signature or database verification. Login, recovery, registration, mail,
and OAuth-start limits remain strictly IP-scoped.

Managed Leaderboard KotH capability exchange has a separate source bucket,
`RSCTF_KOTH_CAPABILITY_IP_ADMISSION_PER_MINUTE` (default `6000`, valid
`3000..1000000`). The default bucket refills at the maintained 2,000-team
fixed-rate profile of 100 authentications/second and holds three complete waves.
At most eight capability lookups per web process may occupy PostgreSQL at once;
excess work receives `429` with `Retry-After`. After a capability is verified,
the ordinary 150 requests/minute allowance is applied to its canonical game,
challenge, and participation. Reporter context and observation traffic therefore
keeps a separate rate-limit budget during a shared-source login wave.
The Helm equivalent is `config.kothCapabilityIpAdmissionPerMinute`.

A&D submission is charged by distinct plausible flags, not HTTP requests. The
default permits four immediate maximum-size batches for one participation, then
refills at 10 flags/second. Repeating one flag in a batch costs one token. Keep
`RSCTF_AD_SUBMIT_BURST_FLAGS=400` in production; the upper bound exists for an
explicit isolated load campaign, not as a scoring or event-size setting.

## Kubernetes backend

| Variable | Default | Purpose |
| --- | --- | --- |
| `RSCTF_K8S_NAMESPACE` | `rsctf-challenges` in code | Namespace for generated challenge resources; the Helm chart overrides this per release |
| `RSCTF_K8S_PUBLIC_ENTRY` | Pod node IP fallback | Address advertised for normal challenge NodePorts |
| `RSCTF_K8S_CHALLENGE_UID` | `10000` | Non-root UID/GID used in generated challenge Pods |
| `RSCTF_K8S_AD_SERVICE_CIDR` | Unset | Authoritative cluster Service CIDR; required on every non-migration Kubernetes-backend role for A&D/KotH provisioning and checker target isolation, even without VPN |
| `RSCTF_K8S_ISOLATED_POD_NETNS` | `false` | Explicit confirmation of an ordinary isolated Pod network namespace |
| `RSCTF_K8S_CONTROL_NAMESPACE` | Service-account namespace fallback | Namespace containing the rsctf control Pod |
| `RSCTF_K8S_CONTROL_POD_LABEL` | `app.kubernetes.io/name=rsctf` | `key=value` selector allowed to reach A&D services |
| `RSCTF_K8S_KOTH_REPORTER_POD_SELECTOR` | Unset | Comma-separated exact callback Service pod selector for managed KotH egress. It must include `app.kubernetes.io/name`, `app.kubernetes.io/instance`, and `app.kubernetes.io/component`; copying the Service's complete `.spec.selector` prevents a challenge from reaching unrelated rsctf roles. The control namespace, canonical selector, and resolver peers are part of the lifecycle routing revision, so changing any of them rotates the target credential and prevents crash-orphan adoption. The Helm chart derives this for `all`, `control`, and `network`; a split `engine` must set `kubernetes.kothReporterPodSelector` to the `network` Service selector. |
| `RSCTF_K8S_DNS_CIDRS` | Nameservers in `/etc/resolv.conf` | Comma-separated exact resolver IPs or host-prefix CIDRs admitted on TCP/UDP 53. This supports ordinary CoreDNS Service routing and NodeLocal DNSCache without assuming a single Pod label. Set `kubernetes.dnsCidrs` when rsctf runs outside the challenge cluster or uses a different resolver path. Broad resolver subnets and loopback addresses are rejected. |
| `RSCTF_K8S_AD_INGRESS_CIDRS` | Empty | Extra exact CIDRs allowed into A&D service policies |
| `RSCTF_K8S_ISOLATED_INGRESS_CIDRS` | Unset | Required for direct `Isolated` NodePorts; exact post-NAT source CIDRs admitted to the challenge port |
| `RSCTF_K8S_POD_CIDRS` | Unset | Required for direct `Isolated` NodePorts; all cluster Pod CIDRs, excluded from every admitted source block |

Use the Helm chart for the maintained ServiceAccount, Role, and network-policy configuration.

## SMTP

| Variable | Required together | Purpose |
| --- | --- | --- |
| `RSCTF_SMTP_HOST` | Yes | SMTP server hostname |
| `RSCTF_SMTP_PORT` | No | SMTP port; transport chooses a normal default when omitted |
| `RSCTF_MAIL_FROM` | Yes | From address |
| `RSCTF_SMTP_USER` | No | SMTP username |
| `RSCTF_SMTP_PASS` | With username/provider | SMTP password |

Recovery and bulk credential-delivery paths construct mail from these environment variables. Test mail from the deployed environment; similarly named values saved in Admin settings do not replace this startup configuration everywhere.

## Donations

The public supporter wall is optional and disabled by default. Configure it in
**Admin → Settings → Donations**. Trakteer is the first supported provider; the
provider selection is stored separately so later integrations do not change the
public API. Enabling the feature requires a provider API key. The key is write-only:
the admin API reports only whether one is configured, and it is never returned to
the browser.

The public `/api/donations` response contains only bounded, successful support
history: up to 10 aggregated leaderboard entries and 20 recent public messages.
Supporter email addresses, payment methods, order IDs, and the provider credential
are neither requested nor exposed. Provider responses are capped at 256 KiB and
200 rows. Successful projections are cached for five minutes, with a bounded stale
copy for provider outages, so public traffic does not fan out into one Trakteer
request per page view.

## OAuth

Set a client ID and secret for each enabled provider:

```dotenv
RSCTF_GOOGLE_CLIENT_ID=
RSCTF_GOOGLE_CLIENT_SECRET=
RSCTF_DISCORD_CLIENT_ID=
RSCTF_DISCORD_CLIENT_SECRET=
```

The callbacks are `/api/oauth/google/callback` and `/api/oauth/discord/callback`
on `RSCTF_PUBLIC_URL`; register the matching URI with each provider. Credentials
saved under **Admin → Settings → OAuth** take effect immediately and override
these environment fallbacks. Persisting an empty client ID disables that
provider. Provider endpoint override variables exist for enterprise/testing,
but most deployments should use the defaults.

## CAPTCHA

The live CAPTCHA provider is selected through the platform's stored account policy. Provider-specific environment variables include:

| Variable | Purpose |
| --- | --- |
| `RSCTF_CAPTCHA_PROVIDER` | `none`, Turnstile, or hashcash/proof-of-work mode supported by the server |
| `RSCTF_TURNSTILE_SECRET` | Cloudflare Turnstile secret |
| `RSCTF_HASHPOW_DIFFICULTY` | Proof-of-work difficulty |

Test registration after changing CAPTCHA configuration. A mismatch can lock out all new users while leaving administrator sessions unaffected.
