# Performance and load verification

## Hot-path design

Scoreboards, A&D/KotH state/targets/boards/timelines, and other client-polled reads are
hot paths.

- Never put one blocking `std::sync::Mutex<HashMap>` on every request and never hold a
  synchronous guard across `.await`. Shard by stable key, use a read-heavy `RwLock`,
  or use a lock-free design.
- Cache serialized bodies as `bytes::Bytes` and serve hits verbatim. Do not
  deserialize/re-serialize cached JSON.
- Use `TieredCache` (L1 plus Redis L2) and `SingleFlight` for heavily polled reads,
  normally around a five-second TTL. Key every response dimension (including monitor
  or freeze visibility) and invalidate on the mutation that changes it.
- Keep Redis bounded (`256mb`, `allkeys-lru`) and cap individual cached values. Asset
  blobs are capped at 512 KiB; the working set must not grow without limit.
- Put Argon2, hashing, compression, and other CPU-heavy work on `spawn_blocking`.
- Avoid per-request string formatting/allocation where a borrowed or structured key
  works. Do aggregate/filter work in indexed SQL, not in a Rust loop over growing rows.

## Load harness layout

- k6 HTTP scenarios live in `tests/load/k6/*.js`.
- Node orchestrators live in `tests/load/*.mjs` and share `lib.mjs` and, for tunnels,
  `byoc-agents.mjs`. Do not add shell orchestrators.
- Unit/regression tests live in `tests/load/test/*.test.mjs`; subprocess fixtures live
  in `tests/load/test/fixtures/`.
- A new scenario needs a k6 script, thin Node runner, npm script, README baseline,
  `server_5xx ~0`, responsive `healthz`, and post-run duplicate/integrity checks.

Common runs from `tests/load/`:

```sh
npm run player
N=60 npm run byoc
N=120 npm run worst-case
```

Use `node lifecycle.mjs` through `provision.mjs` for the complete event lifecycle:
BYOC tunnels, Jeopardy create/destroy, attachment upload/download, and KotH capture and
cycle-capability rejection.

## Measurement rules

- Run release builds and compare at a fixed `constant-arrival-rate`, not peak req/s.
- A performance claim needs before and after from the same harness, host, fixed rate,
  data size, and scenario. Compare CPU percentage at held throughput or endpoint p95.
- Export full avg/p50/p90/p95/p99/max distributions with `SUMMARY_JSON`, sample RSCTF
  and PostgreSQL CPU/RAM, and update the repository performance report and optimization
  ledger whenever numbers change.
- Current reference workload: 500 teams, 400 VUs plus 80 tunnels on eight cores,
  approximately 1.4k req/s, around 2.4 cores, 284 MiB peak, and zero 5xx/integrity
  errors. Host noise makes peak-throughput comparisons invalid.
- The Jeopardy submit tail is write-bound. Do not add more read caching as a claimed
  fix; defer non-critical on-solve event/notice writes only with durability and ordering
  tests.
