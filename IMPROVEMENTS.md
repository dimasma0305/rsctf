# RSCTF improvement rollout

Approved scope: the September 8, 2026 UI/UX and backend review. Work proceeds in
small, locally verified releases. Existing functionality is reviewed before adding
anything; a historical bug report or unchecked TODO is not proof a bug remains.

Status: **the saved-configuration slice of step 1 is shipped**. Runtime preflight
and the remaining steps are queued, not completed. This plan does
not authorize changes to live competition scores, participants, or event settings.

## 1. Event readiness and actionable failures

- [x] Saved-configuration readiness page: schedule, freeze, writeup deadline,
  enabled/approved challenges, latest build outcomes, and links to the owning UI.
- [x] Explicitly separate configuration checks, stale data, and unverified runtime
  readiness. Hidden rehearsals and prebuilt images must not produce false failures.
- [x] Actionable initial-load, denied-access, refresh, and service-failure states
  on the readiness page.
- [ ] Follow-up: inspect real roster, checker, worker, capacity, VPN, and instance
  readiness through bounded existing service owners; avoid per-challenge fan-out.
- [ ] Extend useful failure messages to event setup, player access/downloads,
  repo imports, builds, bulk actions, mail, and settings without exposing secrets.

Acceptance: unit cases for known/unknown states; browser fixtures for loading,
errors, refresh, roles, keyboard, 320px and translations; no background polling
added; live immutable release verification. No global “ready” claim based only on
metadata. An operational preflight must use actual runtime evidence.

## 2. Writeup grading and safe admin workflows

- [ ] Review existing grading behavior, including retained drafts when switching
  teams/tabs, navigation-away warnings, conflict recovery, and private projections.
- [ ] Add review notes/history and export where needed; preserve official scoring.
- [ ] Audit CSV import history, row editing, individual mail retry, event/team
  assignments, bulk outcomes, divisions, and manager navigation.
- [ ] Make recovery paths clear without duplicate email, imports, or submissions.

Acceptance: realistic multi-page PDF and repeated navigation tests, bounded memory,
private authorization tests, draft/conflict regressions, and auditable mutations.

## 3. Container capacity and multiplayer reliability

- [ ] Reconcile current implementation against the Docker scalability list in
  [TODO.md](TODO.md), then address confirmed gaps in admission, logs, health-check
  churn, worker lifecycle, and image preflight.
- [ ] Measure VPN, TCP, UDP, BYOC and multiplayer behavior at fixed arrival rates;
  distinguish server saturation, transport loss, client connectivity, and game bugs.
- [ ] Improve tunnel/session continuity and truthful recovery/status indicators.
- [ ] Verify repo sync limits and build/install recovery without unnecessary rebuilds.

Acceptance: bounded tests on disposable resources, stated load and loss conditions,
CPU/memory/network measurements, integrity checks, and no disruption of live games.
Do not promise flawless connections under arbitrary client network conditions.

## 4. Scoring and event lifecycle

- [ ] Review join/auth boundaries, mid-event challenge activation, late teams,
  end/extend transitions, finalization, decay, and format-specific reset behavior.
- [ ] Explain earned points and finalized/provisional scoring in player/admin views.
- [ ] Verify scoreboard freshness, A&D roster visibility, and solo/NPC behavior
  where challenge contracts explicitly support it.
- [ ] Add missing lifecycle, retry/idempotency, revision, and authorization tests.

Acceptance: real database/worker tests where needed, deterministic scoring fixtures,
and explicit permission before retroactively changing live competition data/rules.

## 5. Shared UI, navigation, and rendering

- [ ] Unify page sizing, spacing, hierarchy, loading/error/empty states, navigation
  state, transitions, and notifications across player and admin views.
- [ ] Profile rendering; provide appropriate reduced-motion/lightweight behavior.
- [ ] Audit keyboard, screen-reader semantics, focus, contrast, 320px layout, and
  localization; retain the approved classic/refined visual direction.

Acceptance: before/after screenshots, browser performance traces, route/navigation
regressions, Axe checks and an explicit statement of manual accessibility limits.

## 6. Player sections

- [ ] Home and event discovery; event overview and preparation.
- [ ] Login, recovery, teams, participation and account/profile statistics.
- [ ] Challenge cards/list, globe, details, flag submission and feedback.
- [ ] Scoreboard explanations; A&D/BYOC tools, KoTH status and VPN diagnostics.
- [ ] Writeup uploads, guides and interactive tours, posts and About.

Review existing behavior first. Fix confirmed usability problems with focused
before/after evidence, rather than redesigning every page without a clear benefit.

## 7. Remaining admin sections

- [ ] Dashboard, event/challenge editors, imports, teams and bulk operations.
- [ ] Repository bindings, builds, instances, workers and operator timelines.
- [ ] Monitoring/anti-cheat, logs/audit, settings, notices and content management.

Acceptance: obvious next actions, preserved filters/context/drafts, bounded requests,
per-item outcomes, backend authorization and consistent responsive navigation.

## 8. Backend resilience and operations

- [ ] Auth boundaries, idempotency and revisions across remaining controllers.
- [ ] Realtime/polling ownership, query/pool bounds and cache consistency.
- [ ] Background jobs, durable uploads/storage recovery and email delivery.
- [ ] Defensive security review, observability and actionable operator diagnostics.
- [ ] Backups and isolated restore drills; faster local-first CI/release checks.

Acceptance: focused regressions, real infrastructure where behavior depends on it,
no unbounded resources, and one verified immutable digest per production release.
Security work stays defensive; do not run exploitation or stress against live users.

## Release evidence

Each shipped slice records commit, local checks, visual evidence, immutable digest,
replica health and changed-behavior smoke results here. A local commit or green CI
alone does not complete a deployment step.

### Step 1: saved-configuration checklist

Implemented under **Event administration → Readiness**, with attention-first
ordering, explicit unknown/stale states, manual refresh and no mutation controls.
English and Indonesian copy use the existing admin shell. Runtime checks remain
manual; this slice does not claim complete event operational readiness.

Local verification: strict frontend typecheck/lint/build, 628 client tests and 16
visual-harness unit tests passed. All 12 browser states passed without reported Axe
violations, overflow or runtime exceptions. Fixtures cover permission loss, transient
failures, loading, empty events, manager navigation, keyboard and compact layouts.
Screenshots are retained in `visual-audit-output/event-readiness/`. These are
automated accessibility checks plus visual inspection, not a manual screen-reader
audit. Rust formatting, a warning-free all-target build and 1,703 tests passed;
410 infrastructure-dependent tests were ignored by the standard suite. The initial
two-job build exceeded its 12 GB cgroup cap; the single-job retry passed without
raising that cap. Real existing API reads returned 200 for the administrator, 401
anonymously and 403 for a non-manager.

Shipped September 8, 2026:

- Code commit: `2b6ffab68414ab1a7c427fe84c443cde1e51f7da`, pushed to `main`.
- Package version: `0.1.118`.
- Image: `ghcr.io/dimasma0305/rsctf@sha256:cf89f69dce496ae583bd9925406b39477d9dcd540fc89aed934bab97ed583ab5`.
- [Release pipeline](https://github.com/dimasma0305/rsctf/actions/runs/34207046816)
  passed, including platform/label verification and attestation.
- TCP: two web replicas, control, and firewall helper are healthy on that digest
  with zero restarts at verification. `/healthz` returned exactly `ok`; recent
  application logs contained no warnings, errors or unexpected 5xx.
- Intechfest: the frontend is extracted from that same image; the existing backend
  was not replaced or restarted by the rollout. `/healthz` returned exactly `ok`.
- Both domains passed 12 live-origin browser fixture states. TCP's HTML (including
  its documented anti-autofill injection) and 24 referenced assets matched the image;
  Intechfest's HTML and 24 referenced assets matched the extracted image frontend.
- Databases and Redis retained their container IDs and start times. No competition
  data, scores, users, or challenge containers were changed by this rollout.

Host evidence: `/root/rsctf-production/releases/sha-2b6ffab6/DEPLOYMENT.md` and
`visual-audit-output/event-readiness-{tcp,intechfest}/`. This release does **not**
complete runtime preflight, writeup draft protection, or the remaining roadmap.

### Reliability findings queued for investigation

During pre-release inspection on September 8, TCP control and the Intechfest source
backend had restarted after `traffic capture owner heartbeat timed out`, including
around 08:24 UTC. No new backend code had been deployed. Follow up with bounded
latency/lease diagnostics and disposable database tests; do not weaken fencing or
claim the cause is known from these logs alone.

An orphaned headless visual-audit browser (parent PID 1, temporary audit profile)
was stopped, releasing about 6.3 GB of renderer memory. This is not proof it caused
the heartbeat timeouts. No events, users, scores, files, or running challenge
containers were removed.
