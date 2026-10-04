# Workflow performance

## Baseline and scope

The successful [v0.1.136 main publication](https://github.com/dimasma0305/rsctf/actions/runs/37203108162)
on 2026-10-04 took 21m45s. Rust CI took 8m16s, coverage 7m34s,
and Kubernetes added 4m06s after Rust. A frontend-only
[PR run](https://github.com/dimasma0305/rsctf/actions/runs/37205246021)
still ran Rust for 9m12s followed by Kubernetes for 4m10s, although its
frontend checks took 1m14s.

BuildKit hit the server's cargo-chef dependency cache. ARM64's own release
compilation still took 887 seconds. The changes below do not claim to remove
that cost: production retains opt-level 3, fat LTO and one codegen unit.
Native AMD64/ARM64 builds, SBOMs, provenance verification, exact-commit image
promotion, and immutable release publication remain mandatory.

## Work removed

- PRs touching only `web/` or `docs/` run their component checks plus repository
  conventions and harness contracts. Agent/protocol changes retain the server
  integration gates. Unknown/shared paths and incomplete diffs run everything.
  Diffing uses Git history, not the API's capped list of changed files.
- Every manual or reusable publication run executes the full suite. The final
  `Required CI checks` job runs even after failures and rejects missing,
  cancelled, failed, or unexpectedly skipped jobs. It is suitable as a stable
  required branch check; this change does not alter repository protection rules.
- Clippy already compiles the same target/feature sets, so redundant `cargo check`
  passes are removed. Builds, tests, formatting, coverage and warnings-as-errors
  remain enforced.
- Rust CI uploads its application and library-test executables once. Kubernetes
  and isolated anti-cheat jobs consume only that same-run, same-SHA artifact,
  eliminating two additional server compilations. Artifacts expire after one day.
- Documentation uses one reusable build for CI and Pages. PRs no longer trigger a
  second standalone documentation workflow. Pages deployment stays main-only.

## Cache boundaries

Rust dependency/tool caches are warmed on main and restored by PRs. This avoids
large redundant copies under each PR merge ref. Source binaries are transferred
as same-run artifacts, never accepted as cached publication evidence.

Linux AMD64 and Windows worker release builds use the same workspace/key/flags as
main CI. Tag jobs restore but do not save caches that the next tag cannot access.
ARM64 worker builds remain native and cold until a main job exists to warm that
architecture; there is deliberately no cross-architecture cache fallback.

`cargo-audit` now uses a pinned tool cache; its advisory check still runs on every
selected security job. Coverage retains its separate instrumented cache and
pinned `cargo-llvm-cov`. Frontend installs retain frozen lockfiles, age/trust
policy and pnpm's lockfile-keyed store cache.

These boundaries follow [GitHub cache scope rules](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching)
and [rust-cache's key and save controls](https://github.com/Swatinem/rust-cache).
Do not add secrets to caches or reuse PR artifacts in publication workflows.

## Verification

Run `node --test tests/load/test/ci-plan.test.mjs tests/load/test/workflow-performance.test.mjs`
and `actionlint`. Planner tests include more than 300 changed files, moves out of
shared paths, unusual filenames, unavailable history, and all aggregate failure
states. Exercise `scripts/test-kubernetes-koth-callback.sh` with
`RSCTF_K8S_TEST_BINARY` set to the just-built test executable, then run full CI.

Compare actual Actions runs with matching scope and cache state before claiming
a speedup. A dependency update that changes compiler inputs is not a controlled
before/after benchmark; retain queue time separately from job execution time.

### First verified runs

The [full optimization PR gate](https://github.com/dimasma0305/rsctf/actions/runs/37206899144)
passed in 9m28s wall time (including orchestration). Its same-run artifact consumers
passed the real Kubernetes and anti-cheat tests in 1m11s and 1m12s respectively,
versus 4m10s and 3m55s in the earlier frontend PR gate. This is observed Actions
timing, not a controlled host-performance benchmark.

The subsequent [frontend-only SHA-256 PR gate](https://github.com/dimasma0305/rsctf/actions/runs/37207679763)
passed in 1m45s wall time. React checks took 1m26s; planner, harness contracts,
repository conventions and the aggregate gate passed, while unrelated jobs were
explicitly skipped by the tested plan. Publication still requests the full suite.

Local validation passed Actionlint, 11 focused workflow tests and 482 harness
contracts (one environment-only skip). The stripped test artifact emitted the
actual policy locally. Kind bootstrap on the shared host was blocked before the
application tests by an exhausted fsnotify/inotify resource limit in containerd;
the owned clusters were removed without changing host limits. The clean GitHub
runner's real Kubernetes gate subsequently passed.
