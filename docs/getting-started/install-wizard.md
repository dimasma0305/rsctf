# Install with the wizard

The wizard is the easiest way to install rsctf. It asks only the questions that change the deployment, generates secrets locally, validates the configuration, and waits for the application to become healthy.

## 1. Run the remote installer

Run the installer directly from GitHub:

```bash
curl -fsSL https://raw.githubusercontent.com/dimasma0305/rsctf/main/scripts/install.sh | bash
```

This downloads only the installer and small deployment templates. Docker pulls the ready-to-run `dimasmaualana/rsctf:latest` image; it does not clone the source repository or compile the application.

If you prefer to inspect the script first:

```bash
curl -fsSLo rsctf-install.sh https://raw.githubusercontent.com/dimasma0305/rsctf/main/scripts/install.sh
less rsctf-install.sh
bash rsctf-install.sh
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

When installation succeeds, the wizard prints the site URL and useful management commands. You can check it again at any time:

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

Open the printed URL and register the first account. That account becomes the administrator and is activated immediately.

Continue with [First login and setup](./first-login).

## What the wizard changes

The wizard manages only the generic deployment files under `deploy/`:

- `deploy/.env` — private generated settings and secrets
- Docker Compose services and selected optional overrides
- Named Docker volumes for PostgreSQL and uploaded files

It does not modify the specialized root `docker-compose.yml`, delete existing data, configure your DNS provider, or open host firewall ports for you.

## Re-run or diagnose

Run the installed copy again to validate it or use the diagnostic mode. To change an existing profile, back up and edit `deploy/.env`, then run the installer again; existing secrets are never silently replaced.

```bash
cd rsctf
./scripts/install.sh
./scripts/install.sh doctor
./scripts/install.sh --help
```

If a startup check fails, inspect the most recent logs:

```bash
cd rsctf/deploy
docker compose logs --tail=200 rsctf db redis
```

See [Troubleshooting](../reference/troubleshooting) for common failures.
