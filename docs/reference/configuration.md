# Configuration reference

rsctf reads startup configuration from environment variables. Docker stores them in `deploy/.env`; Helm maps chart values and Secrets into the Pod environment.

Restart rsctf after changing a startup value. Settings changed in **Admin → Settings** are stored in PostgreSQL and usually take effect through the application UI/runtime instead.

## Core service

| Variable | Default | Purpose |
| --- | --- | --- |
| `RSCTF_ROLE` | `all` | `all`, `web`, `control`, `engine`, `network`, or one-shot `migrate`; see the [scaling guide](../deploy/scaling) |
| `RSCTF_BIND` | `0.0.0.0:8080` | HTTP listen address inside the process/container |
| `RSCTF_DATABASE_URL` | Local development URL | PostgreSQL connection URL; required in deployment |
| `RSCTF_DB_MAX_CONNECTIONS` | `32` | Per-process database connection cap; computed minimum described below |
| `RSCTF_REDIS_URL` | Unset | Redis cache URL; when configured, Redis is required for readiness and reconnects after an outage |
| `RSCTF_DISTRIBUTED_RATELIMIT` | `false` | Share rate limits through Redis for multiple replicas |
| `RSCTF_AUTH_IP_BACKSTOP_PER_MINUTE` | `120000` | High shared-source ceiling after credential validation (`12000..1000000`) |
| `RSCTF_CREDENTIAL_IP_ADMISSION_PER_MINUTE` | `30000` | Cheap shared-source ceiling before bearer verification/token lookup (`3000..1000000`) |
| `RSCTF_JWT_SECRET` | Insecure development placeholder | Session signing secret; deployment validation requires at least 32 bytes and rejects known defaults |
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

Once any S3 setting is present, incomplete configuration fails startup instead
of falling back to local disk. S3 stores blob assets only. Persist and share
`RSCTF_STORAGE_ROOT` as well when replicas use repository worktrees, checker
files, packet captures, or snapshots.
Startup and readiness probe a small `.rsctf-health` object. Switching an
existing local installation to S3 requires copying its existing content hashes
first; rsctf does not silently dual-read or migrate historical objects.

## Registration

| Variable | Default | Purpose |
| --- | --- | --- |
| `RSCTF_ALLOW_REGISTER` | `true` | Allow public password registration; the empty-database admin bootstrap remains possible |
| `RSCTF_EMAIL_CONFIRM` | `false` | Require email-confirmation behavior for later accounts |
| `RSCTF_ACTIVE_ON_REGISTER` | `true` | Make later registered users active immediately |

`RSCTF_ADMIN_CONFIRM` and `RSCTF_USE_CAPTCHA` are loaded into one startup config structure, but the current live registration/captcha paths do not consistently consume them. Configure the active account/CAPTCHA policy in the Admin UI and test it with a normal account.

## Dynamic containers

| Variable | Default | Purpose |
| --- | --- | --- |
| `RSCTF_CONTAINER_BACKEND` | `auto` | `none`, `auto`, `docker`, or `kubernetes`; deployments should select explicitly |
| `RSCTF_CONTAINER_MAX_MEMORY_MB` | `4096` | Global upper bound for one challenge container |
| `RSCTF_CONTAINER_MAX_CPU_COUNT` | `8` | Global CPU-count upper bound for one challenge container |
| `RSCTF_DOCKER_PUBLIC_ENTRY` | Unset | Hostname/IP advertised for Docker-published challenge ports |
| `RSCTF_PROVISIONING_CONCURRENCY` | `4` | Concurrent provisioning operations |
| `RSCTF_REPO_SCAN_CONCURRENCY` | `1` | Concurrent long-lived shared checkout scans per process (`1..4`) |
| `RSCTF_TRAFFIC_CAPTURE_ENABLED` | `false` | Allow the singleton `all`/`control`/`network` worker to collect packet captures for challenges that enable it; Compose deployments must also select the matching capture overlay that grants `NET_RAW` |
| `RSCTF_CAPTURE_DEVICE` | `any` | libpcap device used by the singleton capture owner |
| `RSCTF_CAPTURE_RECONCILE_SECONDS` | `2` | Durable capture desired-state recovery interval (`1..60` seconds) |
| `DOCKER_HOST` | Local socket | Docker daemon endpoint used by the Docker backend |

If the selected explicit backend is unavailable, startup fails. `auto` can fall back to no container manager and is prohibited when the integrated VPN is enabled.

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
| `RSCTF_AD_VPN_SERVICES_NETWORK` | `rsctf-ad` | Docker A&D service network name |
| `RSCTF_AD_VPN_EGRESS_NETWORK` | `rsctf-ad-egress` | Separate bridge for explicitly allowed A&D egress |
| `RSCTF_AD_VPN_LISTEN_PORT` | `51820` | WireGuard UDP listen port |
| `RSCTF_AD_VPN_SERVER_ENDPOINT` | Derived | Public `host:port` placed in player configurations |
| `RSCTF_AD_VPN_DNS` | `1.1.1.1` | DNS server placed in generated WireGuard profiles |
| `RSCTF_AD_VPN_ALLOWED_IPS` | Derived routes | Optional explicit routes in player profiles |
| `RSCTF_AD_SSH_PORT` | `2222` | A&D SSH bastion listen port |
| `RSCTF_AD_SSH_PUBLIC_HOST` | Docker public entry | Host advertised for the SSH bastion |
| `RSCTF_AD_TICK_SECONDS` | Engine/game setting | Default A&D tick timing override (`30..600` seconds); persisted round boundaries are anchored to this cadence |
| `RSCTF_AD_CHECKER_TIMEOUT_SECONDS` | `30` | Per-check timeout; set deliberately below the event tick only after checker validation |
| `RSCTF_AD_CHECKER_CONCURRENCY` | CPU-scaled, `32..128` | Maximum concurrent A&D/KotH probes (`1..256`) |
| `RSCTF_CHECKER_UID_BASE` | `60000` | First otherwise-unused numeric UID reserved for isolated checker processes; changing it requires restart |
| `RSCTF_CHECKER_PROCESS_BUDGET` | `32` | Reserved UID count and process-wide custom-checker concurrency bound (`1..256`); pool wait counts against the checker timeout |
| `RSCTF_AD_GAME_CONCURRENCY` | `4` | Maximum games whose round pipelines run concurrently (`1..16`) |
| `RSCTF_AD_FLAG_PUSH_CONCURRENCY` | `64` | Maximum concurrent managed/BYOC flag publications (`1..256`) |
| `RSCTF_AD_FLAG_PUSH_ATTEMPTS` | `3` | Bounded flag-publication attempts per service (`1..5`) |
| `RSCTF_AD_FLAG_PUSH_TIMEOUT_SECONDS` | `2` | Timeout for one publication attempt (`1..10`) |
| `RSCTF_AD_CHECKER_MEM_MB` | Internal default | Checker sandbox memory cap |

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
scan can briefly retain checkout, game-control, and checker-publication guards
while its challenge insert needs a fourth connection. Let `R` be
`RSCTF_REPO_SCAN_CONCURRENCY` and `P` be
`RSCTF_PROVISIONING_CONCURRENCY`. The per-process pool floor is:

| Process mode | Minimum `RSCTF_DB_MAX_CONNECTIONS` |
| --- | ---: |
| One-shot `migrate` | `2` |
| `web` or `engine` | `4R + 2P + 1` |
| Non-VPN `all`, `control`, or `network` | `4R + 2P + 3` |
| Active VPN-owning `all`, `control`, or `network` | `4R + 2P + 6` |

The migration role uses only the pool's two baseline connections. A network
owner retains both the network/BYOC lease and the traffic-capture lease even
without VPN, plus one progress connection. The VPN allowance additionally
covers its `LISTEN` connection and nested kernel/allocation reconciliation. At
the defaults (`R=1`, `P=4`), web/engine need 13 connections, a non-VPN network
owner needs 15, and a VPN owner needs 18. Keep additional headroom for ordinary
request bursts where practical.

Checker and flag work is bounded by the persisted round deadline. Evidence that
finishes at or after that deadline is excluded, unresolved samples become
platform-attributed voids, and the next round keeps the authoritative cadence.
If the scheduler was unavailable for more than a complete tick, the next round
is re-anchored at recovery time; expired flag windows are never replayed.

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
| `RSCTF_K8S_AD_INGRESS_CIDRS` | Empty | Extra exact CIDRs allowed into A&D service policies |

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

## OAuth

Set a client ID and secret for each enabled provider:

```dotenv
RSCTF_GOOGLE_CLIENT_ID=
RSCTF_GOOGLE_CLIENT_SECRET=
RSCTF_DISCORD_CLIENT_ID=
RSCTF_DISCORD_CLIENT_SECRET=
```

The callback is under `/api/oauth/<provider>/callback` on `RSCTF_PUBLIC_URL`. Provider endpoint override variables exist for enterprise/testing, but most deployments should use the defaults. OAuth login reads environment credentials, not the similarly named Admin settings.

## CAPTCHA

The live CAPTCHA provider is selected through the platform's stored account policy. Provider-specific environment variables include:

| Variable | Purpose |
| --- | --- |
| `RSCTF_CAPTCHA_PROVIDER` | `none`, Turnstile, or hashcash/proof-of-work mode supported by the server |
| `RSCTF_TURNSTILE_SECRET` | Cloudflare Turnstile secret |
| `RSCTF_HASHPOW_DIFFICULTY` | Proof-of-work difficulty |

Test registration after changing CAPTCHA configuration. A mismatch can lock out all new users while leaving administrator sessions unaffected.
