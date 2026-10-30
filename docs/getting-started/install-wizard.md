# Install with the wizard

The wizard is the easiest way to install rsctf. It asks only the questions that change the deployment, generates secrets locally, validates the configuration, and waits for the application to become healthy.

## 1. Prepare your server and run the installer

Use a Linux host. If Docker is missing, the installer offers to run Docker's
official installer with administrator access. You can also install
[Docker Engine and Compose v2](https://docs.docker.com/engine/install/) yourself.
If your user cannot access Docker afterward, follow the printed Docker
post-install link and rerun the same command. Existing Docker installations
are never replaced automatically.
Rust, Node.js, Git, and a source checkout are not needed. Before setup begins,
the installer checks Docker and Compose and explains how to fix missing prerequisites.

Run this in a terminal:

```bash
curl --proto '=https' -fsSL https://github.com/dimasma0305/rsctf/releases/latest/download/install.sh | bash
```

The bootstrap accepts only a strict `vX.Y.Z` release (or resolves the latest
release to one), downloads one coherent deployment archive plus its checksums
and GitHub artifact-attestation bundle, and verifies both by default. The
archive pins the matching immutable GHCR server image digest. It does not
clone the source repository or compile the application.

The installer uses Bash and reads wizard answers from your terminal, so the
pipe remains reserved for the script. It waits for the complete script before
performing any work. This entry point trusts HTTPS for the installer itself;
the release bundle is independently verified. To verify the installer before
execution too, use the [manual procedure](../reference/installer#verify-the-installer-before-execution).

For a repeatable event, pin the release with `--ref`:

```bash
curl --proto '=https' -fsSL https://github.com/dimasma0305/rsctf/releases/latest/download/install.sh | bash -s -- --ref vX.Y.Z
```

The explicit `--skip-attestation` option is a warned checksum-only recovery or
development escape hatch, not a production installation mode.

## 2. Choose local or public

The first screen offers:

- **Try locally**: accept the default and open `http://localhost:8080` on the installation host.
- **Public server**: enter your domain; the included Caddy proxy configures HTTPS.
- **Advanced setup**: select an existing reverse proxy, a custom local port,
  dynamic Docker challenges, or an A&D WireGuard VPN. You can also pass `--advanced`.

For a public server, point DNS at the server and allow inbound TCP 80/443 and UDP
443 before installing. The basic profiles support accounts, teams, file-based
challenges and scoring. They leave local Docker challenge hosting and VPN disabled.

Secrets are stored in `deploy/.env` with owner-only permissions. The file is ignored by Git and is not overwritten on later runs unless you explicitly reconfigure it.

::: danger Docker socket access
Dynamic Docker challenges require mounting `/var/run/docker.sock` into rsctf. Access to this socket is effectively root access to the host. Use a dedicated server or VM for untrusted events.
:::

## 3. Confirm the installation

When installation succeeds, the wizard shows the setup URL and the setup token
from the first-start Docker logs. You can check service status again at any time:

```bash
cd rsctf/deploy
docker compose ps
docker compose logs -f rsctf
curl -fsS http://127.0.0.1:8080/healthz
curl -fsS http://127.0.0.1:8080/livez
```

An `ok` response from `/livez` means the HTTP process can serve a request.
`/healthz` additionally verifies PostgreSQL and the selected cache backend.

## 4. Create the administrator

Open the setup URL printed by the installer. Copy the displayed code into the
browser's **Setup token** field and choose your administrator account.
If you started Docker Compose directly, find the same instructions with
`docker compose logs rsctf`.

Docker creates the code automatically, persists it across restarts, and shows
it at startup while the database has no users. It is removed when the first
administrator is created. Do not share the startup logs before completing setup. The
matching first account becomes the administrator and is activated immediately.
Later registrations neither require nor honor this token.

Continue with [First login and setup](./first-login).

## What the wizard changes

The wizard manages only the generic deployment files under `deploy/`:

- the verified, versioned deployment files extracted as one release bundle
- `deploy/.env` — private generated settings and secrets
- Docker Compose services and selected optional overrides
- Named Docker volumes for PostgreSQL and uploaded files

It does not modify the specialized root `docker-compose.yml`, delete existing data, configure your DNS provider, or open host firewall ports for you.

When `./scripts/install.sh` runs inside a trusted source checkout that already
contains `deploy/compose.yml`, it uses those local files instead of downloading
a release bundle. Verify that checkout and choose an immutable `--image` when
it is not the matching tagged release.

## Re-run or diagnose

Run the installed copy again to validate it or use the diagnostic mode. To change an existing profile, back up and edit `deploy/.env`, then run the installer again; existing secrets are never silently replaced.

```bash
cd rsctf
./scripts/install.sh
./scripts/install.sh --doctor
./scripts/install.sh --help
```

To prepare VPN or Docker settings on another host, use `--configure-only`.
It requires the Docker CLI and Compose v2 to validate the configuration, but
does not require daemon access, a Docker socket, WireGuard, or `/dev/net/tun`.
Those runtime requirements are checked when you start the installation.

If a startup check fails, inspect the most recent logs:

```bash
cd rsctf/deploy
docker compose logs --tail=200 rsctf db redis
```

See [Troubleshooting](../reference/troubleshooting) for common failures.
