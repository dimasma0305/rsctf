# rsctf agent entry point

rsctf is a Rust CTF platform with a React client in `web/`. Keep public APIs stable
unless the request changes them, and put behavior in the layer that owns it.

## Mandatory engineering workflow

For any implementation, debugging, review, optimization, test-harness, release, or
deployment work, read `.agents/skills/rsctf-engineering/SKILL.md` completely and follow
its routing table. Read every reference it selects before editing. That skill contains
the authoritative architecture, raw-SQL, wire-format, frontend/accessibility,
performance, verification, and production-release checklists.

A request to explain, diagnose, or review is read-only unless it also asks for changes.

## Fast non-negotiables

- Apply SOLID, DRY, KISS, and YAGNI as practical design tests. Prefer the smallest
  cohesive change; do not add abstraction or configuration for hypothetical work.
- Preserve unrelated dirty-worktree changes. Use `rg`/`rg --files` for discovery and
  `apply_patch` for manual edits.
- No `.rs` file may exceed approximately 1000 lines. Split large modules by cohesive
  responsibility while preserving public re-exports.
- Player, admin, and edit controllers remain folder modules under
  `controllers/{game,admin,edit}/`; `server.rs` merges only their top-level routers.
  Attack-Defense/KotH code belongs under cohesive `ad/` or `koth/` domains, never new
  top-level `ad_*` modules.
- New database access uses bound raw `sqlx` through the existing `PgPool`, not new
  SeaORM query code. Upserts are atomic `ON CONFLICT` operations backed by a unique
  index. Never read-check-then-insert.
- Schema changes use a new registered, idempotent forward migration. Never edit a
  shipped migration.
- Wire DTOs are camelCase; enums are string names except the established numeric
  exceptions; timestamps are Unix milliseconds; success bodies follow the existing
  raw/envelope endpoint contract.
- Authorization belongs on the backend. Test negative boundaries such as anonymous,
  non-member, pending/rejected, wrong division, hidden, and pre-start access.
- Hot request paths may not block Tokio, convoy on one lock, dogpile PostgreSQL, copy
  cached bodies unnecessarily, or grow Redis/storage without a bound.
- React work must match the existing theme, be keyboard and screen-reader operable,
  respect reduced motion, and work without overflow down to 320px.
- Every bug/security boundary gets a regression test. Use real PostgreSQL, container,
  worker, VPN, or browser behavior when the risk depends on it.

## Build and completion gate

Run every local Cargo compile or test through `scripts/bounded-cargo.sh`; never start
raw Cargo builds concurrently from separate worktrees. The wrapper serializes builds,
shares dependency artifacts, and hard-caps CPU/memory on systemd hosts. Focused checks
still come first, but the final build/test gate also uses the wrapper. CI may use an
equivalent stricter isolated runner.

`cargo build` must have zero errors and zero warnings; `cargo test` must pass. Run the
strict frontend typecheck, lint, tests, build, and relevant visual/Axe audits. Polled
reads, A&D/KotH, BYOC, or performance changes also require the fixed-rate load workflow
defined by the engineering skill.

`https://tcp.1pc.tf` is canonical production. Any completed change that can affect a
built/deployed artifact must be released and deployed as one verified immutable digest
to every applicable replica. A local build, commit, push, tag, or green workflow is not
completion. Verify exact `healthz` body `ok`, replica health/version/digest, changed
behavior, recent logs, and installer endpoints when applicable. Report explicitly when
production is not deployed and why. Prose/comments/tests that cannot affect an artifact
may skip deployment, but say so.

## Specialized safety

Follow applicable SEI CERT secure-coding rules. For safety-critical C/C++, apply the
relevant MISRA rules and do not claim compliance without formal analysis, documented
deviations, and verification evidence.

## Writing-skill routing

- An explicit AI-pattern audit or detector-facing prose cleanup uses
  `.agents/skills/avoid-ai-writing/SKILL.md`.
- An explicit `$humanizer` or natural-voice rewrite not framed as an AI-pattern audit
  uses `.agents/skills/humanizer/SKILL.md`. Do not run both unless asked.
- Academic writing uses the academic-writing workflow first; an optional
  avoid-AI-writing pass is final and minimal. Preserve citations, quotations,
  equations, technical terms, justified hedging, headings, and evidence.
- Writing edits improve quality; never guarantee detector classification.
