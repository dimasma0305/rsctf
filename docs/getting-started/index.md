# Choose your path

You can use rsctf on one machine for a small event or run it in Kubernetes. Start with the simplest option that supports your challenges.

## Which deployment should I use?

| Your goal | Recommended path | What it enables |
| --- | --- | --- |
| Try rsctf on this computer | **Local Docker** | Accounts, teams, static challenges, scoring, and administration over local HTTP |
| Run a public event on one Linux server | **Docker + automatic HTTPS** | A public site with PostgreSQL, Redis, persistent uploads, and TLS |
| Launch dynamic challenge containers | **Docker challenge backend** | Per-player or per-team containers; requires Docker socket access |
| Run A&D with the integrated VPN | **Docker + A&D VPN** | WireGuard, isolated A&D services, and the SSH bastion; Linux-only and privileged |
| Use an existing cluster | **Kubernetes + Helm** | The platform and optional challenge Pods/Services; some Docker-only features are unavailable |

If you are unsure, choose **Local Docker**. You can re-run the wizard later and enable more features.

## What the standard installation includes

- One rsctf container containing both the React web interface and Rust server
- PostgreSQL for durable application data
- Redis for bounded, non-persistent caching
- Persistent storage for uploaded files
- Automatic database migrations when rsctf starts

## Before you begin

For Docker, install Docker Engine or Docker Desktop with the Compose v2 plugin. The installer pulls a ready-to-run image; Rust, Node.js, pnpm, and a source checkout are not required. A public A&D/VPN deployment requires a Linux host with rootful Docker, WireGuard kernel support, and `/dev/net/tun`.

For Kubernetes, install `kubectl` and Helm 3, select the intended cluster context, and make sure you have permission to create namespaces, workloads, Secrets, PVCs, and RBAC resources.

::: warning Protect the first registration
The first account created through password registration becomes the platform administrator. Complete the first registration immediately after installation, then review the registration policy in **Admin → Settings**.
:::

## Next step

Use the [installation wizard](./install-wizard). It generates unique secrets and validates the selected deployment before anything starts.
