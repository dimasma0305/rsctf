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
- runs the Rust API from the working tree and rebuilds it after backend changes;
- runs Vite with hot module replacement for changes under `web/src/`; and
- runs the real suspicion reconciler for anti-cheat development; and
- disables round scheduling, checker execution, container provisioning, packet
  capture, and the event VPN.

Open `http://localhost:63000`. The terminal prints the first-admin bootstrap
token. You can print it again without starting the services:

```sh
node scripts/dev.mjs --token
```

The first Rust build can take several minutes. Later rebuilds reuse Cargo's
incremental cache. PostgreSQL, Redis, uploaded development files, and generated
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
