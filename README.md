# rsctf

rsctf is a Rust platform for running Capture-the-Flag competitions with a React and Mantine frontend. It supports accounts, teams, Jeopardy challenges, dynamic containers, scoreboards, event administration, Attack & Defense, and King of the Hill.

## Install

Users do not need to clone the repository or compile the application. The installer downloads a small Docker deployment bundle and pulls the published image:

```bash
curl -fsSL https://raw.githubusercontent.com/dimasma0305/rsctf/main/scripts/install.sh | bash
```

The default image is:

```text
dimasmaualana/rsctf:latest
```

For a real event, pin a versioned image tag. The first account registered against a new database becomes the platform administrator.

## Documentation

The task-oriented documentation covers:

- Guided Docker installation and automatic HTTPS
- Kubernetes deployment with Helm
- First login and platform setup
- Player guides for Jeopardy, A&D, and KotH
- Organizer event and challenge workflows
- Configuration, backup, update, security, and troubleshooting
- GitHub Pages, Docker image publishing, and challenge repository bindings

Run the docs locally:

```bash
cd docs
corepack enable
pnpm install --frozen-lockfile
pnpm dev
```

Build the deployable static site with `pnpm build`. GitHub Actions publishes the result to GitHub Pages.

## Deployment choices

| Path | Best for | Entry point |
| --- | --- | --- |
| Docker Compose | One server or VM | `scripts/install.sh` |
| Kubernetes | Existing cluster | `oci://ghcr.io/dimasma0305/charts/rsctf` |
| Specialized homelab | Existing Traefik + full A&D VPN | Root `docker-compose.yml` |

The generic deployment defaults to platform-only mode. Dynamic Docker challenges require the host Docker socket, which is root-equivalent access. The integrated WireGuard VPN additionally requires Linux, `NET_ADMIN`, `/dev/net/tun`, and one VPN-enabled rsctf replica.

## Repository layout

```text
src/                 Rust/axum backend
web/                 React, Mantine, and Vite client
docs/                VitePress documentation site
deploy/              Generic image-based Docker deployment
charts/rsctf/        Kubernetes Helm chart
scripts/install.sh   Interactive installation wizard
.github/workflows/   CI, docs deployment, and image publishing
```

The Rust source keeps controllers, services, models, repositories, and migrations in predictable domain-oriented modules. See [AGENTS.md](AGENTS.md) for repository conventions.

## Contributing from source

Source builds are for contributors and image publishing, not normal installation.

```bash
cargo test --locked

cd web
corepack enable
pnpm install --frozen-lockfile
pnpm check
pnpm test
pnpm build
```

The production Dockerfile builds the React client and Rust release binary into one runtime image.

## Security

Before exposing rsctf publicly, read the [deployment security guide](docs/deploy/security.md). Back up both PostgreSQL and the uploaded-file volume. Never commit deployment `.env` files, Kubernetes Secret values, repository PATs, or live CTF credentials.

## License

RSCTF is free and open-source software under the [MIT License](LICENSE.txt).
See the [licensing guide](LICENSING.md) and [NOTICE](NOTICE) for details,
including the vendored [CreepJS MIT license](web/src/lib/creepjs/LICENSE).
