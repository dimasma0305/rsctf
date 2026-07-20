# GitHub integration

rsctf uses GitHub in three distinct ways: documentation deployment, container-image publishing, and optional challenge repository bindings.

## Documentation on GitHub Pages

The `Docs` workflow validates the VitePress site on pull requests. A push to `main` builds the static pages and deploys them through the protected `github-pages` environment.

After pushing the repository:

1. Open **Settings → Pages**.
2. Set the source to **GitHub Actions**.
3. Run the Docs workflow or push a docs change to `main`.
4. Open the deployment URL shown by the workflow.

The workflow obtains the repository's Pages base path, so forks work at `/repository-name/` without hardcoded URLs. For a custom domain, configure the domain and DNS in repository settings; a `CNAME` file alone is not enough.

## Images built by GitHub Actions

The image workflow builds the full-stack Dockerfile and publishes the end-user image to the repository-linked GitHub Container Registry package:

```text
ghcr.io/dimasma0305/rsctf
```

This public GHCR package is the installer, Compose, and Helm default. When both
`DOCKERHUB_USERNAME` and `DOCKERHUB_TOKEN` are configured, the workflow also
publishes the same digest to the optional
`docker.io/dimasmaualana/rsctf` mirror; installs do not depend on that mirror.

Publishing behavior is intentionally predictable:

| Git event | Platforms | Published tags |
| --- | --- | --- |
| Pull request | amd64 | Build only; never pushed |
| Push to `main` | amd64 | `main` and a commit-SHA tag |
| Stable tag such as `v1.2.3` | amd64 + arm64 | `1.2.3`, `1.2`, `1`, and `latest` |

Release tags use strict `vX.Y.Z` syntax and must match the application, worker,
BYOC agent, and Helm chart versions in the tagged commit.

Release images include OCI metadata, BuildKit provenance, and an SBOM. GHCR
publication uses `GITHUB_TOKEN`. Optional Docker Hub publication uses the
repository secrets `DOCKERHUB_USERNAME` and `DOCKERHUB_TOKEN`.

If the optional Docker Hub secrets are added later, open **Actions → Container
image → Run workflow**. A manual run on `main` republishes the `main` and
commit-SHA tags without requiring an empty source commit.

Keep the GHCR package public so anonymous Docker and Kubernetes installations
can pull it without registry credentials. Keep the optional Docker Hub mirror
public as well when it is enabled.

## Sample challenge source validation

The standalone [`dimasma0305/rsctf-challenges`](https://github.com/dimasma0305/rsctf-challenges) repository is pinned here as the `examples/challenge-repository` Git submodule. Its `Validate challenge repository` workflow validates seven manifests, compiles and smoke-tests the hosted A&D, self-hosted A&D, and KotH checkers, and builds all five checked-in `src/Dockerfile` contexts without pushing them to a registry. Every checker keeps platform context parsing, verdict mapping, `@checker`, and the shuffled `run_ad_checker()` / `run_koth_checker()` entry points in a dependency-free, protocol-neutral `lib.py`; the legacy `@ad_checker` and `@koth_checker` wrappers remain supported. Each `run.py` implements the challenge's protocol and registers focused checks. Their order is cryptographically shuffled per checker process, every function is attempted once, and failures are aggregated deterministically. The managed Pwn example pins `pwntools==4.15.0` for its raw TCP line protocol, the self-hosted Web example pins `httpx==0.28.1`, and KotH uses standard-library HTTP. The example manifests omit `containerImage`, so a trusted Repository Bindings scan exercises rsctf's import-time build path instead of pulling prebuilt sample challenge images from Docker Hub or GHCR.

After a non-recursive rsctf clone, populate the example with `git submodule update --init --recursive`.

See [Import the sample challenge repository](../organizers/sample-repository) for the Docker-backend and shared-daemon requirements of local source builds. Challenge authors can use the standalone repository's [manifest reference](https://github.com/dimasma0305/rsctf-challenges/blob/main/CONFIGURATION.md) and [checker guide](https://github.com/dimasma0305/rsctf-challenges/blob/main/CHECKERS.md), including the separate hosted and self-hosted A&D templates and the KotH checker contract. Copy the complete checker directory. An optional `requirements.txt` beside `run.py` may contain simple, exact PyPI pins only; rsctf rejects URLs, local paths, pip options, editable or unpinned requirements, and source-only packages, then installs accepted dependencies wheel-only into the immutable checker environment.

## Helm chart package

The Helm workflow packages the chart on the same `vX.Y.Z` release tag and publishes it as an OCI artifact:

```text
oci://ghcr.io/dimasma0305/charts/rsctf
```

This lets Kubernetes operators install a versioned chart without cloning the repository. After the first publish, make the chart package public in GitHub package settings if anonymous cluster installations should be supported.

## Create a release image

Run application CI first, then create and push a semantic version tag:

```bash
git tag -s v1.2.3 -m "rsctf v1.2.3"
git push origin v1.2.3
```

Review the Actions run and the generated multi-architecture manifest before announcing the version. Pin that release tag in production instead of tracking `main`.

## Challenge repository bindings

An administrator can connect a Git repository from **Admin → Repository Bindings**. Use this to keep event and challenge definitions under review in GitHub.

Important behavior:

- A repository must contain a `.gzevent`; standalone `challenge.yaml` files are not imported.
- Each `.gzevent` defines one game and imports challenges beneath its directory.
- A rescan preserves operator-edited game settings and updates challenges in place by binding-relative manifest path, retaining challenge IDs, submissions, first solves, counters, and scoring history.
- Missing played manifests are retained as disabled tombstones, and unsafe removal is rejected while an active or unfinished event state still depends on the challenge.
- Private repositories need a PAT. Push-on-edit needs a writable branch and write-capable PAT.
- Tags or detached refs cannot receive push-back edits.
- Interval fields are stored and displayed, but background interval scanning is not currently scheduled. Use the manual scan action after a repository change.
- PATs are stored in PostgreSQL and omitted from API responses; database backups must still be treated as sensitive.
- A scan may build checked-in Dockerfiles and resolve/install pinned checker wheels. Treat the selected repository commit and its package dependencies as administrator-approved executable input, and give the scanning rsctf process outbound access to PyPI and its file hosts when requirements are present.

Prefer a fine-grained token limited to the one challenge repository and only the contents permissions the workflow needs. Use a read-only token unless push-on-edit is required.

## Contributing to the docs

Each documentation page has an **Improve this page on GitHub** link. Pull requests run the docs build, application CI runs independently, and Dependabot watches Cargo, the React client, the docs package, and GitHub Actions.
