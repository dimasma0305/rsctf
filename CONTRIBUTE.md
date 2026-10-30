# Contributing to RSCTF

This guide is for people changing RSCTF's source code. To install and use the
published application, start with the [README](README.md#install).

RSCTF has a Rust/axum backend and a React, Mantine, and Vite client. PostgreSQL
and Redis provide its database and cache. Read [AGENTS.md](AGENTS.md) and its
required [engineering workflow](.agents/skills/rsctf-engineering/SKILL.md) before
editing. The [application map](.agents/skills/rsctf-engineering/references/application-map.md)
connects features to their frontend, backend, and test owners.

## Development setup

Use Docker Engine with Compose v2, Node.js 22, pnpm 11.8.0, and the Rust
toolchain pinned in [rust-toolchain.toml](rust-toolchain.toml). The
[source-development guide](docs/reference/source-development.md) covers native
prerequisites, remote SSH forwarding, port overrides, status, and shutdown.

For an isolated local stack with PostgreSQL, Redis, automatic Rust rebuilds,
and Vite hot module replacement, run from the repository root:

```bash
node scripts/dev.mjs
```

Open `http://localhost:63000`. Development data, storage, and secrets live in a
separate Compose project and the ignored `.rsctf-dev/` directory. This workflow
does not use production data or require a GitHub push.

## Repository layout

```text
src/                         Rust/axum backend
web/                         React, Mantine, and Vite client
docs/                        VitePress documentation site
deploy/                      Generic image-based Docker deployment
charts/rsctf/                Kubernetes Helm chart
agents/byoc-agent/           Team-side tunnel agent
agents/worker-agent/         Trusted worker agent
scripts/install.sh           Interactive installation wizard
scripts/install-worker.sh    Linux worker-agent installer
scripts/install-worker.ps1   Windows worker-agent installer
.github/workflows/           CI, documentation, and release publishing
```

Controllers, services, models, repositories, and migrations are grouped by
domain. Follow the ownership and SQL rules linked from [AGENTS.md](AGENTS.md).

## Checks and source builds

Install the web dependencies:

```bash
corepack enable
pnpm --dir web install --frozen-lockfile
```

Use the bounded wrappers for local Cargo and frontend checks. They share one
compile slot across worktrees and apply the repository's resource limits.

```bash
scripts/bounded-cargo.sh fmt --all -- --check
scripts/bounded-cargo.sh clippy --all-targets --all-features --locked -- -D warnings
scripts/bounded-cargo.sh test --locked
scripts/bounded-cargo.sh build --all-targets --locked

scripts/bounded-frontend.sh check
scripts/bounded-frontend.sh lint:check
scripts/bounded-frontend.sh test
scripts/bounded-frontend.sh build
```

The [testing and coverage reference](docs/reference/testing.md) covers the
database-backed suite, coverage baseline, agent checks, browser audits, and
event-scale harness. Run the focused checks for the changed area and the
completion gates required by the engineering workflow. Include validation
results in the pull request.

The production [Dockerfile](Dockerfile) builds the React client and Rust release
binary into one runtime image. Normal installations use that published image.

## Documentation development

```bash
pnpm --dir docs install --frozen-lockfile
pnpm --dir docs dev
```

Build the static site from the repository root with:

```bash
scripts/bounded-frontend.sh exec pnpm --dir ../docs build
```

[docs-build.yml](.github/workflows/docs-build.yml) defines the documentation
checks, including journal PDF generation. [docs.yml](.github/workflows/docs.yml)
publishes the site to GitHub Pages.

## CI and publishing

- [ci.yml](.github/workflows/ci.yml) selects and runs the required checks.
- [image.yml](.github/workflows/image.yml) builds, verifies, and attests the server
  and BYOC agent images.
- [worker-agent-release.yml](.github/workflows/worker-agent-release.yml) publishes
  the worker archives, installers, checksums, and verified deployment bundle.
- [chart.yml](.github/workflows/chart.yml) publishes the Helm chart.

Before publishing, verify that coordinated package and chart versions agree:

```bash
python3 scripts/check-release-versions.py
```

Follow the [verification and release workflow](.agents/skills/rsctf-engineering/references/verification-release.md)
for exact-source checks, immutable image digests, migrations, and production
verification. Documentation-only changes that cannot alter a built artifact can
skip application publication and deployment.

## Secrets and security

Read the [deployment security guide](docs/deploy/security.md). Never commit
deployment `.env` files, Kubernetes Secret values, repository PATs, or live CTF
credentials. Development, test, and production data must remain separate.
