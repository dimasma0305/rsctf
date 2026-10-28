# Finding and running the right checks

Read [verification-release.md](verification-release.md) for completion rules,
[performance-load.md](performance-load.md) for benchmark claims, and
[fast-development.md](fast-development.md) for worktree/build batching.
Commands below run from the repository root unless stated otherwise.

The real OAuth cookie regression uses Chromium, a disposable PostgreSQL schema,
and a local provider. Set RSCTF_TEST_DATABASE_URL to a disposable database and
CHROME_BIN if Chromium is not on PATH, then run:

```sh
scripts/bounded-cargo.sh test --lib discord_browser_accepts_session_and_removes_only_state_cookie -- --ignored --test-threads=1
```

It follows real cross-site redirects and checks the next authenticated profile
request. It does not emulate a deployed TLS reverse proxy; retain that smoke test
in the release gate. CI coverage discovers and runs this ignored database test.

## Discover the smallest meaningful check

For shutdown during external work, run the
[control-job shutdown harness](../../../../tests/load/control-job-shutdown.mjs).
It uses the real control process and Docker builds/resets, with graceful and forced
exit cases. Rollout cases use durable worker state with injected observations;
they do not replace native-worker acceptance. The [load README](../../../../tests/load/README.md)
documents required isolation and the explicitly injected crash-recovery lease expiry.
The focused Rust regression for a partially completed drain is in
[startup_tests.rs](../../../../src/startup_tests.rs):
`scripts/bounded-cargo.sh test --bin rsctf drain_deadline_joins_completed_tasks_once_and_cancels_remaining_work`.

For backup consistency, use the
[disposable restore drill](../../../../tests/load/restore-drill.mjs) and its
[fixture](../../../../tests/load/restore-drill/seed.sql). The load README documents
its acknowledgement, PostgreSQL container and tested-binary requirements.
It restores local files and all database rows, checks score/session/downloads,
and rejects missing or corrupt objects. Its S3 variant restores referenced contents
under a fresh prefix; multi-host and object-version-history recovery remain separate.

| Changed boundary | Locate / run first | Broader verification |
| --- | --- | --- |
| Rust logic or DTO | Owner's inline/sibling tests; filter by module/function | Warning-free build, default tests, all-feature Clippy |
| SQL, authorization, idempotency, leases | Owner's `*_tests.rs` and ignored PostgreSQL tests | Disposable real PostgreSQL; negative authorization, races and replay |
| Login/OAuth/session | [src/controllers/oauth_integration_tests.rs](../../../../src/controllers/oauth_integration_tests.rs), account tests; [tests/visual/account-access.mjs](../../../../tests/visual/account-access.mjs) | Real HTTP cookies/redirects plus browser login/recovery paths |
| Teams, cache retirement, canceled reads | [web/src/utils/PersistentCache.test.ts](../../../../web/src/utils/PersistentCache.test.ts), identity/cache utility tests; [tests/visual/team-access.mjs](../../../../tests/visual/team-access.mjs) | Strict frontend gate and backend membership tests |
| Layout/admin workflows/localization | [tests/visual/design-ux.mjs](../../../../tests/visual/design-ux.mjs), [tests/visual/admin-operations.mjs](../../../../tests/visual/admin-operations.mjs), [tests/visual/event-readiness.mjs](../../../../tests/visual/event-readiness.mjs) | Relevant page audit at desktop/320px, keyboard, Axe and translations |
| Scoreboard/pages/freeze | [src/controllers/game/scoreboard_page_tests.rs](../../../../src/controllers/game/scoreboard_page_tests.rs), wire/visibility tests; [tests/visual/scoreboard-page.mjs](../../../../tests/visual/scoreboard-page.mjs) | [tests/load/scoreboard-page.mjs](../../../../tests/load/scoreboard-page.mjs), canonical/full/page integrity |
| Repository YAML score curves | [score_curve_regression.rs](../../../../src/services/git_sync/tests/score_curve_regression.rs), [push-back round trips](../../../../src/services/git_sync/push_back.rs); filter `score_curve` | Real PostgreSQL `repository_update_preserves_challenge_solves_and_refreshes_content -- --ignored`: creation, omission, explicit reset, grading lock and queued Git push-back |
| A&D/KotH correctness | Tests under controllers/services `ad/`, `koth/`; [tools/ad-scoring-sim/README.md](../../../../tools/ad-scoring-sim/README.md) | [tests/load/managed-koth.md](../../../../tests/load/managed-koth.md), [tests/load/arena-read.mjs](../../../../tests/load/arena-read.mjs), real runtime lifecycle |
| Docker retry/first start | [src/services/container/real_docker_tests.rs](../../../../src/services/container/real_docker_tests.rs), [src/services/image_preflight/](../../../../src/services/image_preflight/) | Same-run Docker retry/adoption and repaired-image CI regressions |
| Trusted worker | Agent/protocol tests and Docker integration tests | [tests/load/worker-plane-local.mjs](../../../../tests/load/worker-plane-local.mjs), [tests/load/worker-plane.mjs](../../../../tests/load/worker-plane.mjs); Linux and Windows gates |
| BYOC / VPN / proxy | Tunnel/lease/auth tests; [tests/load/byoc.mjs](../../../../tests/load/byoc.mjs), [scripts/check-event-vpn.mjs](../../../../scripts/check-event-vpn.mjs) | Real agent connection/revocation/restart on the intended topology |
| Capture/security/evidence | Owner's real DB/runtime tests | [tests/load/cheat-acceptance.mjs](../../../../tests/load/cheat-acceptance.mjs), [tests/load/monitor-history.mjs](../../../../tests/load/monitor-history.mjs), [tests/load/monitor-evidence-inventory.mjs](../../../../tests/load/monitor-evidence-inventory.mjs) |
| Upload/download/storage | Storage, upload, blob-reference and authorization tests | [tests/load/asset-download.mjs](../../../../tests/load/asset-download.mjs), [tests/load/image-storage.mjs](../../../../tests/load/image-storage.mjs), retention/readback checks |
| Caches / Redis recovery | Cache tests, response-key/invalidation tests | [tests/load/redis-outage.mjs](../../../../tests/load/redis-outage.mjs), fixed-rate affected reads with correct authorization/visibility |
| Deployment / network policy | [scripts/validate-compose-security.sh](../../../../scripts/validate-compose-security.sh), [scripts/validate-helm-chart.sh](../../../../scripts/validate-helm-chart.sh) | Docker firewall tests, real Kubernetes callback isolation, live role/digest checks |
| Load harness / CI scripts | [tests/load/test/](../../../../tests/load/test/), [tests/load/package.json](../../../../tests/load/package.json) | Harness contracts and exact failing workflow phase |
| Navigation skill docs | `node --test .agents/skills/rsctf-engineering/scripts/check-navigation.test.mjs`, then `node .agents/skills/rsctf-engineering/scripts/check-navigation.mjs` | Link/module/page coverage plus source review; [effectiveness evidence](navigation-effectiveness.md) |

A test's name or source presence is discovery, not evidence it ran. Record exact
filters, environment-dependent ignores, runtime/backend and tested source SHA.
Browser interception proves UI behavior against fixtures; database/runtime tests
prove server boundaries separately.

## Rust commands and infrastructure

Use the bounded wrapper for every local Cargo invocation, including agent crates.
These are examples, not a command to run every suite after a small docs edit:

```sh
scripts/bounded-cargo.sh test --locked --lib oauth -- --nocapture
scripts/bounded-cargo.sh test --locked --lib oauth -- --ignored --nocapture
scripts/bounded-cargo.sh test --manifest-path lib/worker-protocol/Cargo.toml --locked
scripts/bounded-cargo.sh test --manifest-path agents/worker-agent/Cargo.toml --locked
scripts/bounded-cargo.sh test --manifest-path agents/byoc-agent/Cargo.toml --locked
```

Read each test's configuration helper first; an `--ignored` filter alone does not
provision services. Use disposable PostgreSQL/Redis/Docker fixtures. The exact
coverage database selection/exclusions live in
[scripts/coverage/services.json](../../../../scripts/coverage/services.json), [scripts/coverage/run.mjs](../../../../scripts/coverage/run.mjs),
[scripts/coverage/partition.mjs](../../../../scripts/coverage/partition.mjs), and [.github/workflows/ci.yml](../../../../.github/workflows/ci.yml).
Do not point fixture cleanup at a live database or print credential environments.

Final applicable Rust checks, after the implementation batch settles:

```sh
scripts/bounded-cargo.sh fmt --all -- --check
scripts/bounded-cargo.sh build --all-targets --all-features --locked
scripts/bounded-cargo.sh test --all-targets --all-features --locked
scripts/bounded-cargo.sh clippy --all-targets --all-features --locked -- -D warnings
```

## Frontend and browser commands

[web/scripts/run-tests.mjs](../../../../web/scripts/run-tests.mjs) owns client test discovery;
[web/package.json](../../../../web/package.json) also includes visual-harness contract tests.
The wrapper changes into `web/` and invokes pnpm. Do not run the Rust/frontend
wrappers concurrently; they share one cross-worktree build slot.

```sh
scripts/bounded-frontend.sh install --frozen-lockfile
scripts/bounded-frontend.sh check
scripts/bounded-frontend.sh lint:check
scripts/bounded-frontend.sh test
scripts/bounded-frontend.sh build
scripts/bounded-frontend.sh exec node ../tests/visual/account-access.mjs
scripts/bounded-frontend.sh exec node ../tests/visual/team-access.mjs
scripts/bounded-frontend.sh exec node ../tests/visual/design-ux.mjs
```

Focused browser suites cover accounts, teams, competition workspaces, A&D,
arena rendering, URL/hash navigation, writeups/PDF, guide and community pages.
Read [tests/visual/README.md](../../../../tests/visual/README.md) and each harness's target/fixture defaults
before running it; some require a built client or preview server. For every-page
discovery/selection, reuse [tests/visual/route-catalog.mjs](../../../../tests/visual/route-catalog.mjs) and
[tests/visual/audit.mjs](../../../../tests/visual/audit.mjs). Provide dynamic fixture IDs and secret-file
credentials only when the selected harness requires them. Inspect screenshots
as well as automated Axe/overflow/runtime results.

## Load, performance and evidence

Use [tests/load/README.md](../../../../tests/load/README.md) to choose the scenario and its disposable fixture:
player/lifecycle, polled reads, scoreboard, admin/edit operations, captures,
anti-cheat, donations, repositories, storage, BYOC and trusted workers.
[tests/load/k6/](../../../../tests/load/k6/) owns k6 scenarios; Node orchestrators share
[tests/load/lib.mjs](../../../../tests/load/lib.mjs) and [tests/load/process-control.mjs](../../../../tests/load/process-control.mjs).

```sh
npm --prefix tests/load test
rg -n '^## ' tests/load/REPORT.md
rg --files tests/load | rg 'scoreboard|cache|storage|retention|worker|byoc'
```

[tests/load/REPORT.md](../../../../tests/load/REPORT.md), [tests/load/results/](../../../../tests/load/results/) and retained scenario JSON
are historical evidence, not current capacity guarantees. Compare the same source
scenario, fixed arrival rate, dataset, topology and host; retain latency
distributions, CPU/RAM, errors/drops and integrity checks. Component gains do not
establish whole-platform gains. Architecture-only benchmark programs/results
must be read on their branch until merged; do not invent commands on main.

## CI and release navigation

| Gate / artifact | Source of truth |
| --- | --- |
| Which checks run and whether skips are allowed | [scripts/ci-plan.mjs](../../../../scripts/ci-plan.mjs); unknown paths/manual/publication runs select all |
| CI jobs, service fixtures, required aggregate | [.github/workflows/ci.yml](../../../../.github/workflows/ci.yml) |
| Formatting, size, installer/proxy/convention checks | `repository-conventions` in the CI workflow, including engineering navigation links; no universal folder linter on this baseline |
| Docs/PDF verification and publication | [.github/workflows/docs-build.yml](../../../../.github/workflows/docs-build.yml), [.github/workflows/docs.yml](../../../../.github/workflows/docs.yml), [docs/package.json](../../../../docs/package.json) |
| Server and BYOC native/multiarch image build, assembly, attestation | [.github/workflows/image.yml](../../../../.github/workflows/image.yml), [Dockerfile](../../../../Dockerfile), [agents/byoc-agent/Dockerfile](../../../../agents/byoc-agent/Dockerfile), [deploy/Dockerfile.release](../../../../deploy/Dockerfile.release) |
| Coordinated version and main-ancestry checks | [.github/workflows/release-validation.yml](../../../../.github/workflows/release-validation.yml) |
| Worker native artifacts and release | [.github/workflows/worker-agent-release.yml](../../../../.github/workflows/worker-agent-release.yml) |
| Helm publication | [.github/workflows/chart.yml](../../../../.github/workflows/chart.yml), [charts/rsctf/Chart.yaml](../../../../charts/rsctf/Chart.yaml) |
| Managed KotH load / linker experiment | [.github/workflows/managed-koth-load.yml](../../../../.github/workflows/managed-koth-load.yml), [.github/workflows/linker-benchmark.yml](../../../../.github/workflows/linker-benchmark.yml) |

Merging main does not automatically publish a server image. Read the publisher's
manual/tag triggers and same-run quality/attestation requirements before release.
Version-bearing files include root/BYOC/worker Cargo manifests and their lockfiles,
plus the chart version/appVersion. Never assume a green PR is a published version
or that a published image is deployed.

For an authorized production rollout, follow the immutable-digest gate in
[verification-release.md](verification-release.md). Verify the exact `ok` health
body, every applicable replica's version/digest, changed behavior, installer
endpoints when relevant, and recent logs. Report missing access explicitly.
Prose-only skill navigation changes do not require a server release/deployment.
