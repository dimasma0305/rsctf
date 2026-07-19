# Installer options

Resolve a strict release, download its installer and attestation, verify the
publisher policy, and only then execute it:

```bash
(
  set -euo pipefail
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  curl_args=(--disable --fail --silent --show-error --location \
    --proto '=https' --proto-redir '=https' --tlsv1.2 --connect-timeout 15 \
    --max-time 300 --retry 5 --retry-all-errors --retry-max-time 300 \
    --speed-limit 1024 --speed-time 30)
  latest="$(curl "${curl_args[@]}" --max-filesize 1048576 \
    -o /dev/null -w '%{url_effective}' \
    https://github.com/dimasma0305/rsctf/releases/latest)"
  prefix='https://github.com/dimasma0305/rsctf/releases/tag/'
  [[ "$latest" == "$prefix"* ]]
  version="${latest#"$prefix"}"
  [[ "$version" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]
  base="https://github.com/dimasma0305/rsctf/releases/download/${version}"
  curl "${curl_args[@]}" --max-filesize 1048576 \
    -o "$tmp/install.sh" "$base/install.sh"
  curl "${curl_args[@]}" --max-filesize 16777216 \
    -o "$tmp/attestation.json" \
    "$base/rsctf-worker-agent-attestation.json"
  gh attestation verify "$tmp/install.sh" \
    --bundle "$tmp/attestation.json" \
    --hostname github.com \
    --repo dimasma0305/rsctf \
    --signer-workflow dimasma0305/rsctf/.github/workflows/worker-agent-release.yml \
    --source-ref "refs/tags/$version" \
    --deny-self-hosted-runners
  bash "$tmp/install.sh" --ref "$version"
)
```

The bootstrap resolves GitHub's latest release only when its redirected tag is a
strict `vX.Y.Z`. It downloads one versioned deployment archive together with
its checksums and GitHub artifact-attestation bundle, verifies both integrity
and provenance by default, and then runs the installer from that archive. The
bundle selects the matching immutable
`ghcr.io/dimasma0305/rsctf@sha256:…` server image, so a normal install never
silently follows `main`, a version tag, or `latest`. It does not download the
application source or build an image.

Install a current GitHub CLI with `gh attestation verify` support before using
the remote bootstrap. The explicit `--skip-attestation` recovery option retains
HTTPS and checksum verification but gives up independent build-provenance
verification; it prints a warning and should not be used for a production
event.

## Help and diagnostics

```bash
bash scripts/install.sh --help
bash scripts/install.sh --doctor
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
| `--image IMAGE` | Deliberately override the release bundle's pinned server image |
| `--install-dir PATH` | Choose where the small deployment bundle is saved |
| `--ref vX.Y.Z` | Install one exact strict release tag instead of resolving the latest release |
| `--skip-attestation` | Explicitly accept checksum-only verification for controlled recovery/development |
| `--configure-only` | Generate and validate without starting containers |
| `--doctor` | Diagnose Docker and the existing configuration |
| `--non-interactive` | Use explicit flags and safe defaults without prompting |

Use `--help` for the complete list and current defaults. Unknown arguments are rejected.

## Automation

For unattended provisioning, explicitly provide the release tag, exposure
mode, public URL/domain where applicable, and `--non-interactive`. Keep secret
values in a protected environment or let the installer generate them locally.

Example repeatable installation after downloading and verifying `install.sh`
as shown above:

```bash
bash ./install.sh --ref v1.2.3 --mode local --non-interactive
```

Common environment overrides are:

| Value | Purpose |
| --- | --- |
| `RSCTF_IMAGE` | Deliberate server-image override; the release bundle otherwise pins its matching image |
| `RSCTF_INSTALL_DIR` | Directory receiving the deployment bundle |
| `RSCTF_BUNDLE_URL` | Controlled release-asset mirror; it must provide the same bundle, checksums, and attestation |
| `RSCTF_REF` | Strict `vX.Y.Z` release tag; branches and moving refs are rejected by remote bootstrap |

Run `--help` for the exact supported flag names in the installed version; the installer rejects unknown arguments.

## Trusted local checkout

Running `./scripts/install.sh` from a source checkout that already contains
`deploy/compose.yml` continues to use those local deployment files; it does not
download or replace them. In that mode the Git checkout is the trust boundary,
so verify the checked-out commit or signed tag yourself. Select an immutable
server image with `--image` when the checkout is not the matching release tag.

## Existing installation behavior

- An existing private environment file is preserved by default.
- The installer does not silently generate a new database password over existing volumes.
- If the environment file is missing while a managed PostgreSQL volume exists, installation stops and asks you to restore the original file.
- Changing the JWT secret logs out every session.
- Changing only the PostgreSQL environment password does not change the password stored inside an existing PostgreSQL volume.
- Destructive volume removal is never part of normal installation or diagnosis.

The setup token is not printed after installation. Retrieve it only from a
trusted local terminal when creating the first administrator:

```bash
sed -n 's/^RSCTF_BOOTSTRAP_TOKEN=//p' deploy/.env
```

`deploy/.env` is owner-readable only. Do not paste the token into a URL or run
the retrieval command in captured provisioning logs.

## Verify what will run

Before starting or after reconfiguration:

```bash
cd deploy
docker compose config --quiet
docker compose config --images
```

The second command should show the expected published rsctf image plus official PostgreSQL, Redis, and optional Caddy images.
