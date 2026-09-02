# Source development

The source-development stack provides a fast feedback loop without publishing a
Git branch or building a release image. It is deliberately separate from every
installed or production RSCTF instance.

## Start the development stack

Install Docker, Rust, Node.js 22 or later, and pnpm. From the repository root,
run:

```sh
node scripts/dev.mjs
```

The runner:

- starts PostgreSQL and Redis in the dedicated `rsctf-source-dev` Compose project;
- binds every service to loopback rather than exposing a development server;
- generates stable local-only secrets under the ignored `.rsctf-dev/` directory;
- builds the Rust API through the shared bounded Cargo target, then restarts it after
  a locally batched backend change;
- runs Vite with hot module replacement for changes under `web/src/`; and
- runs the real suspicion reconciler for anti-cheat development; and
- disables round scheduling, checker execution, container provisioning, packet
  capture, and the event VPN.

Open `http://localhost:63000`. The terminal prints the first-admin bootstrap
token. You can print it again without starting the services:

```sh
node scripts/dev.mjs --token
```

The first Rust build can take several minutes. Later rebuilds reuse the same bounded
Cargo target and compiler cache as other worktrees, and source changes are debounced
into local batches. PostgreSQL, Redis, uploaded development files, and generated
secrets persist across source-process restarts.

## Develop on a remote machine

Vite and the API stay loopback-only. Forward the frontend port through SSH:

```sh
ssh -L 63000:127.0.0.1:63000 user@development-host
```

Then open `http://localhost:63000` on your workstation. Vite proxies API,
asset, and WebSocket requests to the local Rust process, so the browser needs
only that one tunnel. Do not point this workflow at production PostgreSQL,
Redis, storage, secrets, or a production API.

## Operations and overrides

```sh
node scripts/dev.mjs --status
node scripts/dev.mjs --down
```

`--down` stops only the development dependency containers and preserves their
named database volume. The source processes stop with `Ctrl+C`; dependencies
remain warm for the next run.

These optional variables change loopback ports when the defaults conflict:

| Variable                  |                        Default |
| ------------------------- | -----------------------------: |
| `RSCTF_DEV_BACKEND_PORT`  |                        `18080` |
| `RSCTF_DEV_FRONTEND_PORT` |                        `63000` |
| `RSCTF_DEV_DB_PORT`       |                        `55432` |
| `RSCTF_DEV_REDIS_PORT`    |                        `56379` |
| `RSCTF_DEV_RUST_LOG`      | `rsctf=debug,tower_http=debug` |

Set `RSCTF_DEV_PUBLIC_URL` only when a trusted local reverse proxy supplies a
canonical HTTP(S) origin. Prefer SSH forwarding for ordinary remote work.

For a temporary HTTPS review hostname routed by the host's existing Traefik
network, bind Vite only to that Docker bridge and start the opt-in gateway:

```sh
RSCTF_DEV_PUBLIC_URL=https://dev.1pc.tf \
RSCTF_DEV_FRONTEND_HOST=172.1.0.1 \
node scripts/dev.mjs --public
```

The gateway publishes no host port; Traefik is the only public entry point.
Use the actual address assigned to the `traefik` bridge on the development
host. Never use `0.0.0.0` or point this stack at production secrets/services.

The standard source runner intentionally disables container provisioning. A
Docker backend applies the configured writable-layer limit when the daemon and
backing filesystem support it. On an incompatible host (for example overlay2
on ext4), rsctf keeps the configured value but starts the workload without a
writable-layer quota and displays a persistent warning in the challenge
editor. CPU, memory, PID, network, log, and lifecycle limits remain active.
Monitor free disk space closely; production should use overlay2 on XFS with
project quotas (or another supported driver) or a quota-capable worker.

This stack is for debugging and review. A production deployment must still be
built by the release workflow and rolled out by immutable image digest.

## Exercise the full Docker and VPN runtime locally

Use `compose.dev.yml` when a change must exercise container provisioning, A&D,
KotH, or the event VPN. It reuses the isolated development PostgreSQL and Redis,
but runs the shared locally linked Rust binary inside a stable development
runtime image. A backend-only source change therefore needs no release image:

```sh
scripts/bounded-cargo.sh build --locked
docker compose -f compose.dev.yml up --detach --build --wait
```

Create `.rsctf-dev/runtime.container.env` first with the deployment-specific
RSCTF settings and secrets required by the feature under test. Keep it mode
`0600`; `.rsctf-dev/` is ignored and must never be committed. The dedicated A&D
network must also exist with the configured service subnet. For the standard
development subnet:

```sh
docker network inspect rsctf-source-dev-ad >/dev/null 2>&1 || \
  docker network create --subnet 10.13.41.0/24 rsctf-source-dev-ad
```

After a Rust edit, rebuild and restart only the backend:

```sh
scripts/bounded-cargo.sh build --locked
docker compose -f compose.dev.yml restart backend
```

The binary directory, files directory, API/SSH/VPN ports, and backend A&D
address can be overridden with `RSCTF_DEV_BINARY_DIR`,
`RSCTF_DEV_STORAGE_DIR`, `RSCTF_DEV_BACKEND_PORT`, `RSCTF_DEV_SSH_PORT`,
`RSCTF_DEV_VPN_PORT`, and `RSCTF_DEV_AD_BACKEND_IP`. Do not use this full-runtime
profile with production databases, secrets, storage, or networks.
