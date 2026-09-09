# RSCTF improvement rollout

Approved scope: the September 8, 2026 UI/UX and backend review. Work proceeds in
small, locally verified releases. Existing functionality is reviewed before adding
anything; a historical bug report or unchecked TODO is not proof a bug remains.

Status: **the saved-configuration slice of step 1 and two visual-refinement passes
are shipped**. Runtime preflight and the remaining steps are queued, not completed. This plan does
not authorize changes to live competition scores, participants, or event settings.

Visual direction: keep the platform attractive and recognizable. Removing generic
"AI slop" means cutting filler, repetitive panels, and distracting decoration—not
removing color, artwork, or personality. Preserve distinct event colors, the
competition visuals, readable labels, and the established classic/refined theme.
For posterless event cards, use subtle motifs and translucent color fades into a
neutral base, not bold, full-height color bands.

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

- [x] First visual refinement: About, Posts, Guide, and event Readiness. Remove
  oversized decorative introductions and repeated cards; retain working controls,
  competition visuals, permissions, and explicit readiness limits.
- [x] Home and event discovery visual refinement: compact shortcuts, reading feed,
  simpler catalog controls, and wrapping event/membership labels.
- [ ] Event overview and preparation.
- [ ] Login, recovery, teams, participation and account/profile statistics.
- [ ] Challenge cards/list, globe, details, flag submission and feedback.
- [ ] Scoreboard explanations; A&D/BYOC tools, KoTH status and VPN diagnostics.
- [ ] Writeup uploads, guides and interactive tours, posts and About.

Review existing behavior first. Fix confirmed usability problems with focused
before/after evidence, rather than redesigning every page without a clear benefit.

## 7. Remaining admin sections

- [x] Dashboard visual refinement: compact totals, one refresh action, flatter
  chart/activity sections, and no duplicate shortcut panel.
- [ ] Event/challenge editors, imports, teams and bulk operations.
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

### September 9: content-first visual refinement

Reviewed rendered About, Posts, Guide, Readiness, Builds, and Profile screens.
Changed the first four: compact branding and resource rows on About; a reading
list on Posts; a smaller walkthrough and unboxed Guide article; attention-first
Readiness rows with contextual links. Builds and Profile were not redesigned in
this pass. The competition globe and official scoring behavior are unchanged.

- UI commit: `e50164c2213fba6ca71dc54fb79fa375c2694f7a`.
- Released commit: `3c75474c6133212ecde04927f79677cc9bdc639f`, package `0.1.118`.
- Image: `ghcr.io/dimasma0305/rsctf@sha256:4abe2e11985fea535511f03fe7c792258f1d4ff0fec1bc953e5e84fd841d80d2`.
- [Release pipeline](https://github.com/dimasma0305/rsctf/actions/runs/34300821117)
  passed. Its initial predecessor was blocked by dependency advisories; narrow
  updates to xmldom 0.8.15 and js-yaml 4.3.2 cleared the high-severity audit gate.
- Local strict check, lint, build, 630 client tests, and 18 fixture tests passed.
  Rust build/fmt passed without warnings; 1,703 tests passed, 410 environment-only
  cases remained ignored locally. No Rust files exceeded the size limit.
- Browser coverage included 17 community states, 12 readiness states, 72 guide
  states, 58 competition views, 12 verdict flows, and full-content responsive
  audits. The final image audit passed desktop and 320px checks.
- Both live origins passed 12 readiness fixture states and six public page
  renders. Final audits had no Axe violations, overflow, or browser errors.
  Tests include keyboard, Indonesian, light mode, and reduced motion; a manual
  assistive-technology audit was not performed.
- TCP's two web replicas, control, and firewall helper are healthy on the exact
  image, with zero restarts. Both origins return exact `ok` from `/healthz` and
  serve the matching 24 entry assets. Real TCP readiness reads return 200 for
  admin, 401 for anonymous, and 403 for a non-manager.
- Intechfest serves the frontend extracted from the same image. Its backend,
  databases, Redis, events, scores, and challenge containers were not changed.
  Post-start application logs show no errors, panics, or unexpected 5xx.

Evidence and rollback: `/root/rsctf-production/releases/sha-3c75474c/DEPLOYMENT.md`.
Screenshots: `visual-audit-output/refined-{local,candidate,live}/`. Final results
include documented transient browser rechecks; they are not a claim that every
page or the broader roadmap is complete.

### September 9: Home, Games, and Dashboard refinement

Reused the existing theme, PostCard feed layout, navigation, and AdminPage header
actions. Removed oversized introductions, decorative placeholder rings/stripes,
redundant shortcut panels, and nested section cards. Event state and membership
labels now wrap in the content column instead of clipping inside narrow posters.
Home leads with announcements; the Dashboard's three totals fit above the mobile
dock. No scoring, authorization, API, polling, or database behavior changed.

- UI commit: `7af71cb8c854e572a03ef556fbc9bec27c3b82ca`.
- Released commit: `fda113656f58af503f7c127913049ec802c49706`, package `0.1.118`.
- Image: `ghcr.io/dimasma0305/rsctf@sha256:b5fc21b796945966f3c94859ef5a9065c871a26db6bc44d3fa179339f1646e4d`.
- [Release pipeline](https://github.com/dimasma0305/rsctf/actions/runs/34306533857)
  passed, including image verification/attestation and Kubernetes isolation.
- Local strict frontend check, lint, build, 632 client tests, and 20 fixture tests
  passed. The dependency audit has one low finding; its high-severity gate passed.
  Rust fmt/build passed without warnings; 1,703 tests passed, with 410
  environment-dependent tests ignored locally.
- All 27 final-source and 27 packaged-image browser states passed, including
  320px–1920px, light/Indonesian, reduced motion, keyboard navigation, filtering,
  refresh/tabs, and loading/error/empty states. The 12-render full-page audit passed;
  nine reviewed warnings describe intentional schedule-label ellipses. Full titles
  remain available in event cards and schedule link names/title attributes.
- The initial rollout exposed low contrast on placeholder event IDs in light mode.
  The corrective commit removes the residual text opacity and adds source/browser
  regressions. The complete packaged theme matrix passed before the second rollout.
- TCP's two web replicas, control, and firewall helper are healthy on the exact
  image with zero restarts. Both sites return exact `ok` from `/healthz`; their
  three changed route HTML documents and 24 entry assets match the release.
- Intechfest serves the frontend extracted from the same image; its backend was
  not replaced or restarted. Databases, Redis, competition data, and challenge
  containers were not changed. Anonymous admin reads still return 401.

- Final live-origin suites passed 27 states per domain. One initial TCP chart-state
  contrast report was not reproduced in three focused loads/six checks or the
  complete unchanged rerun; its cause is unconfirmed and the failed evidence is
  retained. No application styles or assertions were altered for that recheck.
- Eight real public Home/Games renders passed with no Axe violations, overflow,
  or runtime errors. Two Intechfest warnings are intentional narrow schedule-label
  ellipses, not clipped card statuses. Post-start logs show no errors, panics,
  heartbeat failures, or unexpected 5xx; final replica checks still pass.

Automated checks are not a formal accessibility certification; manual NVDA or
VoiceOver testing was unavailable. No performance or network-stability claim is
made. The remaining pages and broader roadmap are not marked complete.

Evidence and rollback: `/root/rsctf-production/releases/sha-fda11365/DEPLOYMENT.md`.
Screenshots and reports: `visual-audit-output/refined2-contrast-{local,full,candidate}/`
and `visual-audit-output/refined2-final-live/`.

### September 9: restore event-card color

The visual cleanup went too far for cards without posters. Restored their stable
per-event gradients while preserving the simpler layout and wrapping status labels.
Uploaded artwork is untouched; opaque icon/ID backgrounds protect contrast in both
themes. The design direction near the top of this plan now explicitly preserves
color and personality when removing generic decoration.

- Released code: `af212f21a31b144c402454c10fc9cbe085313678`, package `0.1.118`.
- Image: `ghcr.io/dimasma0305/rsctf@sha256:b4a7213bd22ba7d61a930b50fa17f2c3ade88ac160b3e98c12c1e99c1d651623`.
- [Release pipeline](https://github.com/dimasma0305/rsctf/actions/runs/34310159913)
  passed. Local strict check/lint/build, 633 client tests, and 1,703 Rust tests passed
  without build warnings; 410 infrastructure-dependent tests were ignored locally.
  The dependency audit has one low finding and passed the high-severity gate.
- Local and packaged 27-state browser runs and a 12-render full-page audit passed.
  Final live runs passed 27 states per origin plus eight real public page renders,
  without reported Axe violations, overflow, or browser errors. Reviewed narrow
  schedule-label warnings and initial harness failures are retained in the evidence.
- Follow-up test-only corrections scope injected setup to the intended top-level
  origin and await fonts/focus rendering before assertions. All 21 harness unit
  tests pass; application error/contrast assertions remain enabled. Tests are
  excluded from the image, so this follow-up does not require another deployment.
- TCP's two web replicas, control, and firewall helper are healthy on the exact
  image with zero restarts. Both origins return exact `ok` and serve matching HTML
  and 24 entry assets. Post-start logs show no errors, panics, or unexpected 5xx.
- Intechfest uses that image's frontend without a backend restart. No databases,
  competition data, or challenge containers were changed. Automated accessibility
  checks are not a formal certification; manual NVDA/VoiceOver testing was unavailable.

Evidence and rollback: `/root/rsctf-production/releases/sha-af212f21/DEPLOYMENT.md`.
Real catalog screenshot: `visual-audit-output/event-colors-live/tcp-public/desktop--games--index--viewport.png`.

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
