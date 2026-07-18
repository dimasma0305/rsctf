# rsctf - repository conventions

rsctf is a Rust CTF platform with a React client in `web/`. Backend and frontend
changes are both supported; place a change in the layer that owns the behavior while
keeping the public API stable.

## High-level design principles (language-agnostic)

Apply these standards across the Rust backend, React client, load harnesses, and
supporting tooling so the architecture remains clean, maintainable, and scalable:

- **SOLID:** Keep responsibilities cohesive, interfaces focused, dependencies
  explicit, and implementations replaceable where useful. Apply the underlying
  design principles without forcing object-oriented abstractions where the language
  or problem does not need them.
- **DRY (Don't Repeat Yourself):** Do not duplicate business rules or substantial
  logic across modules. Extract genuinely shared behavior into the narrowest
  appropriate utility or domain module, while avoiding premature generalization.
- **KISS (Keep It Simple, Stupid):** Prefer the smallest clear design that meets the
  requirement. Do not introduce deep abstraction layers, complex folder trees, or
  indirect control flow without a demonstrated need.
- **YAGNI (You Aren't Gonna Need It):** Build only what the current requirement
  needs. Do not add speculative features, extension points, configuration, or
  directories for possible future work.

## Specialized safety and security standards

When work involves safety-critical software or components where failure could cause
physical harm, apply the relevant formal standards in addition to this repository's
normal conventions:

- **MISRA (C/C++):** Use the applicable MISRA rules for safety-critical C or C++
  components, especially automotive, aerospace, and medical-device code. Do not
  claim MISRA compliance without the required analysis, documented deviations, and
  verification evidence.
- **SEI CERT:** Follow the applicable language-specific SEI CERT secure-coding rules
  to prevent vulnerabilities such as memory corruption, injection, integer errors,
  and unsafe resource handling. Prefer these rules whenever they impose stronger
  security requirements than general style guidance.

## File-size rule (enforced)

**No `.rs` file exceeds ~1000 lines.** When a module grows past that, break it into a
folder module — `foo.rs` becomes `foo/mod.rs` plus focused sibling files — split by
cohesive responsibility, not arbitrarily. `mod.rs` keeps the public surface: the
`router()` (for controllers), shared types, and `pub use` re-exports so external call
sites stay `crate::controllers::foo::Thing`.

Check before committing:

```sh
find src -name '*.rs' | xargs wc -l | awk '$1>1000 && $2!="total"'
```

## Controller organization

Each controller area is a **folder module** under `controllers/`, with A&D / KotH
sub-controllers nested under their area (not flat siblings):

```
controllers/
  game/            # GameController + the play-facing A&D/KotH controllers
    mod.rs         #   router() merging the submodules + shared context helpers
    play.rs        #   join/leave/challenges/submit
    scoreboard.rs  #   scoreboard + notices + events
    containers.rs  #   per-instance container create/destroy
    traffic.rs     #   packet-capture serving
    writeup.rs
    ad/            #   AdGameController (player A&D: token/ssh/targets/timeline/submit)
    koth/          #   KothController (player KotH board)
  admin/           # AdminController + AdAdminController
    mod.rs
    config.rs · users.rs · teams.rs · logs.rs · repo_bindings.rs
    builds.rs · anti_cheat.rs · diagnostics.rs
    ad.rs          #   AdAdminController (round advance, service registration)
  edit/            # EditController
    mod.rs
    games.rs · challenges/ · flags.rs · attachments.rs · builds.rs · ad/
```

`mod.rs`'s `router()` merges the submodule routers; `server.rs` merges only the
top-level area routers (`controllers::game::router()`, …).

## Services / models with folder modules

Large services split the same way: `services/ad/engine/` (reducers, checker, rounds),
`services/suspicion/` (detectors, correlation). `models/data/` groups entities by
domain (`ad.rs`, `koth.rs`, `games.rs`, …); `pub use <mod>::*` in `models/data/mod.rs`
keeps entity paths flat.

### A&D domain layout (enforced)

Do not add top-level files or folders named `ad_*`. Attack-Defense modules belong
under a cohesive `ad/` domain folder, for example `services/ad/vpn/firewall.rs` or
`controllers/game/ad/targets.rs`. The service engine, VPN, and SSH implementations
physically live at `services/ad/{engine,vpn,ssh.rs}`; only compatibility aliases such
as `services::ad_engine` belong in `services/mod.rs`. Keep those re-exports when
moving legacy code so callers do not need a repository-wide path rewrite.

## Migrations

Schema changes go in a **new** `migrations/mXXXX_*.rs` (idempotent: `if_not_exists` /
`add_column_if_not_exists`) and are registered in `migrations/mod.rs` — never edit an
already-shipped migration, so existing deployments upgrade cleanly on startup.

## Wire-format invariants (must hold for the React client)

- **Enums are strings** on the wire (`"Admin"`, `"Misc"`), except `ReviewRating` /
  `GamePermission` (numeric). See `utils/enums.rs`.
- **Timestamps are Unix-millisecond numbers** — apply `#[serde(with = "crate::utils::datetime::millis")]` / `millis_opt`.
- Success responses are the **raw model**; only endpoints whose API contract requires
  it use the `RequestResponse`/`ArrayResponse` envelopes.
- DTO fields are `#[serde(rename_all = "camelCase")]`.

## Performance (keep the hot paths fast)

The polled read paths (scoreboard / A&D / KotH boards + timelines — every client hits
them every few seconds) carry the load. Keep the established optimization patterns and
do not regress them.

- **No blocking lock on a per-request path.** A single `std::sync::Mutex<HashMap>`
  touched on every request is a *futex convoy* under async load — tokio workers park on
  it, throughput *inverts* with concurrency, CPU sits idle (that idle-CPU-under-load is
  the tell). Shard it (`middlewares/rate_limiter.rs` is 256 independently-locked shards
  keyed by `hash(policy, key)`), use `RwLock` for a read-heavy map (the L1 cache — reads
  take a shared lock, eviction a write lock), or go lock-free. Never hold a `std::sync`
  guard across `.await`.
- **Serve cached bodies as `bytes::Bytes`, zero-copy.** The `Cache` trait stores/returns
  `Bytes`; a hit is a refcount bump, not a copy. Success bodies are the raw model (see
  wire-format), so cached JSON bytes are byte-identical to a fresh serialize — ship them
  verbatim via `([(header::CONTENT_TYPE, "application/json")], bytes)`. Don't
  deserialize→re-serialize on a hit: `build_scoreboard_json` serves bytes;
  `build_scoreboard_cached` (returns the model) is only for callers that project it.
- **Two-tier cache + single-flight for polled reads.** `services/cache.rs::TieredCache`
  (in-process L1 over Redis L2) kills the network hop on hot reads;
  `utils::single_flight::SingleFlight` coalesces concurrent recomputes so a TTL expiry
  doesn't dogpile the DB (stampede). A new heavily-polled read → cache it (≈5 s TTL, key
  on `is_monitor` so the freeze cutoff can't leak), and `cache.remove` it on the mutation
  that changes it (mutations read fresh; only the read path uses the cached variant).
  **Redis (L2) must stay bounded** — it runs `maxmemory 256mb` + `allkeys-lru` (compose),
  so caching larger values (e.g. asset blobs, `assetblob:{hash}`, capped at 512 KiB each)
  can't grow it until OOM + `noeviction` write-failures. Cap what you cache; the LRU is the
  backstop. Verified under load: ~1–6 MB working set, 0 evictions.
- **CPU-heavy work goes on `spawn_blocking`.** Argon2 (`hash_password_async` /
  `verify_password_async`) and any hashing/compression — run inline they starve the async
  runtime under a login/submit flood.
- **No needless per-request allocation on the hot path.** No `format!` where a cheaper
  key works — the limiter keys by `(Policy, ip)`, not a formatted string, because `Policy`
  is already in the map key. Prefer `&[u8]` / `Bytes` / borrows over owned `String`s.
- **Push heavy/aggregate queries into SQL; index hot columns.** Use the raw-SQL escape
  hatch (`AppState::pg()` → the `PgPool` sea-orm owns) where the ORM over-fetches —
  count / aggregate / `DISTINCT ON` in Postgres, don't `.all()` a growing table into Rust
  and reduce there. Every hot filter/sort column gets an index in a new migration
  (`create_table_from_entity` only makes PK/unique constraints).

## Data access — raw SQL, minimize the ORM (enforced)

**Write new query code as raw `sqlx`, not sea-orm.** sea-orm is being phased out (the
raw-sqlx migration — see the `seaorm-removal-plan` memory), so every new
read/insert/update/upsert should go through the `PgPool` directly:

```rust
sqlx::query_as::<_, (i32, String)>(r#"SELECT id, name FROM "Teams" WHERE game_id = $1"#)
    .bind(game_id)
    .fetch_all(st.pg())            // or db.get_postgres_connection_pool() off a &DatabaseConnection
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;
```

- Prefer raw SQL over `Entity::find/insert/update/delete` + `ActiveModel` — reach for
  the ORM only when a raw query would be materially more error-prone, and note why.
- **Upserts are `INSERT … ON CONFLICT (cols) DO UPDATE SET … = EXCLUDED.…`**, never a
  read-check-then-insert (that races — it created duplicate rounds/scores; see m0025).
  Every upsert target needs the matching **unique index** (new migration).
- Tables are quoted PascalCase (`"AdRounds"`), columns snake_case; bind params `$1..`,
  never string-interpolate values. Detect a unique-violation via the SQLSTATE:
  `matches!(&e, sqlx::Error::Database(d) if d.code().as_deref() == Some("23505"))`.

## Writing-skill routing

- For an explicit request to audit AI tells, reduce AI-writing patterns, or make prose
  sound less machine-generated, use `.agents/skills/avoid-ai-writing/SKILL.md`.
- Keep `.agents/skills/humanizer/SKILL.md` for an explicit `$humanizer` request or a
  natural-voice rewrite that is not framed as an AI-pattern audit. Do not run both on
  the same text unless the user explicitly asks for both passes.
- For academic material, apply the academic-writing workflow first and use
  `avoid-ai-writing` only as a final, minimal prose pass. Preserve citations,
  quotations, equations, technical terms, justified hedging, required headings,
  and evidentiary qualifications. Never add errors to imitate human writing.
- Treat detector-facing edits as writing-quality improvements, never as a guarantee
  that a detector will classify the text as human-authored.

## Build / verify

`cargo build` must be **0 errors, 0 warnings**; `cargo test` green. Prefer verifying
behavior end-to-end (a local binary against the `rsctf-pg` Postgres, or the compose
stack) over trusting compile+tests.

**Benchmark CPU at a fixed rate, not peak req/s.** When cores sit idle at your target
rate you are *not* CPU-bound there — peak req/s is host-noise (it swings thousands of
req/s between passes). Hold the rate fixed (k6 `constant-arrival-rate`, sub-saturation)
and compare CPU-% before vs after; lower CPU at the same throughput *is* the win. And
don't switch web frameworks for speed — axum ≈ actix here (benchmarked, within ~3%); the
wins are in the patterns above. Always run `--release`.

### Load / stress tests live in `tests/load/` and are all JavaScript

A change to a polled read path, the A&D/KotH engine, or the BYOC tunnel should be
load-tested there. The layout is fixed — keep to it:

- **k6 scripts (`tests/load/k6/*.js`) generate the HTTP load**; **Node orchestrators
  (`tests/load/*.mjs`) set up state and run them.** No shell scripts — orchestration
  (spinning up BYOC agent containers, restarting rsctf, DB seeding/discovery) goes in
  Node via `child_process`, sharing `lib.mjs` (config, `sql()`/`docker()` shells, JWT +
  BYOC-token minting, `discover()`, `runK6()`) and `byoc-agents.mjs` (the tunnel fleet).
- Keep Node unit and regression tests in `tests/load/test/*.test.mjs`; test-only
  subprocess helpers belong in `tests/load/test/fixtures/`. Runtime orchestrators and
  support modules stay at the load-harness root so scenario imports remain direct.
- Run via npm from `tests/load/`: `npm run player` (A&D+KotH poll/submit),
  `N=60 npm run byoc` (BYOC scale + flood), `N=120 npm run worst-case` (reconnect storm).
  Every knob is env-overridable (`TARGET GAME CID VUS DURATION N`).
- A new scenario = a k6 script in `k6/` + a thin `*.mjs` runner + an `npm` script + a
  README baseline. Assert on `server_5xx ~0` + healthz responsiveness, and re-check
  duplicate rounds / KotH rows after a run (the race classes m0025/m0026 guard).
- Findings so far (see `tests/load/README.md`): idle BYOC tunnels are cheap/sub-linear;
  a mass reconnect storm is absorbed (rsctf stays responsive); the costs that scale with
  BYOC instance count are the connect storm + the per-tick checker probe. Not a DoS.

### Whole-platform lifecycle harness + the perf data report

`node lifecycle.mjs` (via `provision.mjs`) is the comprehensive run — every path a real
event drives at once, including the **heavy orchestration paths**: real BYOC tunnel spawn
(`FLEET=N` relay agents on a self-hosted A&D challenge), jeopardy **container** create→
destroy, **attachment** upload+download (`/assets`), and **KotH capture** (out-of-band
token writes to a real hill's `/koth/king`). KotH capabilities are scoped to one hill,
container identity, and crown cycle. Provisioning waits for the active cycle and keeps
one capability stable long enough to exercise provisional → confirmed capture; after a
pristine reset, the harness verifies that the previous cycle's capability is rejected.

- **Full latency distribution:** the k6 script already sets
  `summaryTrendStats: ['avg','med','p(90)','p(95)','p(99)','max']`; run with
  `SUMMARY_JSON=out.json … node lifecycle.mjs` to export per-endpoint p50/p90/p95/p99/max
  as JSON. Sample CPU/RAM alongside with `docker stats --no-stream rsctf-rsctf-1 rsctf-db-1`.
- **Keep the report in the repository** with full data (per-endpoint percentiles, a
  CPU/RAM time series, findings) plus a **before-to-after optimization ledger**. Codex
  agents must update that report whenever a run shifts the numbers and add one ledger
  row per optimization.
- **Ledger rule:** a change only lands with a `before` and an `after` from the *same
  harness at the same fixed load*. Compare **CPU-% (or the endpoint's p95) at a held
  rate**, never peak req/s (host noise swings it thousands of req/s between passes).
- Current baseline to beat (500 teams, 400 VUs+80 tunnels, 8-core host, `--release`):
  ~1.4k req/s steady · scoreboard/KotH board p50 60–85 ms, A&D State/Targets cached
  (p50 285/59 ms) · CPU ~2.4/8 cores · RAM 284 MiB peak, bounded (0 % 5xx, integrity clean).
- Optimization log (see the ledger in the report artifact). The read-caching pattern —
  split a per-poll read into a **game-global half** (cached 5 s + single-flight) and a
  cheap **per-request tail**, keeping any freshness-critical field (a flag, the round)
  live: **asset serving** (`assets.rs`, 734ad32) p95 3.5 s→917 ms; **Ad/Targets**
  (`ad/targets.rs`, b80f2ae) filter the caller off the cached global set, 24→4 ms/poll;
  **Ad/State** (`ad/scoreboard.rs`, f5dbb7a) cache config + challenge map, keep round +
  this team's flags fresh, p95 788→538 ms. **find_participation** (`game/mod.rs`, ed1f58d)
  reuses the A&D accepted-only participation cache for jeopardy `/details` + submit
  (`context_info` re-queried 2 rows/request) — jeo details p50 296→198 ms (−33 %), submit
  median 887→~720 ms. Split k6 trends per endpoint before measuring
  (`ad_state_ms`/`ad_targets_ms`/`jeo_submit_ms`) — a blended trend hides which half moved.
- Standing target: the **submit tail** (p50 ~720 ms good, but p95/p99 bounce ±40 %) is now
  **write-bound**, not read-bound — the graded-submission insert + on-solve event/notice
  writes under a flood. Read-caching is done; the lever is deferring the on-solve event +
  notice writes off the response path (like the asset download event) — it's a write, so
  it can't be TTL-cached.
