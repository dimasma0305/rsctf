# Import the sample challenge repository

The public [`dimasma0305/rsctf-challenges`](https://github.com/dimasma0305/rsctf-challenges) repository contains a hidden demonstration event with seven manifests covering every challenge type, including both A&D hosting modes. It is also pinned in the rsctf source tree as the `examples/challenge-repository` Git submodule. Repository Bindings can scan the challenge repository directly and build its five container challenges locally from the checked-in `src/Dockerfile` contexts.

## What the sample contains

| Example | Type | Current status |
| --- | --- | --- |
| Welcome file | `StaticAttachment` | Runnable after you review/replace the public demo flag |
| Per-team bundle | `DynamicAttachment` | Schema example only; keep disabled because imported per-team assignment is incomplete |
| Shared flag service | `StaticContainer` | Built during import and runnable on the Docker backend |
| Personal flag service | `DynamicContainer` | Runnable on the Docker backend; reads the injected `RSCTF_FLAG` |
| Hosted flag-file service | `AttackDefense` | Platform-managed raw TCP line service and `pwntools==4.15.0` checker; requires a complete Docker A&D staging setup |
| Self-hosted flag-file service | `AttackDefense` | BYOC HTTP service and `httpx==0.28.1` checker reached through the outbound relay |
| Claim marker | `KingOfTheHill` | Shared hill and functional checker; currently requires Docker for marker reads |

The event manifest is `.gzevent` at the challenge repository root. Challenges use the layout `AD/<category>/<challenge>/`, `Koth/<category>/<challenge>/`, or `Jeopardy/<category>/<challenge>/`. Because every challenge lives below the root event manifest, the binding imports them into the same game.

## Import it on your rsctf instance

1. Sign in as an administrator.
2. Open **Admin → Repository Bindings**. On the referenced deployment, this is [tcp.1pc.tf/admin/repo-bindings](https://tcp.1pc.tf/admin/repo-bindings).
3. Add this repository URL:

   ```text
   https://github.com/dimasma0305/rsctf-challenges.git
   ```

4. Set the Git ref to `main` (or the release tag you want to demonstrate).
5. Leave the GitHub token empty while the repository is public.
6. Select the run-immediately option or save and choose **Scan now**.
7. Open the newest scan result.

A successful first scan reports one created game and seven imported challenges with zero manifest failures. It also runs five local service-image builds and prepares three checkers.

## Safe result after import

- The game is **hidden**.
- Every imported challenge is **disabled**.
- The public flags are documentation values, not secrets.
- Container manifests omit `containerImage`; rsctf builds their `src/Dockerfile` contexts.
- The two A&D checkers and the KotH checker use protocol-neutral `lib.py` + `run.py` templates with cryptographically shuffled registered checks; Pwn and Web add pinned `requirements.txt` files, while KotH needs none.

These defaults prevent the demo from silently becoming a public competition. They do not replace review: inspect every challenge, image, build/checker result, network rule, and flag before enabling anything.

## Required deployment features

The attachment examples work in platform-only mode. Import-time source builds require the Docker challenge backend and a reachable daemon. Split-role installations may enable them only when every builder and container owner uses the same daemon; acknowledge that verified topology with `RSCTF_SHARED_DOCKER_DAEMON=true`. Kubernetes and independent node-local daemons require prebuilt registry images instead.

Platform-hosted A&D also needs an isolated service network, scheduler, checker sandbox, accepted test teams, and optionally WireGuard. Self-hosted A&D builds the challenge service locally but sends it to each authorized team to run behind the BYOC relay; platform resource and egress settings do not constrain that team-owned container. The separately configured relay-agent image is still a platform dependency and must be mirrored when the deployment cannot pull from Docker Hub. KotH holder election requires backend exec access to read `/koth/king`, and its local checker verifies service health without reading or changing that marker. Preparing the sample Pwn and Web checkers also requires outbound access from the scanning rsctf process to PyPI and its package file hosts; checker runtime egress remains restricted to the supplied challenge target.

::: warning Dynamic Attachment is intentionally not runnable
The current importer creates one challenge-level attachment and unassigned flag rows, but does not assign a per-team flag/attachment to the participation instance. It still imports successfully because it demonstrates the schema. Leave it disabled until that application gap is implemented and tested.
:::

## Import-time image builds

Each of the five container challenges keeps a small Dockerfile and Python service in `src/`. Its manifest intentionally omits `containerImage`, which tells Repository Bindings to archive the challenge package and build `src/Dockerfile` during a trusted scan. The generated image remains local to the shared Docker daemon and is pinned to the immutable build result before the challenge can run. The self-hosted A&D service is then made available through rsctf's authenticated BYOC image endpoint rather than being pulled as a prebuilt challenge image.

The Dockerfiles currently use `python:3.12-alpine`, so Docker may still pull that base image when it is not cached. Mirror or replace the base if the build host must not contact Docker Hub at all. For a production event, review and pin every base image, inspect the import build logs, test the resulting services, and replace every public demo flag.

## Understand rescans

Event settings from `.gzevent` seed only a new game; later Admin edits to that game are preserved. Challenges keep a stable identity based on the binding and their path relative to the event manifest. A rescan updates mutable repository-owned fields in place, so challenge IDs, solves, submissions, first-solve records, counters, and scoring history remain intact.

Removing a manifest does not silently erase played history. rsctf retains the challenge as a disabled tombstone, and rejects unsafe removal while a scored event or an unfinished A&D/KotH round still depends on it. Rehearse structural changes on staging and end/finalize the event before removing a played challenge.

## Files to learn from

- `.gzevent` — game schedule/policy and A&D timing defaults
- `challenge.yaml` — common metadata, flags, attachments, scoring, build intent, port, and A&D fields
- `dist/` — attachment packaging convention
- `src/` — Docker build context imported and built by rsctf
- `checker/lib.py` — protocol-neutral contexts, verdict mapping, `@checker`, shuffled A&D/KotH suite runners, and the legacy single-checker decorators
- `checker/run.py` — the challenge's protocol and focused, order-independent check suite; compare the hosted Pwn raw TCP checker with the self-hosted Web HTTP checker
- `checker/requirements.txt` — optional exact PyPI pins; Pwn uses `pwntools==4.15.0` and Web uses `httpx==0.28.1`
- `scripts/validate.mjs` — strict example validation used by GitHub Actions
- `scripts/test-checkers.py` — live checker smoke tests for all four verdict classes

Copy the complete checker directory when adapting a template. `run.py` imports
its sibling `lib.py`. When dependencies are necessary, every requirements entry
must be a simple exact PyPI pin; URLs, paths, pip options, editable installs,
version ranges, and source builds are rejected. rsctf installs accepted packages
wheel-only while preparing an immutable checker revision. Treat the repository
commit and all dependency pins as trusted, administrator-approved inputs.

The challenge repository's [README](https://github.com/dimasma0305/rsctf-challenges), [manifest reference](https://github.com/dimasma0305/rsctf-challenges/blob/main/CONFIGURATION.md), and [checker guide](https://github.com/dimasma0305/rsctf-challenges/blob/main/CHECKERS.md) document every file and current runtime caveat.
