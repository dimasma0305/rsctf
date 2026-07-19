# Install with the wizard

The wizard is the easiest way to install rsctf. It asks only the questions that change the deployment, generates secrets locally, validates the configuration, and waits for the application to become healthy.

## 1. Run the remote installer

Follow the [verified release-installer procedure](../reference/installer) before
executing the guided installer. The short form below assumes `install.sh` has
already passed that artifact-attestation check:

```bash
bash ./install.sh --ref vX.Y.Z
```

The bootstrap accepts only a strict `vX.Y.Z` release (or resolves the latest
release to one), downloads one coherent deployment archive plus its checksums
and GitHub artifact-attestation bundle, and verifies both by default. The
archive pins the matching immutable GHCR server image digest. It does not
clone the source repository or compile the application.

Install a current GitHub CLI with `gh attestation verify` support first. For a
repeatable event, keep the explicit `--ref vX.Y.Z` shown above.

The explicit `--skip-attestation` option is a warned checksum-only recovery or
development escape hatch, not a production installation mode.

Inspecting the verified local copy before execution is also encouraged:

```bash
less install.sh
bash install.sh --ref vX.Y.Z
```

## 2. Answer the wizard

The wizard checks Docker and Compose, then offers these practical choices:

- **Local HTTP** for evaluation on one computer
- **Automatic HTTPS** with the included Caddy reverse proxy
- **Existing reverse proxy** when your server already runs Traefik, Caddy, Nginx, or another proxy
- Optional **dynamic Docker challenges**
- Optional **A&D WireGuard VPN** on a compatible Linux host

Secrets are stored in `deploy/.env` with owner-only permissions. The file is ignored by Git and is not overwritten on later runs unless you explicitly reconfigure it.

::: danger Docker socket access
Dynamic Docker challenges require mounting `/var/run/docker.sock` into rsctf. Access to this socket is effectively root access to the host. Use a dedicated server or VM for untrusted events.
:::

## 3. Confirm the installation

When installation succeeds, the wizard prints the site URL and useful
management commands, but it does not print secrets. You can check it again at
any time:

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

Open the site with `/account/register?bootstrap=1` appended. From a trusted
local terminal in the installed `rsctf` directory, read the setup token from
the owner-only environment file:

```bash
sed -n 's/^RSCTF_BOOTSTRAP_TOKEN=//p' deploy/.env
```

Do not put the token in a URL, screenshot, shell history, or shared log. The
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

If a startup check fails, inspect the most recent logs:

```bash
cd rsctf/deploy
docker compose logs --tail=200 rsctf db redis
```

See [Troubleshooting](../reference/troubleshooting) for common failures.
