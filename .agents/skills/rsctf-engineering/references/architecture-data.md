# Architecture, data, and security contract

## Design and ownership

- Keep responsibilities cohesive and dependencies explicit. Extract shared business
  rules only when they are genuinely shared; avoid speculative extension points.
- Put player routes under `controllers/game/`, platform administration under
  `controllers/admin/`, and event/challenge editing under `controllers/edit/`.
  Each area is a folder module whose `mod.rs` owns `router()`, shared types, and public
  re-exports. `server.rs` merges only top-level area routers.
- Nest Attack-Defense and KotH code under cohesive `ad/` or `koth/` domains. Do not add
  top-level `ad_*` modules. Keep compatibility re-exports when moving code.
- Split services and model domains the same way (`services/ad/engine/`,
  `services/suspicion/`, `models/data/<domain>.rs`). Preserve flat model paths with
  `pub use` when appropriate.
- No `.rs` file may exceed approximately 1000 lines. Split by responsibility, not at
  an arbitrary line boundary. Check with:

  ```sh
  find src -name '*.rs' -print0 | xargs -0 wc -l | awk '$1>1000 && $2!="total"'
  ```

## Data access

- New reads and writes use the SeaORM-owned `PgPool` through raw `sqlx`. Do not add new
  `Entity::find/insert/update/delete` or `ActiveModel` code unless raw SQL would be
  materially more error-prone; document that exception.
- Quote PascalCase tables, bind every value with `$1..`, and never interpolate user or
  domain values into SQL.
- Bound list queries and push filtering, aggregate, count, `DISTINCT ON`, and ordering
  into PostgreSQL. Add indexes for hot filter/sort columns.
- Upsert atomically with `INSERT ... ON CONFLICT (...) DO UPDATE SET ... = EXCLUDED...`.
  The conflict target must have a matching unique index. Never read-check-then-insert.
- Detect unique violations by SQLSTATE `23505`, not error text.

## Migrations

- Add a new, forward-only `mXXXX_*.rs` and register it in `migrations/mod.rs`.
- Never edit a shipped migration. Make creation/addition idempotent with
  `if_not_exists` or `add_column_if_not_exists` so existing deployments upgrade.
- Preserve production data. Destructive backfills or cleanup require exact scope,
  validation, and explicit authorization.

## Wire contract

- DTOs use `#[serde(rename_all = "camelCase")]`.
- Enums serialize as names such as `"Admin"` and `"Misc"`; only `ReviewRating` and
  `GamePermission` are numeric.
- Timestamps are Unix-millisecond numbers with `utils::datetime::millis` or
  `millis_opt`.
- Successful endpoints return the raw model unless the established endpoint contract
  explicitly uses `RequestResponse` or `ArrayResponse`.
- Keep the generated/manual TypeScript contract in `web/src/Api.ts` synchronized and
  test both shape and behavior.

## Trust boundaries

- Enforce visibility and mutation authorization in SQL/backend code, then let the
  client present the result. A hidden button is never authorization.
- For event data, explicitly test anonymous, non-member, pending/rejected/suspended,
  accepted, wrong-division, hidden, pre-start, ended/practice, monitor, and admin cases
  that matter to the endpoint.
- Do not expose flags, image/build secrets, JWTs, registry credentials, VPN credentials,
  security stamps, or provider API keys through DTOs, logs, screenshots, or tests.
- Follow applicable SEI CERT secure-coding rules. Apply MISRA only to safety-critical
  C/C++ work and never claim compliance without formal analysis and documented
  deviations.
