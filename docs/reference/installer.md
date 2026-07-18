# Installer options

Run the guided installer with no arguments:

```bash
curl -fsSL https://raw.githubusercontent.com/dimasma0305/rsctf/main/scripts/install.sh | bash
```

The script downloads deployment templates only and pulls `dimasmaualana/rsctf:latest` by default. It does not download the application source or build an image.

## Help and diagnostics

```bash
bash scripts/install.sh --help
bash scripts/install.sh doctor
```

`doctor` checks the local Docker/Compose installation, the selected deployment configuration, required devices/networks where applicable, and service health without deleting data.

## Important options

| Option | Purpose |
| --- | --- |
| `--mode local` | Local HTTP bound to loopback; the default |
| `--mode caddy --domain ctf.example.org` | Public HTTPS with the included Caddy proxy |
| `--mode proxy --public-url https://ctf.example.org` | Use an existing reverse proxy |
| `--trusted-proxy-cidrs CIDRS` | Trust forwarded client addresses only from these proxy CIDRs |
| `--with-docker` | Enable dynamic Docker challenge containers |
| `--with-ad-vpn` | Enable Docker challenges plus WireGuard and the SSH bastion |
| `--public-entry HOST` | Address players use for dynamic challenge/VPN ports |
| `--image IMAGE` | Pull a specific published image/tag |
| `--install-dir PATH` | Choose where the small deployment bundle is saved |
| `--configure-only` | Generate and validate without starting containers |
| `--doctor` | Diagnose Docker and the existing configuration |
| `--non-interactive` | Use explicit flags and safe defaults without prompting |

Use `--help` for the complete list and current defaults. Unknown arguments are rejected.

## Automation

For unattended provisioning, explicitly provide the exposure mode, public URL/domain where applicable, image tag, and `--non-interactive`. Keep secret values in a protected environment or let the installer generate them locally.

Example local pinned installation:

```bash
curl -fsSL https://raw.githubusercontent.com/dimasma0305/rsctf/main/scripts/install.sh | \
  bash -s -- --mode local --image dimasmaualana/rsctf:1.2.3 --non-interactive
```

Common environment overrides are:

| Value | Purpose |
| --- | --- |
| `RSCTF_IMAGE` | Published image, such as `dimasmaualana/rsctf:1.2.3` |
| `RSCTF_INSTALL_DIR` | Directory receiving the deployment bundle |
| `RSCTF_BUNDLE_URL` | Raw GitHub/release base used to download templates from a fork |
| `RSCTF_REF` | Branch or tag used with the default GitHub raw bundle URL |

Run `--help` for the exact supported flag names in the installed version; the installer rejects unknown arguments.

## Existing installation behavior

- An existing private environment file is preserved by default.
- The installer does not silently generate a new database password over existing volumes.
- If the environment file is missing while a managed PostgreSQL volume exists, installation stops and asks you to restore the original file.
- Changing the JWT secret logs out every session.
- Changing only the PostgreSQL environment password does not change the password stored inside an existing PostgreSQL volume.
- Destructive volume removal is never part of normal installation or diagnosis.

## Verify what will run

Before starting or after reconfiguration:

```bash
cd deploy
docker compose config --quiet
docker compose config --images
```

The second command should show the expected published rsctf image plus official PostgreSQL, Redis, and optional Caddy images.
