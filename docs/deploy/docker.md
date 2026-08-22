# Docker Compose

The generic files under `deploy/` are the recommended Docker path. The remote
bootstrap verifies one tagged deployment bundle and pulls its matching
ready-to-run image from GHCR. End users do not need the source code, Rust,
Node.js, or a local image build.

## Recommended: use the wizard

Follow the [verified release-installer procedure](../reference/installer). After
the downloaded `install.sh` passes attestation verification, run:

```bash
bash ./install.sh --ref vX.Y.Z
```

See [Install with the wizard](../getting-started/install-wizard) for the guided flow.

## Image used by the installer

For release `vX.Y.Z`, the verified bundle selects:

```text
ghcr.io/dimasma0305/rsctf@sha256:<release-digest>
```

The bootstrap resolves the latest release to a strict version tag by default;
use `--ref vX.Y.Z` to make that choice explicit. It rejects branches and moving
refs, and it never substitutes `latest` or the version tag. The wizard still
accepts a deliberate image override for an immutable digest or a private
mirror.

After installation, validate and start the downloaded deployment bundle with:

```bash
cd rsctf/deploy
docker compose config --quiet
docker compose pull
docker compose up -d
docker compose ps
```

That sequence is for the first start of a new installation only. It is not an
upgrade procedure for a database already served by rsctf.

Check the local listener:

```bash
curl -fsS http://127.0.0.1:8080/healthz
curl -fsS http://127.0.0.1:8080/livez
```

## Optional Compose layers

The installer writes the selected files to `COMPOSE_FILE` in `deploy/.env`, so normal management remains `docker compose ...` from that directory.

### Dynamic Docker challenges

Add the Docker-backend override to give rsctf access to the host daemon. This lets rsctf create per-team challenge containers.

::: danger Root-equivalent boundary
The Docker socket lets the rsctf process control the host daemon. Socket access,
including membership in the host's `docker` group, is effectively host-root
access; container or systemd hardening does not remove that authority. A
compromise of the public application can become a host compromise. Run it on a
dedicated VM/server and keep the host free of unrelated sensitive workloads.
:::

Jeopardy challenges use `PlatformProxy` by default. Selecting direct port mapping,
or enabling `VPN access required` for an event, publishes daemon-selected TCP ports
instead; restrict those ports to the intended audience and set
`RSCTF_DOCKER_PUBLIC_ENTRY` to the hostname or address players can actually reach.
In `PlatformProxy` mode, the maintained Compose files bind each port to
the gateway of a dedicated private `challenge-proxy` bridge. The authenticated
proxy stores this private address as its target. Docker routes host-bound ports
between bridges by default, so bridge membership alone is not the security
boundary: the Docker overlay also starts `rsctf-proxy-firewall`. That guard uses
the host network namespace and only `NET_ADMIN` to install a bind-scoped rule in
both `INPUT` and `DOCKER-USER`; only packets entering the named proxy bridge from
its configured subnet are admitted. Docker-backed rsctf roles wait for the guard
to become healthy before starting, and it periodically repairs removed rules.

Keep `RSCTF_DOCKER_PROXY_BIND`, `RSCTF_CHALLENGE_PROXY_SUBNET`, and the
15-character-or-shorter `RSCTF_CHALLENGE_PROXY_BRIDGE` aligned. An absent,
public, wildcard, mismatched, or unguarded value fails startup or launch instead
of falling back to `0.0.0.0`. This sidecar intentionally has authority over the
host firewall, but it has no Docker socket, devices, writable filesystem, or
capability other than `NET_ADMIN`. Inspect local firewall-policy compatibility
before enabling the Docker backend.

Check the guard without changing rules:

```bash
docker compose exec -T rsctf-proxy-firewall \
  /usr/local/sbin/rsctf-proxy-firewall check
```

`docker compose down` stops application dependents before the guard and removes
its rules. A force-kill or host crash may deliberately leave the restrictive
bind-derived chain behind. After all Docker-backed rsctf roles and challenge
containers are stopped, remove only that managed chain with:

```bash
docker compose run --rm --no-deps rsctf-proxy-firewall remove
```

Docker challenge containers carry a hashed installation scope. All replicas in
one installation must use the same `RSCTF_DOCKER_SCOPE`; independent
installations sharing a daemon need different scopes and different A&D network
names/CIDRs. The default derives from `RSCTF_JWT_SECRET`. Set an explicit value
before rotating that secret so existing workloads remain owned by the same
installation.

Every challenge writable layer is capped by its `storageLimit` (512 MiB when
unset). The local Docker backend fails closed unless the daemon storage driver
supports Docker's per-container `size` option. For `overlay2`, put Docker's data
root on XFS mounted with project quotas (`pquota`) and confirm `docker info`
reports `Backing Filesystem: xfs`; btrfs, devicemapper, zfs, and windowsfilter
are also recognized. An unsupported driver makes container creation unavailable
instead of silently ignoring the author-selected limit.

Docker deliberately rejects `allowEgress: true` for every A&D and KotH
workload. A shared external bridge cannot prevent one hostile workload from
reaching peers, private networks, or cloud metadata, so treating that bridge as
outbound-only would fail open. Keep `allowEgress: false` on Docker. If a service
requires outbound access, use the Kubernetes backend with a NetworkPolicy-capable
CNI; rsctf creates a per-workload egress policy there.

The base `all` role drops Docker's default capability set. It receives only the
identity and cleanup capabilities needed by checker children, `NET_ADMIN` for
their uid-scoped default-deny egress chain, but not `NET_RAW`. Each run receives
one temporary target IP/port rule. Startup and checker execution fail if rule
management is unavailable. Startup also runs a real child through the Landlock
ABI v3 and seccomp path before readiness, so the Linux host kernel must enable
Landlock as an active LSM and seccomp filters. In a split deployment, `web`
receives no Linux capabilities and the narrow set stays on the checker-owning
`control` service.

Packet capture is a separate least-privilege opt-in. Add
`compose.capture.yml` last for an all-in-one deployment, or
`compose.roles.capture.yml` last for split roles. The matching overlay both
enables capture and grants `NET_RAW` only to its owner; setting the environment
variable without the overlay is not sufficient.

Capture files rotate at 128 MiB or one hour by default, retain at most 256 MiB
per challenge participation, stop before the capture filesystem drops below
512 MiB free, and expire 14 days after the game ends. Operators can tune these
bounds with `RSCTF_CAPTURE_MAX_FILE_BYTES`,
`RSCTF_CAPTURE_MAX_PARTICIPATION_BYTES`,
`RSCTF_CAPTURE_FREE_SPACE_FLOOR_BYTES`, `RSCTF_CAPTURE_MAX_FILE_SECONDS`, and
`RSCTF_CAPTURE_RETENTION_DAYS`.

### Automatic HTTPS

The Caddy override publishes ports 80 and 443, obtains a certificate for the configured domain, and proxies the app. Point the domain's A/AAAA record at the server before starting it.

### A&D VPN

The VPN override adds:

- `NET_ADMIN`
- `NET_RAW` for the iptables ipset matcher
- `/dev/net/tun`
- IPv4 forwarding inside the container
- UDP 51820 for WireGuard
- TCP 2222 for the A&D SSH bastion
- An internal Docker network for A&D services

It requires rootful Docker on Linux, WireGuard/ipset kernel support, non-overlapping network ranges, and a reachable public endpoint. `NET_RAW` stays on the singleton VPN owner; public web replicas receive neither VPN capability. Test this mode on the production host before the event.

For opt-in event security, configure the independent event secrets and the
HTTPS proof route in `deploy/.env`, then add `event-sensor` to
`COMPOSE_PROFILES`. The sensor service in `compose.ad-vpn.yml` shares the VPN
owner's network namespace, runs read-only with only `NET_RAW`, and is limited to
0.5 CPU and 128 MiB. Do not enable its profile with an empty
`RSCTF_EVENT_SENSOR_TOKEN`; it deliberately exits instead of collecting
unauthenticated telemetry. See [Event VPN and anti-cheat telemetry](../organizers/event-security).

The Docker A&D services bridge is internal-only. Enabling a challenge's
`allowEgress` setting is rejected before Docker creates or pulls the workload;
Kubernetes is required for isolated allowed egress.

### Multiple web replicas

Do not scale the default `all` service. The advanced `compose.roles.yml`
override changes the public service to `web` and adds one `control` owner;
its Docker and A&D companion overrides place the required capabilities on that
topology. It requires Docker Compose v2.24+ because it removes the base fixed
host port with `!reset`, plus Caddy or another load balancer.

Every role mounts the same `files_data` volume on this one host. All replicas
that serve container-management APIs must also address the same Docker daemon;
independent Docker hosts are unsupported. See
[Scale the single binary](./scaling) for migration, Compose-file combinations,
BYOC routing, connection budgeting, and scale-down commands.

## The root Compose file

The repository's root `docker-compose.yml` is a specialized homelab deployment with an external Traefik network and required A&D VPN. It is intentionally not the generic quick start. Use it only when your environment already matches its Traefik, domain, network, TUN, and Docker-socket assumptions.

## Common commands

Run these from the installed `rsctf/deploy/` directory:

```bash
docker compose ps
docker compose logs -f rsctf
docker compose restart rsctf
docker compose down
```

`docker compose down` preserves named volumes. Never add `--volumes` unless you intend to permanently delete the managed database and files.

For an upgrade, take the database and file backup, pin one immutable image
digest, and run the packaged `../scripts/compose-maintenance-cutover.sh`. Do not
substitute `docker compose pull && docker compose up`: schema-incompatible
releases require every old runtime to be stopped before migration. The exact
command and fail-closed retry contract are in [Operations](./operations.md#update-docker-compose).
