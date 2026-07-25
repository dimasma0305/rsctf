# Testing and coverage

RSCTF treats coverage as a map of risk, not as proof that an event is safe. CI
combines unit tests, isolated PostgreSQL and Redis regressions, frontend tests,
route-catalog contracts, agent builds on Linux and Windows, installer checks,
and deployment validation.

## Required CI gates

| Area | Gate |
| --- | --- |
| Rust server | formatting, all-target check, Clippy with warnings denied, build, and default tests |
| Database behavior | serialized PostgreSQL and Redis regressions under coverage instrumentation |
| Rust dependencies | `cargo audit`, plus a compiled-graph guard for the lockfile-only RSA advisory |
| React client | full dependency audit, warning-free Oxlint, 75 logic tests, strict TypeScript, and production build |
| Worker plane | shared protocol tests, Linux agent tests and Docker lifecycle, Windows check/Clippy/tests/release build |
| BYOC agent | formatting, check, Clippy, and tests |
| HTTP surfaces | exact admin and edit route catalogs plus load-harness contracts |
| Packaging | POSIX/BusyBox and PowerShell installers, Compose security, Helm rendering, docs, and image topology |

The PostgreSQL fixtures use unique schemas, but migration tests still touch
database-scoped catalog state. Run the ignored database suite with one test
thread; parallel execution can make schema teardown race another migration and
create false failures.

## Coverage baseline

Measured on 2026-07-25 with `cargo-llvm-cov 0.8.7`:

| Rust component | Line coverage | Notes |
| --- | ---: | --- |
| Server production source, default suite only | 23.06% | Database-backed ignored regressions excluded |
| Server production source, default + 164 PostgreSQL/Redis regressions | 43.76% | CI floor: 40%; migrations excluded |
| Shared worker protocol | 82.77% | 20 protocol and wire-format tests |
| Trusted worker agent | 39.57% | 45 tests; Docker lifecycle also runs with a real image in Linux CI |
| BYOC agent | 55.03% | WebSocket, flag ordering, reconnect, and filesystem behavior |

The database suite raises measured production-source coverage by 20.70 percentage
points and, more importantly, exercises replica fences, deduplication,
deletion races, A&D/KotH persistence, worker placement, repository solve
preservation, anti-cheat evidence, and Redis coordination on every CI run.

The worker hardening pass raised its measured line coverage from 35.10% to
39.57%. In particular, state-directory and lock security rose from 6.96% to
85.86%, TLS identity loading from 0% to 44.79%, and data-plane status mapping
from 0% to 41.13%.

The client test runner intentionally covers pure logic without a browser DOM.
Its 75 tests are behavior counts, not a line-coverage claim. Browser rendering
and end-to-end behavior remain covered by the lifecycle harness and production
smoke checks.

## Reproduce server coverage

Use disposable PostgreSQL and Redis instances. The command below destroys only
the temporary schemas and keys created by the tests, but it must never point at
production.

```sh
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov --locked --version 0.8.7

export RSCTF_TEST_DATABASE_URL='postgresql://postgres:postgres@127.0.0.1:5432/rsctf_test'
export RSCTF_TEST_REDIS_URL='redis://127.0.0.1:6379'

cargo llvm-cov --all-targets --all-features --locked --no-report
cargo llvm-cov \
  --no-clean \
  --summary-only \
  --all-targets \
  --all-features \
  --locked \
  --ignore-filename-regex '(^|/)(target|migrations)/' \
  --fail-under-lines 40 \
  -- \
  --ignored \
  --test-threads=1 \
  --skip s3_round_trip \
  --skip target_fk_deletes_scoped_tokens \
  --skip ownership_constraints_are_validated_cascades \
  --skip database_board_is_finite_bounded_and_serializable \
  --skip database_aggregate_is_bounded_by_stable_roster \
  --skip database_epoch_rollup_is_idempotent \
  --skip database_rollup_invalidation_keeps_only_the_safe_prefix
```

The skipped S3 test requires a live disposable object store. Two migration
inspection checks require an already fully migrated installation, and the
other four checks require a pre-provisioned A&D game. They remain explicit
environment tests rather than self-contained CI fixtures.

## Event-scale validation

Contract tests are fast and deterministic:

```sh
cd tests/load
npm test
```

Use the lifecycle and fixed-rate load harnesses for changes to polled reads,
A&D/KotH scoring, containers, or tunnels. Follow the commands and current
same-load baselines in `tests/load/README.md` and `tests/load/REPORT.md`.
