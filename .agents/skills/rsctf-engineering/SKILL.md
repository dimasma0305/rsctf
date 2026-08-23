---
name: rsctf-engineering
description: Implement, review, debug, optimize, test, or deploy changes in the rsctf Rust and React repository. Use for backend controllers and services, SQL or migrations, wire contracts, the web client, CTF container and VPN behavior, load or visual harnesses, release workflows, and production changes to tcp.1pc.tf.
---

# RSCTF Engineering

Use this workflow for repository work under `/root/homelab/rsctf`. Keep the design small,
preserve public behavior unless the request changes it, and finish with evidence rather
than an implementation-only handoff.

## Route the task

Read only the references relevant to the work, but read each selected file completely:

- Backend structure, raw SQL, migrations, DTOs, or security boundaries:
  [architecture-data.md](references/architecture-data.md)
- React pages/components, responsive behavior, theme consistency, or accessibility:
  [frontend-quality.md](references/frontend-quality.md)
- Polled reads, resource usage, stress tests, or optimization claims:
  [performance-load.md](references/performance-load.md)
- Tests, release artifacts, GitHub workflows, or production deployment:
  [verification-release.md](references/verification-release.md)

For cross-layer changes, read every applicable reference. A prose-only explanation or
review does not authorize source changes or deployment.

## Workflow

1. Inspect `git status`, nearby implementation, tests, and current public contracts.
   Preserve unrelated work and use `rg`/`rg --files` for discovery.
2. State the behavior and trust boundary before editing. Put each rule in the layer
   that owns it; do not duplicate authorization or scoring rules in the client.
3. Choose the smallest cohesive design. Apply SOLID, DRY, KISS, and YAGNI as design
   tests, not as reasons to add abstraction.
4. Implement with bound inputs, explicit authorization, bounded reads/caches, and
   deterministic state transitions. Add a forward migration for schema changes.
5. Add a regression test for the failure or boundary being changed. Authorization
   work needs negative cases; concurrency work needs race/integrity checks; UI work
   needs keyboard, responsive, and accessibility coverage.
6. Run focused checks early, then the full checks required by the selected references.
   Fix warnings rather than suppressing them without a documented reason.
7. If a released artifact changed, publish and deploy one immutable release digest to
   every applicable `tcp.1pc.tf` replica, then perform live health, smoke, digest, and
   log checks. A commit, tag, or green workflow alone is not completion.

## Non-negotiable review questions

- Can an anonymous, rejected, pending, wrong-division, pre-start, or hidden-event user
  learn data or perform an action they should not?
- Did a new query use raw `sqlx`, bound parameters, bounded results, and the necessary
  indexes or unique constraints?
- Does the wire format remain camelCase with string enums and Unix-millisecond times?
- Can this path block Tokio, convoy on one lock, dogpile the database, or grow Redis or
  storage without a bound?
- Is the UI operable at 320px and by keyboard/screen reader, with focus restored after
  dialogs and motion reduced when requested?
- Is there a regression test that fails on the previous behavior?
- If an artifact changed, what exact immutable digest is live and what proves all
  replicas and smoke tests match it?
