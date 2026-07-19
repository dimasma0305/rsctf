# Docker deployment

The guided installer is the easiest path:

```bash
./scripts/install.sh
```

It creates `deploy/.env`, validates the complete Compose configuration, pulls
the published full-stack image, starts the stack, and waits for `/healthz`. The
first account becomes the active administrator only when registration supplies
the private token stored in the owner-only `deploy/.env`. The installer does
not print that secret. The maintained Compose files refuse an empty setup token
so a fresh database cannot come up in an unbootstrappable state.

You do not need Git or a source checkout. The bootstrap resolves only a strict
`vX.Y.Z` release, verifies the coherent deployment archive against its checksum
and GitHub artifact attestation, and uses the matching immutable server image
digest. Follow the
[verified release-installer procedure](../docs/reference/installer.md) rather
than executing a mutable branch copy. Install a current GitHub CLI with
`gh attestation verify` support first. Pin a release with `--ref vX.Y.Z`; the
explicit `--skip-attestation` option is a warned checksum-only escape hatch for
controlled recovery or development. A trusted source checkout can still run
`./scripts/install.sh` directly and use its existing local deployment files.

Retrieve the first-administrator token only from a trusted local terminal:

```bash
sed -n 's/^RSCTF_BOOTSTRAP_TOKEN=//p' deploy/.env
```

For a configuration-only run that does not start containers:

```bash
./scripts/install.sh --configure-only
```

After installation, all normal operations are run from this directory:

```bash
cd deploy
docker compose ps
docker compose logs -f rsctf
docker compose pull && docker compose up -d
docker compose down
```

`COMPOSE_FILE` in `.env` automatically selects the requested features:

- `compose.yml` is the safe base: rsctf, PostgreSQL 18, and bounded Redis.
  Its `all` role drops Docker's default capability set and adds only the
  checker identity/cleanup capabilities, `NET_ADMIN` for exact per-run egress
  rules, but not `NET_RAW`. Startup and checker execution fail closed if
  required isolation is absent. The host kernel must expose Landlock ABI v3 as
  an active LSM and support seccomp filters; startup proves the real child
  confinement path before readiness.
- `compose.capture.yml` opts an all-in-one process into packet capture and
  grants it `NET_RAW`; add it last. A split deployment uses
  `compose.roles.capture.yml` instead so only the singleton control owner gains
  that capability. Capture remains disabled when neither overlay is selected.
- `compose.docker.yml` enables dynamic challenge containers by mounting the
  Docker socket. Treat this as host-root access and use a dedicated host.
- `compose.caddy.yml` adds Caddy with automatic HTTPS. DNS must point at the
  server and inbound TCP 80/443 plus UDP 443 must be open.
- `compose.ad-vpn.yml` adds the Docker backend, isolated A&D service network,
  WireGuard hub, and SSH bastion. It requires `/dev/net/tun`, `NET_ADMIN`,
  `NET_RAW` for the iptables ipset matcher, and inbound UDP 51820/TCP 2222 by
  default. Docker A&D/KotH workloads must keep `allowEgress: false`; rsctf
  rejects `true` because a shared bridge cannot safely isolate outbound access.
  Use the Kubernetes backend with per-workload NetworkPolicy when egress is a
  challenge requirement.
- `compose.roles.yml` changes the public service to the `web` role and adds one
  checker-owning `control` owner. `web` keeps no Linux capabilities; `control`
  receives the same narrow checker/network set as `all`; the A&D VPN or capture
  companion grants `NET_RAW` only to this singleton owner. It also requires an
  explicit `RSCTF_IMAGE`; pin a reviewed digest so every
  role executes the exact same build. Its
  `compose.roles.docker.yml` and
  `compose.roles.ad-vpn.yml` companions are manual advanced scaling options;
  read `docs/deploy/scaling.md` before using them.
- `compose.workers.yml` enables the raw mTLS worker listener on a normal
  all-in-one deployment. In a split deployment, use
  `compose.roles.workers.yml` instead so only the singleton control owner gets
  port 9443 and the worker CA key. See `docs/deploy/workers.md`; this listener
  must use direct TCP or TLS passthrough, never HTTP TLS termination. The
  default `RSCTF_WORKER_LOCAL_BACKEND=none` is pure remote mode. To retain
  local Docker A&D/KotH, merge the Docker overlay first and the worker overlay
  after it, then select `RSCTF_WORKER_LOCAL_BACKEND=docker`. Add
  `compose.capture.yml` last only when local packet capture is required. This
  ordering keeps Jeopardy on workers while the earlier overlays provide the
  root-owned Docker socket and the final optional overlay couples capture to
  `NET_RAW`. This hybrid is currently all-in-one only and may also merge
  `compose.ad-vpn.yml`. Split worker deployments must remain
  pure remote: local lifecycle and VPN policy requests are not yet delegated
  from web to the singleton owner, and granting every web replica a Docker
  socket would violate the security boundary.

Persistent data lives in the `postgres_data` and `files_data` Docker volumes.
Back up both. Redis is intentionally non-persistent and capped at 256 MB.

The managed database uses PostgreSQL 18's versioned data directory. Changing an
existing PostgreSQL 16/17 installation to this Compose file is a major-version
upgrade, not an in-place image update: restore a logical dump into a fresh
`postgres_data` volume, or use PostgreSQL's `pg_upgrade` procedure. Pointing the
PostgreSQL 18 process at an older data directory is unsupported.
