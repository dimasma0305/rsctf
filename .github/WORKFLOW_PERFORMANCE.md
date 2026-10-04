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

The [v0.1.137 worker publication](https://github.com/dimasma0305/rsctf/actions/runs/37209627663)
confirms that an extra ARM64 cache-warming job is not on this release's critical
path: native ARM64 completed in 1m34s, AMD64 in 53s and Windows in 2m10s. The
publisher then spent 12m06s waiting for the exact tagged image and its trusted
attestation, while the already-running main image build finished. This wait is
not repeated worker compilation; do not remove source/digest verification to
shorten it. After image resolution, seven serial artifact-attestation checks
took 22s. Bounded parallel verification was then measured separately with the
same subjects, signed bundle and trust constraints, as recorded below.

Four local full-subject trials, ordered serial, four workers, four workers,
serial, verified all seven actual v0.1.137 release artifacts. Serial took
29.12s/32.40s and parallel took 8.42s/9.06s: **71.6% lower mean step wall time**
(30.76s to 8.74s). Both conditions used GitHub CLI 2.102.0, identical artifact
and bundle hashes, a warmed trust-metadata cache, a two-CPU/1 GiB cgroup and a
120-second per-step limit. The independent altered-artifact and wrong-tag controls
both rejected the parallel step. Unit regressions execute the real workflow
shell and require all seven subjects, every original trust constraint, bounded
parallelism, failure propagation and complete process settlement.

Before publication, three fresh trials per condition compared four with seven
verifiers in order 4, 7, 7, 4, 4, 7. With the same subjects, bundle, CLI, warmed
metadata and resource caps, four took 7.83/8.96/8.26s and seven took
4.95/5.38/5.74s. Mean step time decreased **35.8%**, from 8.35s to 5.36s,
exceeding the predeclared 10% acceptance threshold. Both real negative controls
again rejected the step. This separate comparison does not combine percentages
from the earlier serial/four-worker trials. Its scripts, exact inputs, six
trials and controls are retained in `attestation-width.json` beside the first
experiment; both owned temporary verification fixtures were removed.

Publication now runs at most seven independent verifiers using `xargs`, one per
subject; regression tests enforce this bound and child settlement. It still
fails if any subject is missing or invalid. This reduces only the verification
step, not compiler time or image availability. Network latency affects these
local timings, and the next tagged Actions publication must verify its actual
end-to-end result. Evidence is retained in
`visual-audit-output/workflow-compiler-experiment/attestation-verification.json`.

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

## Second pass: remove serial critical-path work

The [v0.1.137 publication](https://github.com/dimasma0305/rsctf/actions/runs/37208869415)
took 26m43s for CI and images. This dependency-update run is not a controlled
comparison with v0.1.136. Its Rust job spent 6m05s in Clippy before starting
the 7m33s application build and 2m53s Docker/test compilation stage. The
server's native jobs started about three minutes after the companion jobs started
because of an unnecessary job dependency. ARM64 spent 3m01s cooking changed
dependencies and 15m08s compiling the application.

The next candidate removes those serial dependencies without changing production
compiler settings:

- Formatting/Clippy run beside compilation, with a separate metadata cache so
  concurrent jobs cannot overwrite one another's cache contents. Both jobs remain
  mandatory in the fail-closed aggregate. Build prepares application binaries and
  then all test targets before running the same test suites.
- Native server and companion builds start together. The intermediate server is
  tagged `build-<sha>`, never accepted as the public `sha-<sha>` release candidate.
- After both builds and full CI succeed, `deploy/Dockerfile.release` attaches the
  immutable companion reference using metadata only. No filesystem instruction,
  target executable, emulation, Cargo build, or frontend build runs in assembly.
- Assembly regenerates SBOM/provenance for both platforms. Verification compares
  base/assembled filesystem layers and unrelated runtime config, checks both
  revisions/versions and companion metadata, then attests and promotes only the
  assembled digest. Tag publication still requires that exact main attestation.

Parallelism does not make the 15-minute Rust release compilation disappear, and
a new lint cache is cold on its first run. Distinguish cache-hit image builds
from runs whose application source actually changed.

### Observed full CI result

The [second optimization PR gate](https://github.com/dimasma0305/rsctf/actions/runs/37220491931)
passed in **9m17s** from workflow creation to completion on 2026-10-04. All 16
selected checks passed. Rust compilation/tests took 6m23s and the parallel lint
job took 6m27s (Clippy itself 5m57s with a cold lint cache). Coverage/database
checks took 7m49s, including all 431 selected database regressions and 57.35%
line coverage, above the unchanged 40% floor. Kubernetes and isolated anti-cheat
checks passed in 1m53s and 1m19s after the Rust artifacts were available.

This is observed CI latency, not a controlled percentage improvement over the
dependency-update run. It does not include image compilation, tag publication,
or production deployment. PR #167 was merged as `4ac18dea`; the first main image
validation, [run 37221124219](https://github.com/dimasma0305/rsctf/actions/runs/37221124219),
passed in **9m43s**, including full CI, both native images, assembly, verification
and main-image attestation. Metadata assembly itself took 23 seconds. That run
reused the unchanged v0.1.137 application compilation layer, so its duration must
not be presented as the cost of compiling changed Rust. Its verified main digest
is `sha256:517f532b0b4cc4b8c1064ef4cc95d258e8274baa67a901f59564615156f9daf5`;
this is not a new tagged release or a production deployment.

### Local findings, not an Actions speedup claim

Combining the initial application and test-target compilations was rejected:
the two large `rustc` processes exceeded the local 12 GiB cgroup limit. The
application-first order reuses dependency parallelism while avoiding simultaneous
code generation of the full library and its test harness. Do not raise shared-host
limits or disable tests merely to improve a workflow timing.

Metadata-only assembly with pinned BuildKit 0.32.2 and Syft scanner 1.12.0 passed
an unpublished local AMD64/ARM64 OCI export. Both platforms retained all nine
filesystem layers and the full original runtime config, including Docker's
healthcheck extension, while generating 155-package SBOMs and SLSA provenance.
The exact workflow verification shell also passed against the existing immutable
release. GitHub execution subsequently passed as recorded above; deployment of
a new tagged release remains required.

## Third pass: bounded test concurrency and lightweight orchestration

The frontend test runner now honors the existing `RSCTF_FRONTEND_WORKERS` limit
(integer 1 through 4). Its local default remains two; CI requests four on its
standard four-core Linux runner. Invalid settings fail before compilation, and
test discovery, per-file process isolation, and failure propagation are unchanged.

Four matched local full-suite trials, ordered **2, 4, 4, 2** workers, all passed
722 application tests and 35 visual-harness contracts, with no failures, skipped
tests or cancellations. Two-worker trials took 54.82s and 52.77s; four-worker
trials took 31.58s and 30.87s: approximately **42% lower mean local wall time**.
Both conditions used the same source and installed dependency hashes, Node
24.8.0, pnpm 11.22.0, an isolated 400% CPU quota and 8 GiB memory cap, through
`scripts/bounded-frontend.sh`. This comparison includes test bundling and pnpm
startup, but not dependency installation. No other local compilation overlapped.
The normal local CPU quota is still 150%; the 400% setting was only the matched
CI-shaped experiment. These local timings are not a claim about CI's pinned
Node 22/pnpm 11.8.0; its clean-room gate remains required.

To repeat each condition on an otherwise idle compile slot:

```sh
RSCTF_FRONTEND_CPU_QUOTA=400% RSCTF_FRONTEND_WORKERS=2 scripts/bounded-frontend.sh test
RSCTF_FRONTEND_CPU_QUOTA=400% RSCTF_FRONTEND_WORKERS=4 scripts/bounded-frontend.sh test
```

Only five short, unprivileged Git/Node/API bookkeeping jobs move to `ubuntu-slim`:
CI planning and aggregation, image preparation, shared release-source validation,
and the fresh-release check. Each retains its five-minute timeout and existing
fail-closed policy. Compilers, service containers, Docker builds, integration
tests and signing/promotion jobs remain on their existing full runners. The
[runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
documents the single-core container runner's limits, and its
[image manifest](https://github.com/actions/runner-images/blob/main/images/ubuntu-slim/ubuntu-slim-Readme.md)
includes the required Git, Node, shell and GitHub CLI tools. Local planner and
aggregate tests also passed under a one-CPU, 1 GiB cgroup. Actual Actions startup
latency must be observed; shorter queue time is not guaranteed.

The [third optimization PR gate](https://github.com/dimasma0305/rsctf/actions/runs/37222365230)
passed all 16 selected checks in **9m01s** on 2026-10-04; PR #168 merged as
`54c45030`. React checks took 56s, including a 23s test step and 14s build.
Coverage/database checks remained the critical path at 7m57s, including all
431 selected database regressions. Rust compilation/tests took 6m14s; its
parallel lint job took 2m11s after the separate main lint cache had been warmed.
The slim planner/aggregate jobs took 8s/7s. These are actual Actions observations
on different runners, not a controlled percentage comparison. No checks were
removed, and this PR run does not measure image compilation or deployment.

## Release compiler experiment: faster candidates rejected

Four matched cold-target builds used the same v0.1.137 source, release builder
digest, Rust 1.97.1, two-core quota and 12 GiB limit. All completed without warnings:

| Release profile | Wall time | Sampled peak memory | Server binary bytes |
| --- | ---: | ---: | ---: |
| Fat LTO, one codegen unit (current) | 20m06s | 7,229 MiB | 60,631,568 |
| ThinLTO, one codegen unit | 18m50s | 7,057 MiB | 63,483,536 |
| ThinLTO, sixteen codegen units | 16m41s | 6,634 MiB | 85,115,984 |
| ThinLTO, sixteen units only for `rsctf` | 15m40s | 6,462 MiB | 105,268,776 |

ThinLTO alone improved wall time by only 6.3%, below the experiment's predeclared
10% threshold, so it was rejected. The partitioned candidate improved this first
cold trial by 17.0%, but its binary is larger and runtime effects are unverified.
Neither compiler candidate is applied to the production profile. Warm-source
confirmation and three paired fixed-arrival-rate runtime trials are required
before accepting one; all existing test/security/release gates remain mandatory.

Warm-source confirmation then passed in reversed order: ThinLTO/sixteen units
took **11m28s**, followed by fat/one unit at **14m43s**, a **22.1%** reduction.
Only the root package outputs were invalidated in each task-owned target;
dependencies were retained. Both rebuilt the real application with zero warnings
and reproduced both cold-build binary hashes exactly. This passes the compile
threshold, not the runtime acceptance gate.

The actual pinned release builder uses Rust 1.97.1, while host/CI use 1.98.0.
Both sides of this experiment use the former; comparing builds across those
versions would confound the result. Cargo timings identified the mechanism:
partitioning reduced the root library stage from 501s to 280s, while the final
server link/code-generation stage increased from 379s to 405s. This is a local
compiler experiment, not a prediction that a GitHub publication now takes 16m41s.
Those contrasting stages justify one additional scoped candidate: partition
only package `rsctf`, retaining one codegen unit for dependencies. It is a
predeclared experiment, not an applied profile. That cold build completed in
940.37s, **22.1%** below the fat-LTO control, with zero warnings. It grew the
binary further instead of reducing it. Its warm-source repeat then completed in
**10m15s**, **30.3%** below the matched fat-LTO repeat, with zero warnings and
both cold-build binary hashes reproduced exactly. It is faster than global
ThinLTO/16 under both measured compile conditions, but strict runtime acceptance
remains necessary. No production compiler profile has changed.

The root-only partition candidate was subsequently **rejected** by its three
paired runtime trials. All six fixed-rate runs passed functional, health and
fixture-integrity checks, but the candidate increased mean application CPU by
**5.91%**, sampled peak memory by **16.68%**, and the mean KotH timeline p95 by
**5.47%**. The predeclared maximum regressions are 5% for CPU and every endpoint
p95, and 10% for peak memory. Faster compilation does not override those gates.
The previously compiled global ThinLTO/16 fallback then completed three fresh
alternating baseline/candidate pairs. All six runs passed their functional,
health and integrity checks, but mean CPU increased **7.48%**, Jeopardy scoreboard
p95 **6.33%**, and KotH timeline p95 **5.53%**. Memory improved by 9.14%, which
does not cancel the failed CPU/latency gates. This fallback is also **rejected**;
production retains opt-level 3, fat LTO and one codegen unit. Rejected measurements
are retained, not relabeled or reused as successful results. Full conditions and
the load-harness corrections are in
[the load report](../tests/load/REPORT.md#release-compiler-runtime-acceptance--4-october-2026).

### Additional cache and database checks

An exact-version `cargo-chef prepare` inspection confirmed that the root package
version is already normalized to `0.0.1` in both the generated manifest and lock.
Do not add another version-rewriting layer: application version bumps alone do
not change those dependency-recipe fields.

That normalization deliberately stops at dependency preparation. The application
build embeds its package version and a fingerprint over `Cargo.toml`, the lockfile,
`build.rs` and `src/`; split-role topology compatibility uses that exact fingerprint.
Reusing a prior-version application binary would change the existing version and
replica-identity contract, not merely improve a cache key. This pass preserves that
contract and only promotes verified same-commit application images.

The GitHub cache inventory at 18:37 UTC contained 150 entries totaling about
10 GiB: 5.95 GiB Rust caches, 3.73 GiB BuildKit data and 0.32 GiB other data.
About 2.7 GiB consisted of older Rust dependency-key variants, while current
main caches were successfully reused by PR #168. Being near the cache quota is
not, by itself, evidence of active-cache thrashing. No cache entries or repository
storage limits were changed during this audit.

The pinned [rust-cache implementation](https://github.com/Swatinem/rust-cache/tree/f0d9c3887740aee45f6153b24b3a6b815192ec16)
already excludes incremental/workspace outputs by default and restores compatible
dependency caches across lockfile changes. Whole-workspace caching was not enabled
without a demonstrated benefit: it would add large project outputs and does not
eliminate compilation when application sources or release versions change.

The PR #168 Rust timeline also bounds the benefit of splitting out yet another
test job: Docker retry and normal test execution took only 5s and 9s, while
same-run artifact staging/upload took 4s and 6s. A new downstream test job would
add its own startup, checkout and artifact download. No measured net gain justifies
that extra split; the expensive 2m12s/2m45s application/test-target compilation
remains separate from Clippy and its tested binaries are already shared.

Four isolated PostgreSQL storage trials, ordered disk, tmpfs, tmpfs, disk,
each passed the exact same 431 database tests without failures or skips. Disk
trials took 212.96s/212.18s; a 2 GiB tmpfs took 206.44s/204.72s. The mean
improvement was only **3.3%**, below the predeclared 10% threshold, so this
candidate was rejected and CI retains its existing storage configuration.
All trials used PostgreSQL 18.6, identical test-binary and selection hashes,
equal resource caps, and `fsync`, `full_page_writes` and `synchronous_commit`
enabled. Setup took about two seconds per trial and was recorded separately.
All exact disposable service containers and disk volumes were removed after
the trials; development and production databases were not part of this test.

### Isolated database partitions: full local instrumentation proof passed

Four further trials used the exact same executable and 431-case selection,
ordered serial, two partitions, two partitions, serial. Serial tests took
211.65s/213.62s; two serial partitions took 113.28s/114.63s: **46.4% lower
mean test wall time**. Including service setup, sampling completion and cleanup,
mean elapsed time fell from 217.45s to 120.85s (**44.4%**). These are matched
local measurements, not a claim that the current GitHub workflow is faster yet.

The partitions contain 216 and 215 exact test names. Each has its own disk-backed
PostgreSQL and Redis instance; aggregate CPU/RAM caps remain equal to serial.
Separate Redis database numbers would not suffice because one existing test
intentionally disconnects Pub/Sub clients server-wide. All four runs passed
every expected case, with zero failed/skipped/duplicate cases or sampling errors,
enabled PostgreSQL durability, and verified cleanup of every owned container
and volume. Development and production services were untouched.

The instrumented path proved that default-suite coverage and both partitions'
profiles are retained and merged before CI was changed. The all-target selection,
ten existing environment exclusions and 40% line-coverage floor stay unchanged.
The bounded local Cargo wrapper now honors an explicit `RUSTC_WRAPPER` (including
an empty opt-out) instead of replacing a coverage wrapper with sccache. Its
regression reproduces the previous conflict even on hosts without sccache;
default caching and the shared CPU/RAM/build-lock limits remain intact.

The isolated LLVM 22.1.8/Rust 1.98.0 known-case proof passed: serial and partitioned
execution both covered all six functions and eighteen lines, with identical
summaries and unchanged default-profile hashes. Removing one partition's profile
made the 100% test gate fail; restoring it made the gate pass. This validates the
coverage toolchain/merge mechanism, independently of the full RSCTF suite.

The full all-feature/all-target proof then passed **1,809 default tests and 431
database tests**. Serial and partitioned execution produced identical coverage:
104,650/182,461 lines (57.3547%), 8,221/16,615 functions and identical per-file
counts. All eight executable targets were discovered, including zero-ignored-case
targets. The 61 existing raw profiles retained their exact hashes, and each of the
two partitions supplied its own new profile. Serial-only profiles were moved
outside the report target before evaluating the candidate, so they could not
mask missing coverage. The instrumented database stage took 121.42s versus
212.17s serial; this single functional confirmation is separate from the four
matched uninstrumented timing trials above.

The promoted `scripts/coverage/` implementation was then exercised again with
fresh services. Discovery through the exact `show-env --export-prefix`/Bash
transport reused the instrumented executables in 0.86s without recompiling the
application. All 431 cases passed in 120.18s, and every per-file coverage summary
and all totals exactly matched the serial reference. Both service pairs and
their disk volumes were removed. The new process/partition regressions cover
missing/duplicate tests, incomplete Cargo output, missing/changed profiles,
shared services, timeouts, output limits, cancellation and descendant cleanup.

CI now keeps the original concurrent default coverage build, then discovers the
same all-target/all-feature/locked executables and runs two serial partitions
against separate PostgreSQL and Redis servers. The final report uses the
unchanged exclusions and 40% floor. The pinned tool's `report` subcommand rejects
build-selection flags such as `--all-features`; those remain on compilation, not
reporting. A first local report-only invocation exposed this CLI distinction;
the successful tests/profiles were retained and the corrected report verified
them without rerunning compilation.

The [fourth optimization PR gate](https://github.com/dimasma0305/rsctf/actions/runs/37233050429)
passed all 16 selected checks in **7m39s** on 2026-10-04; PR #169 merged as
`06dd6c75`. Coverage/database checks took **6m41s**, versus 7m57s in the previous
PR run. All 431 selected database cases passed, default coverage profiles were
retained, both partitions contributed new profiles, and line coverage remained
57.35%, above the unchanged 40% floor. Rust compilation/tests took 5m45s,
parallel lint 2m03s, and React 1m25s.
The same-artifact Kubernetes and isolated anti-cheat consumers took 1m23s and
1m22s. These are observed Actions durations on different runners/cache states,
not a controlled percentage speedup or a measurement of image publication and
production deployment.
