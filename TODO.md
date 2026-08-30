# TODO

## Event smoothness and live reliability

Findings from the ongoing event-smoothness and client request-amplification review
on 2026-08-25.

### P0 — Fix before the next live event

- [x] Make event start and end transitions reactive across the event detail, challenge,
  scoreboard, catalog, and home pages.
  - Drive `getGameStatus` consumers from a shared clock instead of relying on unrelated
    React renders.
  - Start and stop Jeopardy scoreboard and team-info polling when the clock crosses the
    event boundaries without requiring a page refresh.
  - Apply the same clock transition to player A&D state and the A&D/KotH operator
    console: a console opened before kickoff currently chooses a zero refresh interval
    and has no render scheduled at kickoff to turn polling on.
  - Revalidate authoritative timing at a bounded visible/online cadence and on
    focus/reconnect so an organizer's start/end extension or early close is not pinned
    to the one game-detail response loaded at navigation time.
  - Drive event discovery from the same local boundary clock. The catalog currently
    regroups cards only when React happens to render or its five-minute list poll
    completes, while the home/recent feed can keep “Upcoming,” “Live,” remaining-time,
    and live-count labels stale for its 30-minute refresh interval. Drive every card
    from one shared server-corrected ticker and keep one bounded visible-only list
    refresh owner rather than starting per-card polling.
  - Avoid trusting an uncorrected client clock for authoritative event access.
  - Add fake-timer regression tests covering a page opened before kickoff and kept open
    through both kickoff and event close, catalog/home lifecycle regrouping and counts,
    plus start/end edits while the page remains open.
  - Relevant code: `web/src/hooks/useGame.ts`,
    `web/src/pages/Index.tsx`, `web/src/pages/games/Index.tsx`,
    `web/src/components/GameCard.tsx`, `web/src/components/RecentGame.tsx`,
    `web/src/components/mobile/RecentGameSlide.tsx`,
    `web/src/components/charts/GanttTimeline.tsx`,
    `web/src/pages/games/[id]/Index.tsx`, and
    `web/src/pages/games/[id]/Scoreboard.tsx`,
    `web/src/pages/games/[id]/Challenges.tsx`, and
    `web/src/pages/admin/games/[id]/AdOps.tsx`.

- [x] Make SignalR notices and operator feeds recover without silently losing events.
  - Retry failed initial connections, handle exhausted reconnects, and cancel retry work
    during unmount.
  - Use capped exponential backoff with jitter instead of SignalR's deterministic default
    reconnect delays so a replica recovery does not synchronize every open client.
  - Use a failure-detection timeout consistent with the server's 15-second keepalive.
  - Keep connection effects keyed to stable route/lifecycle primitives. The monitor
    Events and Submissions effects depend on the complete `game` object, so any
    revalidation or mutation that replaces an otherwise unchanged object tears down
    and recreates the hub connection. Avoid synchronized handshake churn and event
    gaps by sharing one route-scoped monitor connection or depending only on the game
    ID and authoritative open/closed transition.
  - Revalidate the HTTP source after reconnect so messages missed during an outage are
    backfilled and deduplicated.
  - Keep a bounded polling fallback for announcements, submissions, game events, and
    administrative logs.
  - Apply the same lifecycle to the container exec modal. Its one-shot `hub.start()`
    never retries an initial failure, SignalR's finite default reconnect schedule can
    leave an open operator console permanently closed, and every successful reconnect
    must create exactly one replacement PTY because the old one cannot survive the
    transport loss. Provide an explicit Retry action after exhaustion, generation-bind
    `Open`, and keep the existing connection/session admission as the server authority.
  - Add initial-connection failure, harmless game-object revalidation,
    reconnect-gap/exhaustion, concurrent reconnect, exactly-one replacement PTY,
    429/503, and unmount regression tests.
  - Relevant code: `web/src/components/GameNoticePanel.tsx`,
    `web/src/pages/games/[id]/monitor/Submissions.tsx`,
    `web/src/pages/games/[id]/monitor/Events.tsx`, and
    `web/src/pages/admin/Logs.tsx`,
    `web/src/components/admin/ContainerExecModal.tsx`, and
    `src/hubs/container/admission.rs`.

- [x] Bound the live submission, event, notice, and log collections.
  - Cap and deduplicate in-memory entries, following the established Flag Egress feed
    pattern.
  - Put stable submission/event IDs (and a reconnect cursor) on both HTTP and SignalR
    payloads; timestamp-plus-array-index keys cannot reliably deduplicate a message
    that arrives from the socket and later from an HTTP backfill.
  - Avoid copying and rendering the complete event history for every incoming message;
    paginate or virtualize where appropriate.
  - Apply the active search/type filters to live rows as well as the HTTP page, and
    clear every live buffer immediately when the route's game ID changes. Flag Egress
    currently carries game A's buffered rows into game B, while monitor events and
    submissions can display newly arrived rows that do not match the current search.
  - Give each HTTP list read an abortable request generation and ignore responses from
    superseded game, page, search, type, or visibility state. None of these effects
    currently cancels its previous read, so a slow response for an old game/filter can
    overwrite the current page. Update filter and page state atomically; the admin log
    page's separate level/search page-reset effect can fetch the new filter at the old
    page and immediately fetch it again at page one.
  - Add a sustained-message regression or browser performance test that verifies the
    collection and rendered-row bounds, filters, game-to-game isolation, bounded
    request count, and rejection of a deliberately late stale response.
  - Relevant code: `web/src/components/GameNoticePanel.tsx`,
    `web/src/pages/games/[id]/monitor/Submissions.tsx`,
    `web/src/pages/games/[id]/monitor/Events.tsx`, `web/src/pages/admin/Logs.tsx`,
    and `web/src/pages/admin/games/[id]/FlagEgress.tsx`.

- [x] Serialize flag-verdict retrieval and make it resilient to transient failures.
  - Do not start overlapping asynchronous status requests every 500 ms.
  - Preserve the submitted flag and pending state while retrying recoverable failures.
  - Use bounded backoff and cancellation, noting that the backend grades and commits the
    submission before returning its ID.
  - Account for the platform-wide 150-request-per-minute identity limit: the verdict
    loop alone can spend 120 requests per minute, and a KotH challenge modal adds three
    five-second pollers, so ordinary client behavior can throttle itself.
  - Add an appropriate server-side abuse ceiling to the otherwise unadorned status
    route without making legitimate verdict retrieval depend on a fixed 500 ms cadence.
  - Key pending work by game, challenge, and submission. The retained modal's effect
    currently depends only on `submitId`, so a verdict from challenge A can arrive after
    challenge B opens, clear B's flag/proof, and render A's result in B's dialog.
  - Add slow-response, one-time failure, duplicate-callback, modal-unmount, route-change,
    and close-A/open-B-before-A-settles tests.
  - Relevant code: `web/src/components/GameChallengeModal.tsx`,
    `src/controllers/game/submit.rs`, `src/controllers/game/submit_review.rs`,
    `src/controllers/game/routes.rs`, and
    `src/middlewares/rate_limiter.rs`.

- [x] Make normal flag submission idempotent across duplicate dispatch and lost
  responses.
  - The grading transaction inserts and commits the submission, consumes a one-use
    solve receipt, increments the attempt count, and may claim the solve before the
    HTTP response reaches the browser. If that response is lost, the client catches
    the request as a failure, re-enables the form, and preserves the same flag/proof.
    A legitimate retry therefore inserts another submission and consumes another
    limited attempt, or fails because the first request already consumed the receipt;
    only the accepted-score claim is deduplicated by `FirstSolves`.
  - Give each semantic submit a client-generated opaque attempt ID that remains stable
    until its terminal verdict is recovered. Atomically reserve it under a unique
    `(participation, challenge, attempt)` constraint, bind it to a request fingerprint,
    and return the original submission ID/result for exact replays while rejecting an
    attempt ID reused with different content.
  - Keep receipt consumption, attempt accounting, first-solve/notice creation, event
    publication, and anti-cheat evidence exactly once behind that reservation. Add a
    client-side single-flight owner as a usability guard, but retain backend
    idempotency as the authority; the existing submit rate limit remains defense in
    depth rather than correctness state.
  - Add concurrent double-submit, response-lost-after-commit, required one-use receipt,
    one-attempt challenge, wrong-answer replay, mismatched-payload replay, and browser
    retry tests. Every exact replay must recover one submission ID while producing one
    attempt, receipt consumption, notice/event set, and evidence set.
  - Relevant code: `web/src/components/GameChallengeModal.tsx`,
    `web/src/components/ChallengeModal.tsx`, `src/controllers/game/submit.rs`,
    `src/controllers/game/submit_review.rs`, and a new forward migration for the
    idempotency key.

- [x] Replace the admin Instances page's per-row runtime polling before it
  self-rate-limits or overloads the container backend.
  - The default page mounts as many as 100 independent five-second stats pollers:
    1,200 requests per minute from one tab, versus the authenticated global allowance
    of 150 requests per minute. The page therefore throttles itself during normal use.
  - Every request performs a database lookup and a Docker/Kubernetes runtime query;
    missing containers can also enter SWR's default error-retry schedule on top of the
    interval.
  - Replace the row hooks with one bounded batch sample per cadence (or one pushed
    sample), cache each runtime sample briefly, and cap backend runtime concurrency and
    batch size. If batching cannot land immediately, poll only visible rows through a
    shared concurrency-limited scheduler.
  - Pause while hidden/offline or when live stats are disabled, cancel on unmount, do
    not retry 404s, and honor `Retry-After` for 429s.
  - Paginate the list and show the server's real `total`; do not silently monitor only
    the first 100 containers. Either collect real memory limits/network counters or
    label those fields unavailable instead of rendering authoritative-looking zeroes.
  - Add request-count and fixed-rate tests proving that 1 and 100 rows use the same
    number of HTTP requests, runtime concurrency stays bounded, and a missing runtime
    cannot start a retry storm.
  - Relevant code: `web/src/pages/admin/Instances.tsx`,
    `src/controllers/admin/instances.rs`, `src/controllers/admin/mod.rs`, and
    `src/middlewares/rate_limiter.rs`.

- [x] Stop challenge pages from generating background and permanently failing
  request loops.
  - Gate the challenge-detail and solver SWR keys on `modalProps.opened`; the parent
    retains the selected challenge after close, so the top-level modal hooks continue
    polling every 120 and 30 seconds while no dialog is visible.
  - Remove or replace the `/api/game/{id}/Reviews/Summary` poll. No backend route owns
    that URL, yet every challenge page requests it every 60 seconds; a production SPA
    fallback returns HTML and triggers SWR's unbounded default error-retry chain, while
    a plain 404 is silently converted to a successful empty result and polled forever.
  - Never serve the SPA shell for an unmatched `/api` or `/hub` URL. Return a typed 404
    so route typos cannot masquerade as HTTP 200 and drive JSON-parse retry loops.
  - Treat non-JSON, 401, 403, 404, and 429 responses explicitly. Do not turn permanent
    failures into empty success values, and honor `Retry-After` where retry is valid.
  - Cancel modal requests on close/unmount and pause nonessential polling while the
    page is hidden or offline.
  - Page and cap the solver read. The closed modal requests it every 30 seconds without
    `count`; the backend rebuilds/reads the whole scoreboard, scans every team, clones
    every matching solver, and sorts the complete result before returning it.
  - Prefer a visible-page or cursor/delta request so one modal cannot repeatedly project
    an event-wide board merely to show a short solver list.
  - Add request-count regression tests for open/close cycles, missing routes, VPN
    rejection, hidden tabs, long-lived challenge pages, and a maximum-roster solver
    list. Assert that request rate, response size, and in-flight work remain bounded.
  - Relevant code: `web/src/components/GameChallengeModal.tsx`,
    `web/src/components/ChallengePanel.tsx`,
    `web/src/components/KothChallengePanel.tsx`,
    `src/controllers/game/scoreboard.rs`, and `src/server.rs`.

- [x] Split the ten-second player-details poll from the event-wide scoreboard and
  challenge catalog.
  - Every ongoing challenge view polls `/api/game/{id}/details` every ten seconds.
    Although the scoreboard build is cached, each request deserializes the complete
    event board, walks every team to find one caller's row, filters every challenge by
    division permission, and serializes the full visible challenge catalog plus team
    token again. Across `N` active teams, the repeated per-caller `N`-team walk makes
    aggregate projection work quadratic.
  - Each poll also performs participation/context and permission queries, then opens a
    final roster-fence transaction that rechecks the game/division and locks every
    visible challenge before returning. Thousands of synchronized clients can therefore
    turn a nominally cached read into sustained PostgreSQL connection, row-lock, JSON
    allocation, and response-bandwidth pressure during the event.
  - One challenge route mounts the same live hook independently in `Challenges`,
    `ChallengePanel`, and `TeamRank`, creating three ten-second timers for one SWR key.
    The two-second dedupe window may collapse aligned callbacks, but it is not stable
    ownership: delayed mounts, hidden-tab timer throttling, and slow responses can let
    those schedules drift and send multiple identical reads per interval.
  - Cache/version the division-scoped, rarely changing challenge catalog separately
    from a compact participant delta containing rank, score, solved IDs, attempts, and
    other genuinely live fields. Push or explicitly invalidate that delta on accepted
    submissions and roster/configuration mutations; otherwise use one completion-
    scheduled, jittered poll with conditional versions/ETags and no unchanged body.
  - Do not resend the long-lived team token on every refresh. Preserve the exact final
    authorization boundary with a monotonic roster/configuration version or one bounded
    indexed policy query, so caching never serves a revoked participation or stale
    division visibility. A named rate limit is defense in depth, not a substitute for
    making normal event traffic constant-cost.
  - Own the remaining participant refresh once at the route/provider level and pass
    its snapshot and mutation function to child panels; no child component should
    create another timer for the same key or rely on SWR's dedupe interval for
    correctness.
  - Add real-PostgreSQL query-plan and fixed-rate browser tests at maximum team and
    challenge counts, including synchronized tabs, a solve burst, division-policy edit,
    suspension/reinstatement, event close, slow responses, and cache expiry. Per-poll
    rows, locks, bytes, and CPU must stay bounded independently of total team count.
  - Relevant code: `web/src/hooks/useGame.ts`,
    `web/src/components/ChallengePanel.tsx`, `web/src/components/TeamRank.tsx`,
    `src/controllers/game/play.rs`, `src/controllers/game/play_final_policy.rs`, and
    `src/controllers/game/scoreboard_board.rs`.

- [x] Make scoreboard and KotH polling error-aware instead of treating every missing
  response as an unsettled live event.
  - The A&D, KotH, and combined-scoreboard interval callbacks return ten seconds when
    `latest` is absent. A permanent 401/403/404/429 or repeated server failure therefore
    polls forever, while SWR may also run its independent error-retry schedule.
  - An open KotH challenge adds three fixed five-second pollers (token, state, and
    targets) with default retries and no event/error lifecycle policy.
  - The scoreboard route also reads each selected A&D, KotH, or combined board in the
    page for freeze metadata and again inside its table; the desktop Jeopardy view
    reads the standard board separately in its timeline and table. Each hook owns a
    timer, while a source comment assumes SWR will dedupe them. Pass one route-owned
    snapshot into the banner, timeline, and table instead so timer drift cannot double
    normal spectator traffic.
  - The targets poll downloads and freshly overlays the complete A&D/KotH
    challenge-by-team target matrix even though the modal immediately selects one hill.
    Provide a scoped hill read or a shared game-level snapshot so response/database
    work does not scale with every unrelated team and challenge for one open dialog.
  - Give each key one completion-scheduled retry owner. Stop permanent responses,
    honor `Retry-After`, use bounded exponential backoff with jitter for transient
    errors, and suspend hidden/offline/closed-modal work. Keep any deliberate warmup or
    final-settlement polling explicit and bounded.
  - Add fake-timer/request-count tests for pre-start warmup, event closeout, invalid
    game IDs, revoked participation, 429s, outages, modal close, hidden tabs, and
    recovery.
  - Relevant code: `web/src/hooks/useGame.ts`,
    `web/src/components/KothChallengePanel.tsx`, and
    `web/src/pages/games/[id]/Scoreboard.tsx`.

- [x] Stop the post-event maintenance job from creating a six-hour scoreboard cache
  churn loop.
  - Every 30 seconds, `flush_stale_scoreboards` finds every game ended within six hours
    and deletes 12 cache keys. Despite its comment, that includes live, stale, and
    frozen Jeopardy/A&D/KotH representations; the key list also misses the real
    combined-board names.
  - A&D/KotH/combined clients deliberately keep a 60-second final-settlement poll, so
    recently ended boards are repeatedly forced through cold recomputation instead of
    becoming cheap immutable cache hits. The Redis delete work also grows with every
    recently ended event on every maintenance pass.
  - Perform an idempotent, one-time invalidation/materialization when final evidence is
    durably settled. Give immutable final boards a long bounded TTL and remove the
    repeated time-window sweep; retain an explicit repair/admin path for exceptional
    stale data.
  - Add real-Redis/multi-replica tests that count deletes and board builds across event
    close, six hours of fake time, concurrent final-board readers, restart recovery,
    and failed-then-retried finalization. Each board version should build once, not once
    per client minute.
  - Relevant code: `src/services/cron/mod.rs`,
    `src/services/cron/round_finish.rs`, `src/controllers/game/ad/scoreboard.rs`,
    `src/controllers/game/koth/mod.rs`, and `web/src/hooks/useGame.ts`.

- [x] Bound container reaping and orphan detection so maintenance cannot overwhelm a
  live event's database and runtime backend.
  - The 30-second leader pass loads every expired container and destroys them serially
    with no batch or time budget. A backlog can turn one maintenance tick into a long
    stream of database, VPN, and Docker/Kubernetes operations.
  - Orphan detection lists every managed runtime plus every owning container, A&D
    service, KotH target, and active crown-cycle ID. It then linearly compares every
    managed ID with the complete known-ID vector, making the pass quadratic at event
    scale before serially destroying every ready orphan.
  - Claim expired rows in small indexed batches with `FOR UPDATE SKIP LOCKED`, use
    bounded runtime concurrency and a per-pass deadline, and carry backlog explicitly
    to the next tick. Normalize full/short Docker IDs into hash sets so ownership checks
    are constant-time without weakening prefix safety.
  - Keep cleanup jobs independently budgeted so a capture/image/container backlog
    cannot monopolize the maintenance chain. Export scanned/claimed/destroyed/backlog
    counts and pass duration.
  - Add real-runtime and fixed-rate tests with thousands of owned, expired, and orphaned
    resources, failed destroys, concurrent creation, and short Docker IDs. Verify no
    live workload is swept and `healthz`/event requests remain responsive.
  - Relevant code: `src/services/cron/mod.rs`,
    `src/controllers/game/containers.rs`, and
    `src/controllers/game/containers/reaping.rs`.

- [x] Make the A&D/KotH operator console's five-second refresh cost independent of
  event history.
  - The page polls both engine state endpoints throughout every ongoing event, even for
    a pure A&D or pure KotH game and regardless of which view is selected.
  - Each A&D refresh loads every checker-result row for every visible service and then
    finds the latest result in Rust. That read grows for the entire event although the
    response exposes only one result per service.
  - Each KotH admin refresh bypasses the public board cache and recomputes the complete
    epoch scoring board before adding lifecycle, champion, observer, and configuration
    state. Multiple open operator tabs multiply the same cold work.
  - Detect the configured engines from lightweight game metadata, disable absent
    engine keys, and update only the active view unless a shared pushed snapshot keeps
    both current. Fetch latest A&D verdicts with an indexed `DISTINCT ON`/lateral SQL
    query and split static grid metadata from small live deltas.
  - Reuse a versioned, single-flight KotH scoring snapshot for player and admin views;
    overlay the small privileged lifecycle fields afterward. Pause hidden/offline
    clients, honor event transitions, and use bounded status-aware retry.
  - Add real-PostgreSQL query-plan/query-count and fixed-rate tests covering long check
    history, many teams/hills/epochs, pure and hybrid games, multiple operator tabs,
    slow queries, and hidden views. Rows scanned and cold builds per version must stay
    bounded as history grows.
  - Relevant code: `web/src/pages/admin/games/[id]/AdOps.tsx`,
    `web/src/hooks/useGame.ts`, `src/controllers/edit/ad/mod.rs`,
    `src/controllers/game/koth/admin.rs`, and
    `src/controllers/game/koth/board.rs`.

- [ ] Make trusted KotH referee reads and submissions retry-safe before they become a
  positive-feedback load incident.
  - The example referee fetches the complete public context every five seconds. Each
    context request reloads and hashes as many as 2,000 eligible team capabilities and
    returns the full hash roster; the route has no versioned cache, conditional response,
    single-flight build, or named query-work admission.
  - A submission may contain 512 KiB, 64 waves, and 2,000 team-wave rows. The server
    verifies and normalizes it, locks the observer, rebuilds the active context and
    eligible roster, resolves capabilities, loads prior waves, and hashes the snapshot
    before it checks the replay table.
  - The replay identity is the HMAC signature. If the database commit succeeds but the
    response is lost, the supplied referee does not persist `lastSubmittedDigest`; on
    retry it signs the same body with a new timestamp. The replay table therefore sees a
    new request and repeats the full transaction and snapshot-row replacement.
  - Derive an idempotency identity from the authenticated observer, opaque context, and
    canonical body/snapshot digest. Reserve it immediately after bounded body parsing and
    HMAC verification, persist the final response atomically, return that response for an
    exact retry, and coalesce concurrent duplicates instead of reporting a late 409.
    Serialize or reject distinct concurrent submissions for one challenge/round before
    rebuilding the roster and snapshot.
  - Cache/single-flight the context by game, challenge, round, cycle, reset attempt,
    objective schema, and roster/capability generation. Give it a stable ETag, omit or
    separate `generatedAt` so unchanged responses can be 304, and keep encoded-size and
    lifetime bounds explicit.
  - Give context and observation work per-observer/per-challenge weighted admission,
    deadlines, structured failure codes, `Retry-After`, and metrics. In the referee,
    distinguish a refreshable stale context from permanent authentication/schema errors,
    honor `Retry-After`, use capped exponential backoff with full jitter, and reset the
    backoff only after a complete successful cycle.
  - Add real-PostgreSQL tests for a lost response followed by an identical-body/new-
    signature retry, exact-signature and concurrent duplicates, competing distinct
    snapshots, maximum body/roster, context generation changes, and invalid HMAC. Run a
    fixed-rate outage/recovery test that proves one durable snapshot/result, bounded
    database work, and responsive event reads and `healthz`.
  - Relevant code: `examples/challenge-repository/Koth/Web/api-observed-hill/observer/observer.py`,
    `src/controllers/game/koth/api/mod.rs`,
    `src/controllers/game/koth/api/submission.rs`,
    `src/controllers/game/koth/api/submission/evidence.rs`, and
    `src/controllers/game/koth/mod.rs`.

- [x] Make KotH referee-secret rotation recoverable and exactly once.
  - `rotateObserver` uses component-local `observerBusy` as its only duplicate guard,
    while the POST has no operation identity or expected observer version. Rapid
    activation, another operator/tab, or a retry after a lost response can submit two
    distinct rotations.
  - The game lock serializes both requests but deliberately lets both succeed. Each
    generates a different secret, overwrites the prior credential, and deletes the
    current API snapshot and replay ledger. Response construction happens after the
    lock is released, so a later rotation can commit before the earlier caller renders;
    the UI may then show a one-time plaintext secret that is already invalid and whose
    hint/state came from the newer row. Repeated evidence clearing also creates an
    avoidable leaderboard gap.
  - Require an opaque client operation ID and the observer revision/hint the operator
    observed. Atomically reserve the rotation under the existing game lock, persist its
    response/revision, and return the same secret/result for an exact authorized retry.
    Reject a different concurrent operation with a precondition conflict rather than
    silently replacing its credential; clear referee input only on the one committed
    revision transition. Preserve `no-store` and audit every disclosure/rotation.
  - Use a ref-backed client mutation owner and response generation so an older response
    cannot replace the newest displayed credential. After timeout, query/recover the
    known operation before enabling another rotation; do not issue a blind POST.
  - Add rapid double-click, two-tab/operator, multi-replica, lost-response, reversed-
    response-order, revoke/rotate race, and active-observation tests. Assert one secret
    revision, one snapshot/replay clear, one usable returned secret, and uninterrupted
    recovery by the referee.
  - Relevant code: `web/src/components/admin/KothOpsPanel.tsx`,
    `src/controllers/game/koth/api/admin.rs`, `src/controllers/game/koth/mod.rs`, and a
    new registered idempotent forward migration for observer operations/revisions.

- [ ] Make one-time player credential generation safe to retry and order.
  - A&D API-token rotation and server-generated SSH keys use only component-local
    busy state and have no request identity or expected credential revision. The A&D
    and KotH toolkits also mount separate `useAdToken` owners for the same team token.
    Rapid activation, another tab/team member, or an ambiguous-response retry can
    therefore commit multiple credentials while responses race to the reveal modal
    and browser storage.
  - The A&D token endpoint stores only the newest hash, and SSH generation stores only
    the newest public key. An older response arriving last can show/save a token or
    private key whose server-side credential was already replaced. The per-hill KotH
    capability rotation has the same stale-response risk and additionally clears that
    team's unsettled API score on every committed rotation. Serialization of roster or
    engine locks does not make those distinct random credentials equivalent.
  - Give generate/rotate requests an opaque operation ID and expected credential
    revision. Persist a short-lived encrypted response record (or another
    cryptographically safe recoverable result) so an exact authorized retry returns
    the same one-time plaintext; atomically reject a competing stale revision and clear
    dependent score/session state only on one real transition. Keep only the normal
    hash/public half after the bounded recovery window expires.
  - Own token rotation once per game/team across both toolkits, tabs where practical,
    and response generations. A response may update the modal/local storage only if it
    still matches the current operation and returned revision. After timeout recover
    that operation before allowing another rotation. Add a tight named credential-
    mutation policy as defense in depth without treating it as correctness.
  - Add double-click, hybrid-toolkit, two-tab/member, reverse-response, lost-response,
    reload-during-recovery, expired-recovery-record, revoke/rotate, and KotH unsettled-
    score tests. The one displayed credential must authenticate, and one user intent
    must cause one revision and one dependent-state clear.
  - Relevant code: `web/src/components/AdToolkitSections.tsx`,
    `web/src/components/AdGuideModal.tsx`,
    `web/src/components/KothGuideModal.tsx`,
    `web/src/components/KothChallengePanel.tsx`,
    `src/controllers/game/ad/token.rs`, `src/controllers/game/ad/ssh.rs`,
    `src/controllers/game/koth/tokens.rs`, `src/controllers/game/ad/mod.rs`,
    `src/controllers/game/koth/mod.rs`, and a new registered idempotent forward
    migration for credential operations/revisions.

- [x] Admit A&D bearer traffic before its PostgreSQL authentication query.
  - The global middleware recognizes a syntactically valid `ad_...` bearer and calls
    `api_token::authenticate` before applying the normal 150-request-per-minute
    identity/IP ceiling. The only earlier guard is the credential source-IP bucket,
    whose default burst is 30,000 requests with a 500-request-per-second refill (and
    whose configured minimum is still 3,000). Requests rejected by the later limiter
    have therefore already consumed a database round trip.
  - This affects valid, revoked, and random fixed-shape credentials on every `/api`
    path, not just routes that accept A&D authentication. A valid-token query joins
    participation/team state, checks the complete roster for banned or missing users,
    and may update `last_used_at_utc`; a stale exploit script or client retry loop can
    keep doing that work after its token is revoked. Rotating invalid token strings
    defeats any per-value fairness, while a deployment without Redis grants the large
    source burst independently on every replica. The hash index makes a miss cheaper,
    but there is no fail-fast authentication concurrency or query deadline protecting
    the pool.
  - Before PostgreSQL, classify only routes that actually support an A&D bearer and
    apply a cheap distributed bucket keyed by the fixed-size presented-token digest
    plus a source-IP backstop. This key is resource admission only and must never be
    treated as authenticated identity. Set the budget from supported batched-submit
    throughput, return `429` with `Retry-After` before acquiring a pool connection,
    and preserve the existing post-verification account/team limits.
  - Add a small process-level try-admission ceiling and absolute pool/query deadline so
    Redis failure, many source IPs, or a multi-replica burst cannot queue unbounded
    authenticators. If negative decisions are cached, cap entries/bytes and TTLs and
    never let a cached positive decision outlive token, roster, team, or participation
    revocation; prefer invalidation generations over a permissive time-only cache.
  - Give the documented exploit client pattern one in-flight request owner. Batch
    captured flags, honor `Retry-After`, use capped jitter only for transient failures,
    and stop automatically on `401`, `403`, event end, or token replacement instead of
    retrying a stale credential forever.
  - Add fixed-rate tests for valid, revoked, and rotating random tokens; a looping
    client; many identities behind one NAT; many source IPs and replicas; Redis loss;
    a slow/exhausted pool; roster suspension; and token rotation. Assert that denied
    traffic causes zero authentication queries, admitted query/concurrency counts stay
    bounded, revocation is immediate, normal 100-flag batches remain usable, and event
    submissions plus `healthz` stay responsive.
  - Relevant code: `src/middlewares/rate_limiter.rs`,
    `src/services/ad/api_token.rs`, `src/controllers/game/ad/mod.rs`,
    `src/controllers/game/ad/submit.rs`, `src/server.rs`,
    `web/src/utils/SubmitTemplates.ts`, and `docs/players/attack-defense.md`.

- [x] Collapse the admin dashboard's periodic query amplification into bounded SQL
  aggregates.
  - Every dashboard SWR inherits the global one-minute refresh. One dashboard refresh
    currently performs roughly 59 queries before the activity tables: 50 sequential
    participation counts plus review lookups, while the review and writeup tables add
    their own N+1 query chains.
  - Selecting the year trend loads every submission from the previous 365 days into
    Rust and buckets it in memory, then repeats that full transfer every minute.
  - Compute top games, ratings, and activity rows with a constant number of joined or
    grouped raw-SQL queries. Use PostgreSQL `date_trunc`/`GROUP BY` for the trend and
    return only the small bucket result.
  - Use an explicit, single-flight dashboard cadence or manual refresh, cache the
    aggregate briefly, stop hidden/idle polling, and put expensive reads behind the
    query-work admission policy.
  - Add real-PostgreSQL query-count tests and a large-submission fixed-rate dashboard
    test covering every range. The query count and returned row count must remain
    constant as games, reviews, writeups, and submissions grow.
  - Relevant code: `web/src/App.tsx`, `web/src/pages/admin/Dashboard.tsx`,
    `src/controllers/admin/mod.rs`, and `src/controllers/admin/anti_cheat.rs`.

- [x] Do not persist the complete SWR cache, secrets, and unbounded search history in
  browser storage.
  - The custom cache stores every SWR state indefinitely, serializes the complete map,
    and gzip-compresses it synchronously on the main thread every dirty period. A long
    admin/event session therefore grows without a count/byte/TTL bound and can create
    recurring UI stalls.
  - The provider also ignores response cache policy: even KotH capability responses
    marked `private, no-store` are copied into IndexedDB/localStorage. Logout clears
    only a subset of string keys, missing tuple keys and differently cased API paths,
    so account-specific challenge, container, monitor, and admin data can survive an
    account switch or browser restart.
  - Persist only a small allowlist of public, non-sensitive data. Never persist
    credentials, profile/team state, challenge details, container endpoints, or
    privileged feeds; honor `no-store` through explicit key metadata.
  - Namespace any retained public cache by schema/build version, enforce entry/byte
    and TTL bounds, clear user-scoped data atomically on login/logout/session change,
    and move unavoidable serialization off the UI thread.
  - Add shared-browser account-switch, logout/crash recovery, `no-store`, cache-bound,
    and main-thread responsiveness tests.
  - Relevant code: `web/src/utils/Cache.ts`, `web/src/App.tsx`,
    `web/src/hooks/useUser.tsx`, and `web/src/components/KothChallengePanel.tsx`.

- [x] Stop carrying a plaintext team API bearer across logout and account changes.
  - Rotating the A&D/KotH token automatically writes the complete bearer to
    `localStorage` under `ad-api-token-{gameId}`. The key contains no user,
    participation, or team identity, and logout clears neither it nor other keys with
    that prefix.
  - On a shared browser, account B can open the same game after account A logs out and
    reveal/copy account A's still-valid team credential. The bearer authenticates
    headless submit, target, and KotH-token workflows independently of the browser
    session and is intentionally valid until team rotation/revocation.
  - Keep one-time plaintext in memory or offer an explicit secure download/copy flow by
    default. If “remember this device” remains, require an explicit opt-in, namespace it
    by authenticated user plus participation, apply a short expiry, clear it atomically
    on logout/session/account/participation changes, and never render it until current
    server authorization proves the same participation.
  - Add shared-browser A-login/rotate/logout/B-login tests, crash/restart and expiry
    tests, rejected/suspended/moved participation tests, and assertions that logout
    clears the secret even when its network request fails.
  - Relevant code: `web/src/components/AdToolkitSections.tsx`,
    `web/src/hooks/useUser.tsx`, `src/controllers/game/ad/token.rs`, and
    `src/services/ad/api_token.rs`.

- [x] Bound the monitor event/submission queries so a client regression cannot request
  or search the entire history repeatedly.
  - Both live endpoints interpret `count=0` as “return every row” on tables that grow
    throughout an event. Their search path also materializes matching users/teams
    platform-wide before the event query, and the routes have no named query-work
    limiter.
  - Treat zero as the bounded default (or reject it), clamp every page to 1–100, cap
    and normalize search input, and express event-scoped joins/search in one indexed
    SQL query rather than building unbounded ID lists.
  - Cancel stale debounced client searches, admit the endpoints through the heavy-query
    policy, and reserve explicit bounded export endpoints for full-history needs.
  - Add large real-PostgreSQL tests for zero count, wildcard/long search, rapid typing,
    concurrent monitors, and fixed-rate polling with bounded memory and query time.
  - Relevant code: `web/src/pages/games/[id]/monitor/Events.tsx`,
    `web/src/pages/games/[id]/monitor/Submissions.tsx`,
    `src/controllers/game/scoreboard.rs`, and `src/controllers/game/routes.rs`.

- [x] Bound traffic-capture inventory reads before one monitor can saturate blocking
  workers, storage I/O, and PostgreSQL.
  - The game capture summary walks every challenge and participation directory, loads
    and sorts complete PCAP file lists merely to count them. The team view repeats the
    filesystem scan and then runs two serial database lookups per participation.
  - The file endpoint also returns the complete sorted directory without pagination.
    Unlike archive/flow work, these `spawn_blocking` listings have no dedicated
    semaphore or weighted admission, so repeated requests can occupy the blocking pool
    and rescan the same storage tree concurrently.
  - Maintain bounded capture counters/index metadata at write time, page directory and
    file reads with a strict cap, and fetch team display data with one joined raw-SQL
    query. Do not sort an entire directory to answer a count.
  - Put cold inventory reconciliation behind a small listing semaphore and the
    query/work admission policy, coalesce identical reads, and cancel stale client
    navigation requests.
  - Add real-filesystem/PostgreSQL fixed-rate tests with many challenges,
    participations, and PCAPs plus concurrent monitor tabs. Bound open tasks, rows,
    bytes, latency, memory, and storage operations while keeping `healthz` responsive.
  - Relevant code: `src/controllers/game/traffic.rs` and
    `src/controllers/game/routes.rs`.

- [x] Stop hidden anti-cheat tabs and drill-downs from repeatedly rebuilding complete
  evidence histories.
  - Mantine keeps inactive tab panels mounted by default. The anti-cheat page therefore
    fetches the complete flag-sharing incident ledger every ten seconds even while the
    operator is viewing Analysis, in addition to rebuilding the full analysis report
    every minute.
  - `cheatinfo` deliberately passes a null SQL limit and returns every matching row.
    `cheatreport` reloads the incident ledger, canonical solves, suspicion events,
    identity/IP evidence, and reconciliation state, then recomputes pair similarity in
    request memory. Its cost and response size grow throughout the event.
  - Opening an incident's supposedly immutable source review or a pair comparison adds
    another implicit one-minute poll because both local hook configs omit
    `refreshInterval` and inherit the application default. Evidence reconstruction can
    reload every canonical solve for a challenge or participation and every wrong
    attempt through the event. Pair comparison reloads both solve sequences and runs an
    O(A×B) LCS directly on the async request worker; unlike the evidence route, that
    endpoint is not even behind `Policy::Query`.
  - Unmount the inactive panel or disable its SWR key, and make the raw incident log a
    cursor-paginated/delta feed with a strict page cap. Revalidate only the visible
    page and reconcile from a stable incident ID after reconnect.
  - Materialize or briefly cache report versions when evidence changes, single-flight
    concurrent builds, use conditional responses for unchanged versions, and put cold
    report generation behind the weighted query-work policy. Stop or greatly reduce
    polling once evidence is sealed.
  - Treat incident evidence and a selected pair comparison as one-shot, open-modal
    reads with explicit `refreshInterval: 0` and manual refresh. Snapshot immutable facts
    at detection time or cache them by evidence/report version, and cap/page any source
    history that still must be reconstructed.
  - Admit both drill-down routes through weighted query work. Reuse the already
    materialized detector result where possible; otherwise enforce a small input bound
    and run CPU-heavy comparison off Tokio. Abort or generation-bind incident, pair, and
    fused-evidence requests so a late response cannot overwrite a newer selection.
  - Give both reads explicit status-aware retry policies, cancellation, hidden/offline
    suspension, jitter, and `Retry-After` handling instead of inheriting global SWR
    behavior.
  - Add request-count tests proving an inactive tab sends zero requests and an incident
    or pair left open for ten minutes sends only its initial request. Include rapid
    selection changes, persistent errors, maximum solve/wrong-attempt histories, plus
    large-ledger PostgreSQL/load tests for bounded rows, response bytes, build
    concurrency, event-loop latency, and many simultaneous monitors.
  - Relevant code: `web/src/pages/games/[id]/monitor/CheatCheck.tsx`,
    `web/src/components/monitor/CheatSubmissionLog.tsx`,
    `web/src/components/monitor/CheatInfo.tsx`,
    `web/src/components/monitor/SuspicionEvidenceReview.tsx`,
    `web/src/components/monitor/FusedEvidencePanel.tsx`,
    `web/src/utils/AntiCheat.ts`, `src/controllers/game/cheat.rs`,
    `src/controllers/game/cheat_evidence.rs`,
    `src/controllers/game/cheat_evidence_sources.rs`, and
    `src/controllers/game/routes.rs`.

- [x] Make browser-fingerprint collection failure-isolated so one optional probe cannot
  block login, registration, team join, and event join.
  - Fix the consent handoff in both authentication forms. `onAccept` calls
    `setAccepted(true)` and immediately invokes `executeLogin`/`executeRegister`; those
    closures still see the previous `accepted === false`, reopen Terms, and issue no
    fingerprint or account request. One acceptance must advance exactly one semantic
    authentication attempt without relying on a second click or a state-update race.
    Pass the granted decision explicitly into the current operation (or start from a
    generation-bound post-consent transition), and use the same synchronous in-flight
    owner as the form submit path.
  - Registration currently attaches `onRegister` to both `AccountView`'s form
    `onSubmit` and the explicit submit button's `onClick`. A pointer activation runs
    the click handler and then the form submission, so two `executeRegister` calls can
    pass the stale `disabled === false` render and concurrently fetch captcha and
    fingerprint material, hash a password, create/resend an account, and send mail.
    Remove the button click handler and make native form submission the only entry
    point; acquire the ref-backed operation owner synchronously before consent,
    captcha, or fingerprint work so click, Enter, and Terms acceptance all join the
    same generation.
  - Three probe batches attach `.catch(console.error)` to `Promise.all` and then
    destructure the result. If any browser API rejects, the catch returns `undefined`
    and destructuring throws, turning one unsupported or flaky signal into a fatal
    identity-flow failure whenever fingerprinting is enabled.
  - Resolve probes independently with typed fallbacks and bounded timeouts. Distinguish
    genuinely required challenge signals from optional entropy; report a retriable,
    user-visible error only when required evidence cannot be produced.
  - Share one cancellation-aware collection path across all four callers, without
    silently fabricating anti-cheat evidence or retrying fingerprint challenges in a
    tight loop.
  - Add single-click, Enter, click-plus-native-submit, and single-accept
    login/registration tests plus cases where each individual probe rejects, hangs, or
    is unsupported, required-signal failure, optional-signal failure, unmount, and
    successful retry. Assert one semantic activation starts one request and never
    reopens the modal from a stale render.
  - Relevant code: `web/src/utils/BrowserFingerprint.ts`,
    `web/src/pages/account/Login.tsx`, `web/src/pages/account/Register.tsx`,
    `web/src/pages/Teams.tsx`, and `web/src/pages/games/[id]/Index.tsx`.

- [x] Make HashPoW issuance stateless and keep the client from churning expired or
  cross-tab challenges.
  - The anonymous `/api/captcha/powchallenge` route reloads the live captcha settings
    from PostgreSQL and writes a fresh, unique `_HP_*` entry to both local cache and
    Redis for every request. The global 150-request-per-minute source limit bounds one
    IP but neither deployment-wide issuance work nor outstanding Redis keys; it also
    keeps issuing while `UseCaptcha` is false if the stored provider remains
    `HashPow`. Distributed traffic can therefore amplify cheap GETs into database,
    cache-connection, and five-minute key-retention pressure on the authentication
    path.
  - The browser stores one challenge in origin-wide `localStorage`, so login,
    registration, recovery, and multiple tabs can solve and consume the same one-use
    ID. Cross-tab replacement can enqueue another asynchronous solve in a worker that
    is still solving the previous challenge. Initial fetch failure leaves the widget
    saying “computing” forever, while a completed proof has no expiry wake-up and can
    be submitted after its server entry has disappeared, causing avoidable failure and
    reissuance.
  - Issue a self-contained challenge containing a random ID/value, issued/expiry time,
    difficulty, and captcha-policy revision under a server HMAC. Do not persist
    anything at issuance. On verification, validate the signature, expiry, revision,
    and proof first, then atomically `SET NX` only a short-lived consumed-ID marker;
    this preserves one-use semantics while making key creation require paid work.
  - Refuse issuance unless captcha is enabled with the HashPoW provider, serve the
    policy from an invalidated in-memory snapshot, and add a tight named per-source
    issuance policy plus a deployment-wide admission budget, `Retry-After`, keyspace
    metrics, and an explicit Redis memory/eviction bound as defense in depth.
  - Keep one abortable challenge fetch and one worker generation per form. Use
    tab-scoped state, or a `BroadcastChannel` lease if work is deliberately shared;
    terminate superseded worker work, schedule refresh before expiry, clear stale
    results, and retry only transient issuance failures with capped jitter and an
    explicit retry control. Do not poll for challenges.
  - Add multi-tab, simultaneous mount, fetch-failure, slow/stale response,
    policy-change, proof-expiry, replay, unmount, disabled-captcha, persistent 429, and
    invalid-signature tests. A distributed fixed-rate issuance test must bound
    PostgreSQL queries, Redis keys/commands, worker jobs, and authentication latency.
  - Relevant code: `web/src/components/HashPow.tsx`,
    `web/src/utils/PowWorker.ts`, `src/controllers/info.rs`,
    `src/services/captcha.rs`, `src/services/cache.rs`, and
    `src/middlewares/rate_limiter.rs`.

- [x] Bound public team-signature verification and anchor its trust decision.
  - Anonymous `POST /api/team/verify` accepts both the Ed25519 public key and team token
    from the caller under the generic JSON body limit. It Base64-decodes `publicKey`
    before checking for the required 32 bytes and similarly decodes the unbounded
    signature suffix before checking for 64 bytes. A client loop can therefore turn
    each single global request token into megabyte-scale body buffering, decoding, and
    allocation before a cheap rejection; there is no proof-specific or aggregate CPU/
    byte admission.
  - Success currently proves only that a caller-controlled key signed
    `RSCTF_TEAM_{caller-controlled-id}`. It does not load a game key, team,
    participation, event window, or revocation state. That is valid only as a low-level
    verifier when the consumer already trusts the public key out of band; treating the
    200 as platform team authorization lets anyone generate a key and authenticate an
    arbitrary numeric team ID.
  - Put a route-specific body limit near the exact encoded envelope and reject length,
    alphabet/padding, delimiter count, and bounded positive ID before decoding or
    allocating. Add fail-fast distributed per-source and aggregate verification/byte
    admission with a small concurrency ceiling and `Retry-After`; do not let a burst of
    generic-body-sized requests buffer before admission.
  - Make the contract explicit. If it is an authorization endpoint, accept a game/
    credential identifier, load the canonical live public key and participation policy
    on the server, and return a scoped principal/result without accepting a trust root
    from the request. If it remains a stateless cryptographic utility, rename/document
    it accordingly and ensure no internal or published integration interprets it as
    current membership proof.
  - Add exact/missing/oversized/malformed Base64, extra delimiters, negative/overflow ID,
    valid trusted and attacker-generated key, deleted/rejected/ended participation when
    authoritative, rapid-client, large-body, multi-source, and fixed-rate tests. Assert
    bounded request bytes, allocations, verifier jobs, latency, and no impact on event
    submissions or `healthz`.
  - Relevant code: `src/controllers/team/mod.rs`,
    `src/controllers/team/models.rs`, `src/server.rs`,
    `src/middlewares/rate_limiter.rs`, game public-key/participation resolution, and the
    generated `teamVerifySignature` client contract in `web/src/Api.ts`.

- [x] Close the pre-start and participation-status challenge/standings metadata leak.
  - `GET /api/game/{id}` currently includes enabled challenge titles, types, categories,
    and scores for an accepted participant before kickoff. In practice mode, any
    participation row—including pending or rejected—passes the metadata gate.
  - The same projection does not apply division `VIEW_CHALLENGE` permissions, so it can
    reveal challenges that the caller's division is not allowed to view.
  - Its displayed `teamCount` counts pending, rejected, and suspended participation
    rows as joined teams, unlike the accepted-participant semantics used by playable
    surfaces; align the count and document which review states are intentionally shown.
  - The standard scoreboard rejects pre-start reads, but the public A&D and KotH
    scoreboards/timeline do not apply that boundary and can expose specialized
    challenge and roster metadata early. KotH token/state eligibility checks only that
    a challenge is enabled, not that its event is in the playable window; A&D State has
    the same accepted-participant/time-policy split.
  - Apply the end boundary too. A&D State can continue returning service addresses,
    ports, and current flags after close, while token-authenticated A&D/KotH target reads
    have no event-window check. Strip operational secrets/endpoints as soon as play ends
    and expose only an intentionally archival projection afterward.
  - Reuse the authoritative accepted/start/permission policy used by the challenge
    details surface across every engine. If pre-event defense preparation is a real
    product requirement, model it as an explicit operator-controlled warmup policy
    rather than treating an enabled challenge as authorization.
  - Add negative tests for anonymous, non-member, pending, rejected, suspended,
    accepted-before-start, wrong-division, hidden-game, and accepted-after-start access.
  - Relevant code: `src/controllers/game/play.rs`, `src/controllers/game/mod.rs`,
    `src/controllers/game/ad/scoreboard.rs`, `src/controllers/game/koth/mod.rs`,
    `src/controllers/game/ad/targets.rs`, `src/controllers/game/koth/timeline.rs`, and
    `src/controllers/game/koth/eligibility.rs`.

- [x] Collapse control-plane per-card and fixed-interval polling into bounded owners.
  - Every queued/building `ChallengeEditCard` currently starts its own two-second
    interval, and every interval invokes the same full challenge-list `mutate`. The
    request schedule therefore grows with the number of simultaneous builds instead
    of remaining constant.
  - Keep one stable poll owner in the parent, schedule the next refresh only after the
    previous one settles, and stop it when no visible build is in flight.
  - Apply the same single-flight rule to the challenge-detail build-log interval and
    worker-list interval so a slow control-plane response cannot accumulate overlap.
    The worker console currently starts an async full-inventory read every ten seconds
    regardless of visibility, connectivity, or whether the previous request settled;
    manual Refresh can overlap it, and an outage creates another error notification on
    every tick. `Policy::Query` limits concurrent damage, but every admitted request
    still fetches and serializes every `WorkerNodes` row with no page or response cap.
  - Completion-schedule the worker refresh after its prior request settles, share the
    same promise with manual refresh, pause it while hidden/offline, and use capped
    jittered backoff plus `Retry-After` without repetitive notifications. Prefer a
    pushed/versioned heartbeat delta; otherwise page/cap the full inventory projection
    and return only fields needed by the list.
  - On `/admin/builds`, poll only the in-progress set while work exists; do not fetch
    both the unbounded in-progress query every two seconds and the last 200 historical
    builds every five seconds forever. Refresh history once when a build becomes
    terminal, and cap/index the server-side in-progress query.
  - Set the page's inactive Images tab to unmount or explicitly disable its keys. It
    currently polls image inventory and storage every 30 seconds while the operator is
    viewing build logs; each inventory refresh pings Docker, lists all images and
    containers, loads database ownership/reference rows, and then inspects every owned
    image serially. Cache/share a bounded inventory snapshot and limit daemon
    concurrency when that tab is active.
  - Add fake-timer/request-count tests proving that 1 and 100 building cards issue the
    same number of list requests and that the inactive Images tab issues none, plus
    slow responses longer than ten seconds, timer/manual overlap, hidden/offline tabs,
    database and daemon outages, large worker/image sets, unmount, and terminal states.
  - Relevant code: `web/src/components/admin/ChallengeEditCard.tsx`,
    `web/src/pages/admin/games/[id]/challenges/Index.tsx`,
    `web/src/pages/admin/games/[id]/challenges/[chalId]/Index.tsx`, and
    `web/src/pages/admin/workers.tsx`, `web/src/pages/admin/builds.tsx`,
    `web/src/components/admin/BuildImagesPanel.tsx`,
    `src/controllers/admin/builds.rs`, `src/controllers/workers.rs`,
    `src/services/worker_store/nodes.rs`, and
    `src/controllers/admin/builds/images.rs`.

- [x] Turn manual and bulk image rebuilds into idempotent bounded jobs before Docker.
  - All three manual-build controls use component-local React state as the only
    duplicate guard and call the same unadorned synchronous POST. A rapid activation
    before rerender, another surface/tab, or another organizer can submit the same
    challenge concurrently while the UI reports that work was merely “enqueued.”
  - The server actually performs the complete pull/BuildKit operation inside the HTTP
    request. A process-local one-slot semaphore and blocking PostgreSQL advisory lock
    serialize it, but the manual `Requested` path does not recheck or coalesce an
    identical request after acquiring the lock. Every queued duplicate can therefore
    run the same slow image build again, while the active operation retains a pool
    connection for the duration of external Docker I/O.
  - Bulk rebuild is also a synchronous loop with no named route admission. Duplicate
    calls wait in an unbounded process-local semaphore queue; one waiter per replica
    can additionally hold a pool connection on `pg_advisory_lock` while the winner
    keeps its batch connection across every candidate and nested image build. The
    advisory locks protect publication order, but they do not bound accepted HTTP
    work or make retries idempotent.
  - Atomically enqueue a durable build job keyed by challenge, immutable build
    fingerprint, and client operation ID before touching blob storage or Docker. Keep a
    unique active-job constraint so an exact retry/lost response returns the existing
    job ID/status; allow an intentional post-terminal rebuild only as a new confirmed
    operation. Have a bounded fair worker own Docker, deadlines, cancellation, and
    progress, and return 202 immediately.
  - Use nonblocking distributed admission for bulk creation and return/coalesce the
    active batch instead of waiting on `pg_advisory_lock`. Claim a bounded candidate
    page atomically, cap result messages, and release the database connection between
    claims; route-level `Policy::Concurrency` is useful defense in depth but cannot
    replace durable idempotency and queue bounds.
  - Share one ref-backed client promise per challenge/batch across button surfaces,
    keep controls disabled until the job identity is known, and reconcile that job
    after a lost response instead of submitting another build.
  - Add rapid double-click, card/detail/audit-modal, multi-tab, two-organizer,
    multi-replica, lost-response, cancellation, daemon timeout, maximum-candidate, and
    pool-pressure tests. Assert one Docker operation per accepted job, a bounded queue
    and waiter/connection count, stable job recovery, fair event runtime work, and a
    deliberately requested later rebuild that still works.
  - Relevant code: `web/src/components/admin/ChallengeEditCard.tsx`,
    `web/src/components/admin/ChallengeAuditModal.tsx`,
    `web/src/pages/admin/games/[id]/challenges/Index.tsx`,
    `web/src/pages/admin/games/[id]/challenges/[chalId]/Index.tsx`,
    `src/controllers/edit/challenges/audit.rs`,
    `src/controllers/edit/builds.rs`, `src/controllers/admin/builds.rs`,
    `src/utils/single_flight.rs`, and a new registered idempotent forward migration.

- [x] Single-flight deterministic variant generation before scanning or launching work.
  - The organizer button uses React state as its only duplicate guard. A rapid second
    activation before rerender, another tab, another manager, or another replica can
    submit the same plain POST concurrently; the route has no heavy-work policy,
    event-scoped lease, idempotency key, or durable job claim beyond the broad global
    request ceiling.
  - Every request first materializes the complete enabled per-participation
    challenge-by-accepted/suspended-team target matrix. Each stale request then waits
    inside its target loop on the same two-slot process-local semaphore and runs every
    generator twice for as long as 30 seconds per run. The unique index and final
    `ON CONFLICT DO NOTHING` prevent duplicate ledger rows only after all container
    work was paid, so duplicate requests can retain unbounded queued HTTP work, starve
    build-time generator validation, and return `generated: 0` minutes later.
  - Claim one event-scoped generation job across replicas immediately after
    authorization and before `load_targets`; return the existing job/progress or a
    typed 409/429/503 with `Retry-After` instead of queuing another scan. Keep a small
    ref-backed client request owner for immediate double clicks, but do not treat the
    disabled button as the authority.
  - Move the long run to a durable bounded worker with progress, cancellation, a total
    deadline, fair admission versus generator-image validation, and capped target
    batches. Atomically claim each `(game, challenge, participation, revision)` before
    launch (or recheck it under the job lock) so one revision causes exactly one
    deterministic two-run validation across processes; release/recover claims after a
    crash without allowing a stale request to regenerate already-frozen work.
  - Coalesce the manual anti-cheat context derivation with its scheduled reconciliation
    owner as well. The adjacent admin action has the same React-state race and no named
    route admission, while each duplicate holds a transaction and rescans the event's
    VPN DNS, peer, flag-transport, finding, relationship, cheat, and suspicion rows
    before idempotent conflicts discard duplicate inserts.
  - Add rapid double-click, multi-tab, two-manager, multi-replica, lost-response,
    worker-crash/recovery, event-start boundary, persistent failure, and maximum
    target/telemetry tests. Assert one target snapshot and one two-run generator pair
    per claimed revision, one derivation sweep, bounded queued jobs/memory/pool waiters,
    fair validation latency, and useful progress/retry behavior.
  - Relevant code: `web/src/pages/admin/games/[id]/Info.tsx`,
    `src/controllers/edit/event_security.rs`, `src/controllers/edit/mod.rs`,
    `src/services/event_security/variants.rs`,
    `src/controllers/admin/anti_cheat.rs`,
    `src/services/event_security/fusion.rs`, and a new registered idempotent forward
    migration for durable job/claim state.

- [x] Make “Save and roll out” idempotent per workload revision before advancing a
  worker generation.
  - The button's React state is only a same-render usability guard. A rapid second
    activation, another tab/operator, or a retry after a lost response can send the
    same saved workload identity more than once to an otherwise unadorned POST.
  - The cross-replica definition lock serializes those calls but does not coalesce
    them. Up to four admitted definition operations per replica can retain PostgreSQL
    transactions while waiting for the same advisory lock, and later requests queue
    in memory. Once the first request releases the lock to begin its 90-second
    convergence wait, the next duplicate takes the lock and repeats the full target
    scan.
  - `update_workload_definition` unconditionally increments `generation` and resets
    observed readiness even when the specialized definition is byte-for-byte the
    workload revision just applied. A duplicate therefore recreates every stateless
    service again, makes the first request observe its generation as stale, and starts
    another overlapping database convergence loop. Serialization preserves ordering
    but still turns an ordinary retry into repeated event disruption and control-plane
    load.
  - Atomically claim one durable rollout job keyed by game, challenge, immutable
    workload identity, and a client operation ID before loading targets. Exact retries
    must return the same job/result; a concurrent caller should observe the active job
    or receive a typed 409/429/503 with `Retry-After`, never wait on the advisory lock.
    Return 202 and let a bounded owner perform rollout and convergence independently of
    the initiating HTTP connection.
  - Make each target update idempotent too: persist the applied/desired rollout
    revision or compare the complete specialized definition and reservations under the
    generation CAS, returning `AlreadyCurrent` without incrementing the generation.
    Expose a separately named, confirmed force-restart operation if deliberately
    replacing an identical stateless revision is required.
  - Use one ref-backed client promise through save, job creation, and status recovery;
    abort superseded route work and reconcile the known operation after a lost
    response. Add route-level heavy-work admission only as defense in depth.
  - Add rapid double-click, multi-tab, two-operator, multi-replica, lost-response,
    cancellation, worker-timeout, and force-restart tests. Assert one target scan, one
    generation advance per workload/revision, one bounded convergence owner, no
    advisory-lock waiter pool growth, and stable recovery of the original result.
  - Relevant code:
    `web/src/pages/admin/games/[id]/challenges/[chalId]/Index.tsx`,
    `src/controllers/edit/challenges/workload.rs`,
    `src/services/worker/container_backend.rs`,
    `src/services/worker_store/workloads.rs`,
    `src/services/challenge_workloads.rs`, `src/utils/single_flight.rs`, and a new
    registered idempotent forward migration for rollout jobs/revisions.

- [x] Coalesce manual A&D/KotH “Ensure containers” with scheduled reconciliation.
  - The console reports “reconcile queued,” but the POST actually awaits a synchronous
    full-game pass. Component-local `busy` state cannot stop a same-render activation,
    another tab/operator, or a scheduler pass from starting the same work.
  - Every duplicate reloads all enabled A&D challenges and accepted participations,
    walks the team-by-challenge grid, rechecks game/team/challenge eligibility several
    times per pair, and calls the external runtime to inspect every published managed
    service. It also repeats peer allocation/VPN reconciliation and a complete KotH
    hill ensure pass. The per-pair publication locks correctly prevent duplicate
    containers, but they only serialize followers after those requests have already
    been accepted and their full scans allocated.
  - The provisioning semaphore bounds active retained PostgreSQL transactions to four
    per replica, not the number of waiting HTTP requests. Repeated client requests can
    therefore build an unbounded in-memory waiter queue, occupy those pool slots while
    contending across replicas, duplicate runtime/API traffic, and compete with the
    round scheduler that calls the same reconciler on the event-critical path.
  - Make the manual endpoint atomically upsert one durable event-scoped reconcile
    generation and return 202/existing progress immediately. A bounded lease-owning
    worker should coalesce manual and scheduled triggers, merge a newer requested
    generation into the active pass, and preserve the existing per-pair publication
    locks as the final creation fence. Reject or return the active job rather than
    queueing behind a blocking advisory lock.
  - Select and claim bounded pages of missing/stale pairs instead of materializing and
    probing the complete Cartesian grid per request. Run runtime checks with explicit
    concurrency/deadline limits; reconcile VPN state and KotH hills once per effective
    topology/roster change, not once per duplicate trigger. Keep round readiness able
    to await the claimed reconcile generation without launching a parallel sweep.
  - Add a ref-backed client single-flight guard and show real queued/running/progress
    state. Test rapid clicks, multiple tabs/operators/replicas, overlap with a scheduled
    round, large team/challenge grids, slow/dead runtimes, worker crash/recovery, and a
    roster change during a pass. Assert one effective scan per generation, bounded
    waiters/connections/runtime calls, one VPN/KotH reconciliation, and no delayed or
    skipped scoring round.
  - Relevant code: `web/src/pages/admin/games/[id]/AdOps.tsx`,
    `src/controllers/edit/ad/provision.rs`,
    `src/controllers/game/koth/capture.rs`,
    `src/services/ad/vpn/mod.rs`, `src/services/ad/vpn/reconcile.rs`,
    `src/services/cron/scheduler.rs`, `src/services/cron/round_finish.rs`,
    `src/utils/single_flight.rs`, and a new registered idempotent forward migration for
    reconcile jobs/generations.

- [x] Make player and operator A&D resets idempotent and independently load-limited.
  - Both clients call a synchronous destroy/create endpoint but announce “Reset
    queued.” The player has only component-local `resetting` state, and the operator
    has no per-service in-flight state after its confirmation closes, so a rapid
    activation, another tab, or a retry after an ambiguous response can submit the
    same destructive intent again.
  - Per-service local/advisory locks serialize reset requests but do not make them the
    same operation. An operator duplicate that eventually acquires the lock destroys
    the replacement produced by the first request and creates another one. The player
    cooldown prevents that only when it is positive and the first publication
    completed; zero minutes is explicitly accepted by the UI/schema, and the
    operator route intentionally bypasses the cooldown.
  - Even a player request that will receive a cooldown 429 first queues on the local
    coalescing mutex, acquires a provisioning permit and retained PostgreSQL
    transaction, and repeats authorization/model reads. Neither reset route has the
    named container-operation policy. Fan-out over services/users can therefore fill
    the four provisioning slots and an unbounded waiter queue while repeated allowed
    resets churn Docker/Kubernetes, capture teardown, checker reset accounting, and
    VPN reconciliation on the scoring path.
  - Reserve a durable reset operation before the provisioning lock, keyed by service
    and opaque client operation ID and bound to the expected published backend/reset
    generation. Exact retries and lost-response recovery must return the original
    job/result; a delayed intent for an already-replaced backend must be a no-op or
    precondition conflict, never reset the new container. Pass that stable identity to
    `ContainerSpec.operation_id` so a crash after runtime creation can adopt or reap
    the exact replacement instead of orphaning it.
  - Enforce one active reset per service and a hard per-team/per-event plus
    deployment-wide container-operation budget before checking out a retained pool
    connection. A configured zero gameplay cooldown must not disable this
    infrastructure safety floor. Return 202 for accepted work and 409/429/503 with
    `Retry-After` for active/overloaded work; add `Policy::Container` only as defense in
    depth, not as idempotency.
  - Give both clients a synchronous ref-backed guard, display the real job state, and
    recover the known operation after timeout rather than issuing a new reset. Add
    rapid double-click, multi-tab, two-member/operator, positive- and zero-cooldown,
    lost-response/cancellation, multi-replica, runtime-failure, and scoring-overlap
    tests. Assert one retirement, replacement, capture transition, checker downtime
    record, and VPN reconciliation per operation with bounded waiters/pool usage.
  - Relevant code: `web/src/components/AdChallengePanel.tsx`,
    `web/src/pages/admin/games/[id]/AdOps.tsx`,
    `src/controllers/game/ad/mod.rs`, `src/controllers/game/ad/scoreboard.rs`,
    `src/controllers/edit/ad/mod.rs`, `src/services/container.rs`,
    `src/services/ad/engine/`, `src/utils/single_flight.rs`,
    `src/middlewares/rate_limiter.rs`, and a new registered idempotent forward
    migration for reset jobs/generations.

- [x] Replace live scoring/challenge toggles with idempotent desired-state commands.
  - `ScoringPause` flips the stored value without a request body. If its successful
    response is lost, an operator retry resumes scoring; a rapid duplicate or another
    organizer looking at stale state does the same. A duplicated resume can pause the
    event again, and the client cannot distinguish either result from its original
    intent.
  - The A&D/KotH challenge endpoint has the same toggle contract around a much slower
    transition. Requests are serialized, but the first disable commits, clears engine
    control, disconnects BYOC, and destroys challenge containers; a queued duplicate
    then reads `false` and re-enables the database row without recreating what the first
    request removed. The event can therefore show an enabled hill/service with no live
    runtime after an ordinary double activation or lost-response retry.
  - Accept an explicit desired `paused`/`enabled` value plus the control/configuration
    revision (or `If-Match`) that the operator observed. Under the existing game and
    runtime-transition fences, return success without side effects when the resource is
    already in that desired state, reject a conflicting stale revision, and increment
    the revision only for a real transition. Resume must extend a round exactly once;
    disable cleanup must run exactly once; enabling should claim the normal bounded
    reconcile generation when runtime provisioning is required.
  - Keep one ref-backed mutation owner per control, render pending intent rather than
    the stale snapshot, and reconcile the authoritative state after timeout before
    permitting another command. Do not infer a new intent by inverting a value cached
    before the request.
  - Add rapid double-click/keyboard, multi-tab, two-organizer, stale-view, lost-response,
    and multi-replica tests for pause, resume, disable, and enable. Assert final state
    matches the explicit intent, one round extension/cleanup/reconcile occurs, and an
    exact replay is a no-op with the original result.
  - Relevant code: `web/src/pages/admin/games/[id]/AdOps.tsx`,
    `src/controllers/edit/ad/mod.rs`,
    `src/services/ad/engine/`, `src/services/challenge_workloads.rs`, and the
    event/challenge configuration revision persistence.

- [x] Split challenge-build status polling from source-archive inspection.
  - While a reviewed challenge is `Queued` or `Building`, `ChallengeAuditModal`
    requests `auditmeta` every two seconds even though only build status/log can
    change. Each request reloads as much as 72 MiB from blob storage, then reparses a
    ZIP with as many as 2,048 entries on a blocking worker and resends the YAML, file
    tree, and previews.
  - The modal's Download Archive button opens
    `/api/edit/games/{id}/challenges/{cId}/auditarchive`, but no router entry or handler
    owns that path. It therefore downloads a typed 404 or the SPA shell instead of the
    retained source. Add a manager/admin-authorized, `no-store`, size-bounded streaming
    download (or an equally scoped short-lived signed URL), preserve a safe filename,
    and test negative authorization plus the exact case-sensitive route contract.
  - The process-local two-slot parser semaphore is acquired only after the complete
    archive has been loaded. Several ordinary admin tabs can therefore retain large
    blobs concurrently before being rejected, repeatedly consume storage/CPU and
    response bandwidth, and make the latest useful inspection return 503.
  - Poll a compact build-status resource (or push build transitions) and fetch the
    immutable audit projection only once per archive hash. Single-flight and
    size-bound that projection, acquire weighted admission before loading bytes, cap
    total preview/response bytes, and suspend/cancel modal work while hidden, closed,
    offline, or on a terminal error.
  - Add fake-timer request-count tests plus 72-MiB/many-entry archive and concurrent-
    admin load tests. One build may generate repeated compact status reads but only
    one archive load/parse per immutable hash, with bounded pre-admission memory.
  - Relevant code: `web/src/components/admin/ChallengeAuditModal.tsx`,
    `web/src/pages/admin/games/[id]/pending.tsx`,
    `src/controllers/edit/challenges/audit.rs`, `src/controllers/edit/mod.rs`,
    `src/server.rs`, and `src/utils/upload.rs`.

- [x] Make challenge ZIP/Git imports admission- and cancellation-safe before creating
  temporary checkouts.
  - Public/trusted ZIP imports create `rsctf-import-*`, hand extraction to an
    uncancellable `spawn_blocking` task, and remove the directory only after the
    awaited import returns. Dropping the HTTP future during extraction/import skips
    that trailing cleanup; the blocking task can continue writing into a directory no
    longer owned by a request. Repeated client cancellation can therefore leave
    expanded archives in `/tmp` without consuming the ten-pending-challenge quota.
  - `ImportFromGitHub` does not take either challenge-import semaphore. Every duplicate
    immediately creates a distinct temp directory and a `git clone` process for as long
    as 120 seconds before any game-level import serialization. Its 64-MiB tree check is
    performed only after download and deliberately skips `.git`, so it neither limits
    paid network/disk work nor a large shallow object pack. Concurrent tabs/retries can
    multiply subprocesses, egress, temp storage, scans, synchronous image builds, and
    database updates.
  - Move ZIP extraction and Git synchronization into a supervised bounded import job
    that owns its temporary directory independently of the HTTP future. Acquire local
    weighted and deployment-wide/per-event admission before temp creation or process
    launch, return 202/job identity, and reject overload immediately with
    `Retry-After`. The worker must enforce one total deadline and a filesystem quota
    that includes VCS metadata, subprocess descendants, extracted bytes, and build
    staging.
  - Atomically coalesce an operation by event, source kind, normalized URL/ref/subpath
    or archive digest, resolved commit, and client operation ID. An exact retry should
    recover the same import result; one source revision must be scanned/imported and
    enqueue each immutable build at most once across replicas.
  - Use an RAII/supervisor cleanup owner that runs after success, error, panic,
    cancellation, or shutdown; mark active directories and sweep abandoned/expired
    workspaces on startup under a total temp-storage budget. Do not rely on code after
    an `.await` for cleanup, and keep blocking extraction inside the owned task so a
    dropped join handle cannot orphan its output.
  - Add disconnect-during-upload/extraction/clone/import/build, rapid retry, multi-tab,
    multi-replica, 120-second Git timeout, oversized `.git` pack, ZIP bomb, process
    crash/restart, and temp-budget tests. Assert bounded jobs/processes/bytes, one
    source-revision import/build, no abandoned directories, and responsive event
    traffic/`healthz`.
  - Relevant code: `web/src/Api.ts`, `src/controllers/edit/test_container.rs`,
    `src/controllers/edit/transfer.rs`, `src/services/git_sync/git.rs`,
    `src/services/git_sync/mod.rs`, `src/utils/upload.rs`, and a new registered
    idempotent forward migration for import jobs/source revisions.

- [x] Admit bulk ZIP exports before loading their database rows and attachment blobs.
  - One export can collect as many as 2,048 attachment entries and 128 MiB of blob data
    in memory, with per-division and per-challenge query loops. `GAME_EXPORT_SLOTS` is
    acquired only after all of that I/O and allocation, so a request rejected as “busy”
    has already paid almost the full cost and concurrent requests can each retain the
    maximum payload outside the advertised two-export bound.
  - The export button passes an already-started request promise to `downloadBlob`; a
    rapid duplicate action can enter this path twice before React renders the disabled
    state. The process-local ZIP semaphore also does not bound aggregate work across
    replicas.
  - “Download all writeups” is another unguarded bulk-export trigger: every click opens
    a new tab/request. Before acquiring its own two-slot semaphore, the handler loads all
    writeup participations and performs two serial lookups per row, so even requests that
    ultimately receive “busy” can amplify database work with event size.
  - Merely opening the writeup review page also returns every event writeup and calls
    `writeup_for` serially per row, including redundant file/team/game lookups, before
    rendering every card. Keep this bounded list read separate from the explicitly
    admitted full archive.
  - After authorization, acquire deployment-wide weighted admission and a local memory
    permit before any export query or blob read. Batch the relational projection, stream
    bounded attachments into the archive or a bounded temporary/object-store sink, and
    hold the permit through response ownership. Reject overload early with
    `Retry-After`; never load bytes merely to discover no ZIP slot is available.
  - Use one request-factory/single-flight download owner for both buttons, batch the
    writeup source projection into one bounded query, and consider coalescing identical
    `(game, configurationVersion)` exports without sharing across authorization
    boundaries. Page the review list and lazy-load only the selected document.
  - Add rapid double-click, full-local-slot, multi-replica, maximum-file/byte, slow-blob,
    slow-client, cancellation, and storage-error tests. Assert that rejected work reads
    zero blobs and that retained bytes, queries, ZIP jobs, and response tasks stay within
    the declared budget while `healthz` remains responsive.
  - Relevant code: `web/src/pages/admin/games/[id]/Info.tsx`,
    `web/src/pages/admin/games/[id]/Writeups.tsx`,
    `web/src/utils/ApiHelper.tsx`, `src/controllers/edit/transfer.rs`,
    `src/controllers/edit/mod.rs`, and `src/controllers/admin/mod.rs`.

- [x] Stream and globally admit retained A&D snapshot downloads before allocating their
  complete bodies.
  - Both the player and admin retained-snapshot paths call `load_bounded` and materialize
    as much as 128 MiB in one `Vec` before constructing the response. The plain anchor
    download buttons have no synchronous in-flight guard, so a double click or repeated
    activation starts independent full reads and retains each body for a slow client.
  - `Policy::Container` is useful abuse defense, but its six-request per-identity burst is
    unweighted by bytes and is not a process/deployment memory budget. Six maximum-size
    retained blobs can therefore account for roughly 768 MiB for one identity, and
    multiple authorized identities multiply that pressure. The persisted branch also
    bypasses the semaphore used to bound live container export work.
  - Authorize first, read stored size without loading the body, then acquire a
    deployment-wide byte/work budget and a small local stream permit. Proxy with
    `stream_range` (including range/resume, length, ETag, and cancellation), or issue a
    short-lived authorized object-store URL when policy permits; hold local admission
    until the response body is dropped. Keep the 128-MiB object bound and reject overload
    before storage I/O with a typed response and `Retry-After`.
  - Give the player and admin controls one immediate ref-backed/single-flight owner per
    snapshot and expose progress/cancel/retry state. This is a usability guard only;
    backend byte admission remains authoritative for multiple tabs, clients, and users.
  - Add rapid double-activation, six-full-size-request, many-identity/multi-replica,
    local and S3, range resume, slow-client, disconnect, and revoked-access tests. Assert
    bounded resident bytes and streams and zero blob reads for work rejected by admission.
  - Relevant code: `web/src/components/AdChallengePanel.tsx`,
    `web/src/pages/admin/games/[id]/AdOps.tsx`,
    `src/controllers/game/ad/scoreboard.rs`, `src/controllers/edit/ad/mod.rs`,
    `src/storage/blob_storage.rs`, and `src/middlewares/rate_limiter.rs`.

- [x] Bound and cancel live A&D filesystem-diff/file inspection.
  - Opening the forensics modal calls Docker's complete container-changes API and
    returns every changed path without an entry, path-length, response-byte, runtime,
    or concurrency bound. A participant can create a very large change set in its own
    service, making an ordinary operator view allocate, serialize, transfer, tree-build,
    and render attacker-sized data; the Changes, Snapshots, and SnapshotDiff routes can
    repeat the same full daemon walk independently.
  - Selecting a changed path runs `cat <path>` inside the live container. The shared
    exec collector rejects output only after it has accumulated roughly 1 MiB and has
    no command deadline. A participant-controlled FIFO/device/stalled file can retain
    the Docker exec and HTTP task indefinitely, while a large/streaming file consumes
    work that the UI incorrectly labels as a 256-KiB truncated preview. The response
    always reports `truncated: false`.
  - `FileDetail` only marks an old promise cancelled. Rapid path selection, Back, hash
    navigation, modal close, or game change does not abort the HTTP/runtime operation,
    so obsolete file reads overlap and still consume server and daemon capacity even
    though their responses are discarded.
  - Put filesystem-change and file-read work behind deployment-wide weighted plus
    per-container admission, an absolute daemon/exec deadline, and cancellation that
    actually terminates or safely detaches/reaps the runtime operation. Apply
    `Policy::Container` as defense in depth and return typed overload/timeout responses
    with `Retry-After`.
  - Cap/sanitize change entries and aggregate path/response bytes, page or lazily expand
    the tree, and cache/single-flight one result by exact backend identity/generation
    for a short interval. Read an exact path through a byte-limited runtime primitive,
    return at most 256 KiB plus truthful size/binary/truncated metadata, and never feed
    an oversized body to syntax highlighting.
  - Abort the prior Axios request on path/game/service/modal changes, keep one current
    request generation, and expose explicit timeout/too-large/retry states. Add huge
    change-set, long path, regular 1-GiB file, FIFO/device/infinite stream, slow daemon,
    rapid-selection, close/unmount, multi-admin, and container-replacement tests.
    Assert bounded runtime tasks, bytes, response size, highlighting work, and prompt
    cancellation while scoring and `healthz` remain responsive.
  - Relevant code: `web/src/pages/admin/games/[id]/AdOps.tsx`,
    `src/controllers/edit/ad/mod.rs`, `src/services/container.rs`,
    `src/services/container/docker.rs`, `src/services/container/backend.rs`, and
    `src/middlewares/rate_limiter.rs`.

- [x] Quarantine poison worker workloads instead of retrying and starving the queue.
  - The singleton worker reconciler selects the 256 oldest due workloads every 500
    milliseconds. If a persisted spec cannot deserialize into
    `ValidatedWorkloadSpec`, `command_for` logs the error and `continue`s without
    changing the row or its retry time.
  - One corrupt or version-incompatible row is therefore selected and logged twice a
    second forever; 256 such rows permanently fill the ordered batch, hammer the
    database/log pipeline, and prevent every valid event workload behind them from
    reaching an otherwise healthy worker.
  - Validate and normalize definitions on every write, but also handle legacy or
    damaged rows: atomically fence and quarantine the exact assignment/generation with
    a bounded diagnostic, exclude it until an operator or newer generation repairs it,
    and keep fair/keyset progress past poison entries. Keep transient offline/busy
    retries separately backoff-bounded and observable.
  - Add real-PostgreSQL tests with one and 256 invalid oldest rows followed by valid
    creates/deletes, protocol-version drift, concurrent repair, restart, and log-rate
    assertions. Valid work must progress within a bounded interval without dropping
    the poisoned workload's audit evidence or reservation unexpectedly.
  - Relevant code: `src/services/worker/reconciler.rs`,
    `src/services/worker_store/workloads.rs`, and
    `src/services/worker/registry.rs`.

- [x] Make the public attack arena's polling and reconnection load-safe.
  - Replace `setInterval(pollLive, 15000)` with one completion-scheduled, single-flight
    cycle. Add request timeouts, `AbortController` teardown, bounded exponential
    backoff with jitter, visibility/offline suspension, and `Retry-After` handling.
  - Stop requesting nonexistent or incorrectly cased
    `/api/Game/{id}/Scoreboard`, `/api/Game/{id}`, and
    `/api/Game/{id}/AttackFeed` URLs every cycle; use registered canonical routes and
    add a real bounded attack-feed endpoint only if the product still needs it.
  - Back off and jitter the raw WebSocket reconnect loop. It currently retries forever
    with a deterministic one-to-six-second delay, which can synchronize all spectators
    against a recovering replica.
  - Keep a single poll/reconnect owner across startup retries and teardown, and never
    let a slow or hung cycle accumulate concurrent server work.
  - Add slow-response, outage/recovery, invalid-route, background-tab, teardown, and
    many-spectator fixed-arrival-rate tests with bounded client concurrency and near-zero
    404/429/5xx responses.
  - Relevant code: `web/src/pages/games/[id]/Attack.tsx`,
    `src/controllers/game/routes.rs`, and `src/hubs/attack.rs`.

- [ ] Stop stale BYOC agents from forming a permanent synchronized reconnect flood.
  - Generated Compose bundles set the tunnel agent to `restart: unless-stopped`. The
    agent makes 20 attempts per minute after every failure at a fixed three-second
    interval forever, including
    definitive 403/426 responses after token revocation, participation removal, protocol
    mismatch, or event close, and 503 responses from a wrongly routed replica.
  - Every attempt reaches database-backed capability authorization before upgrade; an
    accepted upgrade performs a second live-authorization read. The WebSocket route has
    no handshake-specific global, per-participation, or per-capability admission before
    that work. Source-IP global limits do not aggregate agents distributed across team
    hosts.
  - Use capped exponential backoff with full jitter for transient transport/5xx
    failures, honor `Retry-After`, and reset only after a stable tunnel lifetime. Return
    machine-readable states so the agent can retry pre-start admission at the announced
    boundary but stop (or enter a very slow operator-visible terminal state) for a
    revoked token, closed event, or incompatible protocol. Make shutdown/cancellation
    interrupt pending delay and retain exactly one reconnect owner.
  - Admit upgrades cheaply before database work with bounded global and per-capability
    handshake budgets. Coalesce a pending reconnect for the same participation and
    challenge, avoid the duplicate authorization round trip during hand-off, and return
    an explicit retry/terminal signal that the shipped agent understands.
  - Add agent tests for revoked tokens, event close, protocol mismatch, non-network
    replica, repeated short-lived connections, and stable-session backoff reset. Run a
    synchronized-clock outage/recovery test with hundreds of agents and assert a spread
    reconnect distribution, bounded authorization queries/upgrades, prompt permit
    release, and responsive tunnels and `healthz`.
  - Relevant code: `agents/byoc-agent/src/main.rs`,
    `agents/byoc-agent/src/agent.rs`,
    `src/controllers/game/ad/byoc/compose.rs`,
    `src/controllers/game/ad/byoc.rs`,
    `src/controllers/game/ad/byoc_authorization.rs`, and
    `src/services/byoc_tunnel/`.

- [x] Close the Event-VPN route-casing bypass.
  - The middleware recognizes only exact lowercase `api/game` segments, while the
    registered A&D aliases and arena client use mixed-case `/api/Game/{id}/Ad/...`
    paths. Those aliases can therefore skip the live-peer/proof gate.
  - Canonicalize protected paths before classification or remove mixed-case aliases;
    make client proof matching and server enforcement use the same route definition.
  - Add integration tests for lowercase, uppercase, and mixed-case Jeopardy, A&D, and
    KotH routes, including anonymous, accepted, monitor, invalid-proof, and off-VPN
    requests.
  - Relevant code: `src/middlewares/event_vpn.rs`,
    `src/controllers/game/ad/mod.rs`, and
    `web/src/utils/EventVpnProof.ts`.

- [x] Repair live-arena endpoint and match-lifecycle resolution.
  - Use the registered lowercase game and standard-scoreboard routes; the current
    mixed-case requests do not load the Jeopardy overlay or event end time.
  - Either implement the documented, bounded attack-history route or remove the dead
    `AttackFeed` request and its misleading backfill comments.
  - Ensure an arena opened before or during an event reaches the final podium from an
    authoritative end-time update without requiring a reload.
  - Add route-contract and fake-clock tests for pure A&D, KotH, Jeopardy, and hybrid
    events, including an organizer extending the end time during play.
  - Relevant code: `web/src/pages/games/[id]/Attack.tsx` and
    `src/controllers/game/routes.rs`.

- [ ] Make scheduled and mutated notices reach already-open player pages.
  - Bound normal-notice content by UTF-8 bytes on the backend and keep the maximum
    safely below the 64-KiB SignalR frame envelope after JSON framing. The editor has no
    `maxLength`, `GameNoticeModel` has no validation, and the route otherwise accepts the
    generic JSON-body ceiling. One oversized immediate notice is cloned into the local
    broadcast channel and formatted once per connected player; Redis rejects only after
    the local publish at 256 KiB, while the WebSocket layer is configured for 64 KiB.
    This makes delivery replica-dependent and lets one ordinary organizer action consume
    large memory/bandwidth or disconnect local feeds.
  - Give create/update intent a stable operation ID and content fingerprint. Atomically
    persist one normal notice and an outbox event under a unique `(game_id, operation_id)`
    boundary, return the original notice for an exact replay, and reject a reused ID with
    different content. A rapid double activation or committed-but-lost create response
    must not insert and fan out duplicate announcements.
  - Add a synchronous ref-backed submit owner in the modal, disable every editable and
    close control while committing, show the byte limit, and recover the known operation
    after a timeout instead of blindly posting again. Client guards are usability only;
    the backend byte and idempotency boundaries remain authoritative across tabs and
    direct requests.
  - Preserve the exact notice draft when a request fails. The modal's `finally` block
    currently clears the content, schedule toggle, and publish time after both success
    and failure, so a transient overload or network error destroys the operator's
    announcement while leaving the modal open. Snapshot the submitted draft for its
    operation, clear it only after the matching success/explicit discard, and prevent a
    late response from clearing a newer edit.
  - Add a durable scheduler/reconciler for future `publishAt` notices; the create path
    stores them but only immediately published notices are broadcast.
  - Broadcast or revalidate edits and deletions so open clients do not retain stale
    notice text indefinitely.
  - Allow an editor to clear an existing schedule. The client currently sends
    `undefined`, and the backend's `COALESCE` preserves the old publish time.
  - Reconcile from the HTTP source after reconnect and deduplicate live deliveries.
  - Add max-minus-one/oversized/multibyte content, rapid click/Enter, two-tab,
    lost-response, failed-request draft preservation, edit-while-response-is-late,
    local/distributed fan-out, fake-clock, and real-PostgreSQL tests for schedule,
    reschedule, unschedule, update, delete, restart recovery, and exactly-once visible
    delivery. Assert rejected content is never stored or published and that one notice
    stays within the HTTP, Redis, and SignalR byte envelopes.
  - Relevant code: `src/controllers/edit/notices.rs`,
    `src/controllers/edit/mod.rs`, `src/services/event_bus.rs`,
    `web/src/components/GameNoticePanel.tsx`, and
    `web/src/components/admin/GameNoticeEditModal.tsx`, plus a new registered idempotent
    forward migration for notice operations/outbox delivery.

- [ ] Isolate realtime fan-out so one noisy event cannot starve every connected hub.
  - Replace or shard the single global 512-entry broadcast queue; filter by target and
    game before unrelated sockets compete for the same bounded history.
  - Treat `RecvError::Lagged` and a full distributed outbound queue as data loss, not a
    silent success. Emit metrics and force an authoritative HTTP resync or reconnect.
  - Prevent a maximum 100-flag A&D batch from evicting unrelated notices,
    submissions, monitor events, and administrative logs across all games.
  - Add multi-game burst/load tests covering local and Redis fan-out, lag detection,
    bounded memory, ordering, deduplication, and eventual backfill.
  - Relevant code: `src/services/event_bus.rs`, `src/hubs/signalr.rs`,
    `src/hubs/attack.rs`, and `src/controllers/game/ad/submit.rs`.

- [ ] Reject and meter inbound application traffic on read-only WebSocket feeds.
  - The raw attack socket silently consumes arbitrary Text and Binary frames. SignalR
    accepts any first Text frame as a valid handshake and then silently consumes every
    client Text/Binary application frame even though these hubs expose no client
    methods.
  - Connection admission and the 64 KiB frame/message limit bound sockets and individual
    messages, not bytes or frames per second. A broken or hostile client can therefore
    keep many admitted read-only sockets busy uploading maximum-size frames, consuming
    network and Tokio/WebSocket parsing work without making a valid request.
  - Parse and validate the exact SignalR JSON/version handshake. Close raw feeds on
    Text/Binary input; on SignalR feeds allow only small protocol ping/close frames and
    reject unsupported invocations with a protocol error.
  - Add per-connection and aggregate frame/byte token budgets, read/idle deadlines,
    metrics, and an abuse close code. Keep control Ping/Pong handling standards-compliant
    and release admission permits immediately on every rejection path.
  - Add bad-handshake, unsupported-invocation, sustained-frame, maximum-frame,
    many-connection, idle-timeout, and permit-release tests. The fixed-rate flood test
    must keep event delivery and `healthz` responsive with bounded CPU and memory.
  - Relevant code: `src/hubs/attack.rs`, `src/hubs/signalr.rs`,
    `src/hubs/admission.rs`, and `src/server.rs`.

- [x] Keep worker heartbeats and data-lane recovery bounded when Docker or the
  network stalls.
  - Each negotiated data lane owns a capped backoff that is never reset. After
    enough historical failures, a lane that had then been healthy for hours can
    still wait anywhere from one to 30 seconds after a single new disconnect;
    losing the four advertised lanes delays checker and player proxy traffic.
  - The heartbeat task awaits `runtime.probe()` on every tick and the
    O(containers) `runtime.usage()` every sixth tick without an absolute deadline.
    A hung Docker ping, disk check, or container listing therefore prevents the
    agent from sending even an unhealthy heartbeat, lets the server lease expire,
    and tears down every otherwise usable data lane. The outer pre-connect probe
    can hang the agent indefinitely for the same reason.
  - The outer control loop retries every `run_session` error forever. Revoked/expired
    certificates, wrong CA/server name, unsupported protocol revision, invalid worker
    identity, and other operator-action-required failures therefore reconnect with the
    same capped backoff indefinitely. A fleet of stale or misconfigured agents can
    sustain TLS verification and rejection logs even though none can ever become live.
    The listener caps concurrent global/per-IP handshakes, but has no connection-rate
    or certificate-identity budget and emits a warning for every capacity rejection and
    failed handshake, so churn can monopolize all slots and the log pipeline.
  - Put short absolute deadlines around liveness probes and sample usage in a
    separate bounded task. Send the last bounded usage plus a typed unhealthy or
    timeout state within the lease, or deliberately close the session within a
    bounded time; never let inventory sampling own heartbeat progress.
  - Reset each data-lane backoff only after a stable lifetime, matching the
    control session's 60-second rule. Preserve full jitter for short flaps, make
    cancellation interrupt pending delay, and stop retrying a stale session or
    incompatible protocol after its control owner is gone.
  - Classify control failures into transient transport/runtime states and terminal
    authentication/protocol/configuration states. Exit or enter an explicit quarantined
    readiness state for terminal failures until credentials/configuration change;
    preserve capped jitter and stable-success reset only for transient recovery. On the
    server, add fail-fast per-source, per-certificate/worker, and aggregate handshake
    rate buckets in addition to concurrency, reserve bounded reconnect progress for
    known workers, sample repeated rejection logs, and enforce the same ceiling at the
    load balancer/firewall where available.
  - Add deterministic tests for repeated failures followed by a stable lane, a
    hung Docker ping/list operation, lease expiry and recovery, control-session
    cancellation, all four lanes dropping together, revoked/expired certificates,
    wrong server name/CA, protocol mismatch, many stale agents, handshake-slot churn,
    and log-rate bounds. The fixed-rate worker outage test must keep legitimate worker
    reconnect, proxy/checker traffic, and `healthz` responsive.
  - Relevant code: `agents/worker-agent/src/client/data.rs`,
    `agents/worker-agent/src/client/control.rs`,
    `agents/worker-agent/src/client/mod.rs`, `agents/worker-agent/src/backoff.rs`,
    `agents/worker-agent/src/runtime/docker.rs`,
    `src/services/worker/listener.rs`, and
    `src/services/worker/listener/admission.rs`.

- [x] Claim worker enrollment before CSR signing and make an ambiguous exchange
  recoverable.
  - `/api/workers/enroll` first resolves a still-live one-use token, then sends the
    caller's CSR to an unrestricted `spawn_blocking` signing task, and consumes the
    token only after certificate creation. Concurrent requests carrying the same token
    can all pass the read and perform CSR parsing, proof verification, key signing, PEM
    allocation, and random-serial generation even though only one later database update
    can win. The per-source Register policy limits one IP, but does not bound signer
    work across sources or a leaked token.
  - The agent creates its private key and CSR only in memory and persists them after it
    receives the response. If the server commits `enroll_certificate` but the response
    is lost, the next run creates another key/CSR and the consumed token returns 401;
    the successfully bound certificate is unavailable to the worker and an operator
    must issue a new token. Blind retries would also keep repeating pre-claim signer
    work.
  - Atomically claim the token before entering the signer with a durable enrollment
    operation and bounded lease. Bind the claim to the worker, CSR public-key/request
    digest, and opaque operation ID; an exact retry must join or recover the same issued
    certificate response, while a competing CSR or operation fails before signing.
    Persist the terminal response for a short documented recovery window and make
    signer failure/timeout transition the claim into an explicitly retryable or failed
    state without reopening it to concurrent signers.
  - Put CSR parsing/signing behind fail-fast per-token/source and aggregate concurrency
    and CPU-work admission before `spawn_blocking`. Cap queued jobs, apply an absolute
    deadline, return `429/503` with `Retry-After`, and make the claim state
    replica-safe; the HTTP Register policy remains defense in depth.
  - Persist the generated private key, CSR, and operation metadata atomically in the
    protected agent state directory before the first request. Retry only ambiguous
    transport/5xx outcomes with that exact CSR and capped jitter, reconcile a committed
    result after restart, and never replace a known operation with a fresh key merely
    because its response timed out.
  - Add concurrent same-token/same-CSR, same-token/different-CSR, lost response after
    commit, disconnect during signing, invalid/maximum CSR, signer timeout/panic,
    agent/server restart, expired recovery record, multi-replica, and saturation tests.
    Assert one signing job and certificate per operation, bounded blocking tasks and
    memory, prompt overload responses, and successful recovery without a new token.
  - Relevant code: `src/controllers/workers.rs`, `src/services/worker_pki.rs`,
    `src/services/worker_store/nodes.rs`, `agents/worker-agent/src/enroll.rs`, and a new
    registered idempotent forward migration for enrollment operations.

- [x] Make Event-VPN sensor delivery durable, idempotent, and retry-safe.
  - A full two-entry capture queue records dropped rows, but after the main loop
    receives a batch it uploads only once. Any timeout or 5xx merely logs an
    error and permanently discards flow, DNS, endpoint, and flag-transport
    evidence without recording the loss.
  - Blindly adding retries is unsafe: telemetry rows use conflict-ignore inserts,
    but sensor-drop counters are additive and quota admission charges the full
    estimated batch before deduplication. A lost response after commit can make a
    retry double-count drops or disable telemetry near quota even when it adds no
    rows.
  - Give every batch a stable ID and record its result atomically on the server.
    Keep a bounded durable sensor spool, remove a batch only after an acknowledged
    result, and report actual shedding when explicit byte/count/age limits are
    exceeded. Charge quota and additive counters exactly once from actual new
    rows or an idempotent reservation.
  - Classify permanent 4xx responses separately from transient transport/5xx/429
    failures, honor `Retry-After`, and use capped exponential backoff with full
    jitter and stable-success reset. Preserve one upload owner across restart and
    cancellation.
  - The startup snapshot loop currently retries every error at a fixed five
    seconds, while each snapshot performs a live VPN read plus per-game peer and
    flag-pattern queries. Cache/version and single-flight this snapshot, collapse
    the N+1 query shape, and stop or visibly quarantine invalid credentials and
    configuration instead of polling them forever.
  - Add lost-response-after-commit, duplicate/drop-counter, near-quota duplicate,
    401/429/5xx, restart-with-spool, spool-overflow, and maximum-game/peer/pattern
    tests. A synchronized sensor outage must recover with bounded database work
    and no telemetry gaps that are not explicitly counted.
  - Relevant code: `src/bin/rsctf-event-sensor.rs`,
    `src/controllers/event_security.rs`,
    `src/services/event_security/telemetry.rs`,
    `deploy/compose.ad-vpn.yml`, and `deploy/compose.roles.ad-vpn.yml`.

- [x] Bound and lifecycle-proof trusted solve-receipt and challenge-variant
  records.
  - Every valid receipt request creates a fresh UUID, nonce, proof, and database
    row. There is no verifier attempt/idempotency key, so a lost response followed
    by a legitimate retry creates another valid receipt for the same solve; a
    buggy trusted verifier can append rows indefinitely through the machine
    endpoint.
  - Expired unconsumed receipts remain in the partial lookup index forever. The
    shipped migration rejects every receipt and variant delete, and all of their
    ownership foreign keys use `ON DELETE RESTRICT`; challenge, game, team, or
    participation hard deletion can therefore fail after an otherwise deletable
    pre-event variant or receipt exists.
  - Require a stable verifier attempt ID, store only its keyed hash, and use an
    atomic uniqueness constraint/upsert that returns or deterministically
    reconstructs the original proof after a lost response. Add bounded global and
    per-issuer/participation issuance admission so one faulty verifier cannot
    consume unbounded database and signing capacity.
  - In a new forward migration, separate the short-lived active one-use record
    from explicitly retained audit provenance. Expire, archive, or partition
    active secrets and remove them from hot indexes under a bounded maintenance
    job; define deletion/tombstone semantics that preserve required evidence
    without blocking the platform's authorized hard-delete workflows.
  - Add concurrent duplicate, lost-response, fixed-rate verifier-loop, expiry and
    index-size, pre-event challenge/game/team deletion, and consumed-proof audit
    tests. Each semantic attempt must yield one proof/result and bounded retained
    storage.
  - Relevant code: `src/controllers/event_security.rs`,
    `src/services/event_security/receipts.rs`,
    `src/services/event_security/variants.rs`,
    `src/migrations/m0094_challenge_variants_and_receipts.rs`,
    `src/services/blob_refs/challenges.rs`, and
    `src/controllers/team/revocation.rs`.

- [ ] Renew platform-proxy capabilities without tearing down live player tunnels.
  - Proxy capabilities last two hours, but five minutes before expiry
    `InstanceEntry` immediately blanks the usable endpoint and deletes the local
    WSRX tunnel before it has requested a replacement. Every scheduled renewal can
    therefore interrupt a live TCP session even though the old capability remains
    valid for another five minutes.
  - A failed replacement request has no automatic retry and the still-valid token
    and tunnel have already been discarded. The retry flag is also cleared before
    capability issuance settles, so repeated manual actions can start overlapping
    issuance attempts.
  - Renew with a prepare/commit handoff: fetch and validate one replacement first,
    establish and health-check its tunnel, atomically switch the displayed endpoint,
    then drain the old tunnel. Preserve the old path until its real expiry when
    preparation fails; an admission-token refresh must not terminate an already
    established data stream.
  - Give the handoff one abortable generation and one in-flight owner. Retry only
    transient failures with bounded jitter inside the remaining validity window,
    honor `Retry-After`, and treat authorization/not-found responses as terminal.
  - Add fake-clock and live-stream tests for the T-minus-five-minute renewal,
    failed renewal fallback, stale responses, instance changes, unmount, rapid retry,
    and eventual recovery. Assert uninterrupted bytes and bounded issuance requests.
  - Relevant code: `web/src/components/InstanceEntry.tsx`,
    `web/src/components/WsrxProvider.tsx`, `src/services/token.rs`, and
    `src/controllers/proxy/capability.rs`.

- [ ] Coalesce live-session authorization leases before normal tunnels become a
  PostgreSQL polling flood.
  - Every established platform-proxy session starts its own five-second loop. A game
    lease repeatedly opens the participation advisory/row-lock transaction, rechecks
    account, membership, team, game, challenge, division, container, and Event-VPN
    state, then commits; an exercise lease performs its own database read. The proxy
    admission map limits one user, participation, or workload but has no deployment-
    wide session ceiling, so cost still grows with every connected player and replica.
  - A&D SSH is worse per connection: each shell reruns seven serial ORM reads every
    15 seconds, including loading the complete team membership and user roster. Its
    five-shell-per-team limit has no global bound. Every active or retained-idle BYOC
    endpoint independently performs another authorization query every 15 seconds.
  - The proxy and SSH loops use Tokio intervals with the default burst missed-tick
    behavior and have no query deadline. When PostgreSQL or the pool stalls longer
    than the lease period, each recovered session can immediately issue catch-up
    validations, turning the outage into a synchronized positive-feedback burst.
    A failed validation then closes otherwise live transports, which can feed client
    reconnect traffic into the same recovery window.
  - Publish a durable authorization generation/deadline for account, participation,
    challenge, instance, key, and Event-VPN mutations. Fan that invalidation out to
    process-local sessions and use one bounded, jittered batch reconciliation as the
    fail-safe for missed notifications and database-side changes; do not poll the same
    grant once per socket. Derive event-end wakeups from PostgreSQL time and retain a
    final authoritative check before opening each backend stream.
  - Until that lands, single-flight identical lease reads, set missed ticks to `Skip`,
    use completion-scheduled jitter plus absolute database deadlines/backoff, collapse
    SSH eligibility and banned-roster checks into one indexed query, and add global,
    per-source, per-event, and per-role connection/query-work admission shared across
    replicas. Database uncertainty must fail closed without a reconnect stampede.
  - Add maximum-roster and multi-replica fixed-rate tests for thousands of proxy
    tunnels, five SSH shells per team, active and idle BYOC endpoints, a slow/exhausted
    pool, missed invalidations, event close, key/token rotation, suspension, Event-VPN
    loss, and recovery. Measure lease queries, pool waiters, locks, reconnects, and
    event latency; idle validation work must be bounded independently of socket count.
  - Relevant code: `src/controllers/proxy/mod.rs`,
    `src/controllers/proxy/authorization.rs`, `src/services/proxy_admission.rs`,
    `src/services/ad/ssh.rs`, `src/services/byoc_tunnel/mod.rs`,
    `src/services/byoc_tunnel/authorization.rs`, and
    `src/services/byoc_tunnel/control.rs`.

- [ ] Bound and batch proxy flag-egress telemetry so reconnect churn cannot
  enqueue one PostgreSQL writer per session.
  - A game proxy resolves its target and live identity before the process-local
    session permit is acquired, and `build_egress_scan` then performs two more ORM
    reads for every admitted open. If the container returns its own flag, the
    byte pump launches a detached `tokio::spawn` that opens a transaction, waits
    for the participation-evidence audit lock, and performs an upsert. A session
    records at most once and the unique key coalesces stored rows, but neither
    property coalesces tasks, pool waiters, lock acquisitions, or queries.
  - The four-session user limit controls simultaneous sockets only, is local to
    one replica, and is released as soon as a short connection closes. Native
    WSRX capability connections are anonymous to the global limiter and share
    only its source-IP bucket. A buggy reconnect loop, many accounts/sources, or
    a load-balanced retry storm can therefore churn completed sessions and
    detached writers fast enough to contend with event-critical PostgreSQL work;
    a slow pool lets those untracked tasks accumulate after the sockets are gone.
  - Put proxy opens behind a named distributed token bucket keyed by account,
    participation, workload, and source-IP backstop before target resolution and
    egress metadata reads. Return `429` with `Retry-After`; keep the existing
    process-local live-session permit as a separate resource ceiling. Carry the
    authenticated capability subject into the limiter rather than treating the
    native helper as anonymous, and cache or join immutable egress scan metadata
    behind the relevant instance/flag revision.
  - Replace the per-hit spawn with one supervised, bounded application-lifecycle
    queue. Use non-blocking admission, aggregate identical forensic keys and
    their counts/first/last timestamps in a bounded map, and flush fixed-size raw
    SQL upsert batches with absolute timeouts, bounded retries, explicit overflow
    metrics, and a shutdown drain. Preserve the evidence lifecycle lock and its
    fail-closed sealing semantics without holding one connection per observation.
  - Give the browser/native tunnel exactly one reconnect owner. Retry transient
    failures with capped exponential backoff and jitter, honor `Retry-After`, and
    stop on authorization/not-found responses, expiry, unmount, or a superseding
    generation; never let latency probes, capability renewal, and manual retry
    create independent server-open loops.
  - Add fixed-rate tests for one and thousands of flag-bearing short sessions,
    many accounts behind one NAT, many source IPs, multiple replicas, a blocked
    audit lock, an exhausted/slow database pool, a full telemetry queue, lost
    responses, shutdown drain, and client reconnect recovery. Assert bounded
    tasks, connections, queries, queue memory, and retained rows while `healthz`,
    score submission, and event-control latency remain healthy.
  - Relevant code: `src/controllers/proxy/mod.rs`,
    `src/controllers/proxy/egress.rs`, `src/controllers/proxy/capability.rs`,
    `src/services/proxy_admission.rs`, `src/middlewares/rate_limiter.rs`,
    `web/src/components/InstanceEntry.tsx`, and
    `web/src/components/WsrxProvider.tsx`.

- [ ] Put every proxy tunnel behind revocable connection and byte-work admission.
  - Player/exercise tunnels retain a process-local `ProxyPermit` and a live-identity
    lease, but `/api/proxy/noinst/{id}` passes neither into `run_or_close`. One admin
    session or reusable two-hour preview capability can therefore open an unbounded
    number of test-container sockets, each occupying a TCP/worker stream and two pump
    futures for as long as the 30-minute absolute timeout. The HTTP request-rate limit
    slows opens but does not cap accumulated live connections, and load balancing
    multiplies the gap across replicas.
  - Preview sockets also authenticate the admin/security stamp only at open. Logout,
    demotion, a credential rotation, target deletion, or preview ownership change does
    not revoke an established tunnel. A reconnecting WSRX client can leave old sockets
    alive while opening replacements under the same capability.
  - The 64 KiB WebSocket frame/message ceiling bounds one allocation, not sustained
    work. Neither direction has a frame/byte token budget or inactivity deadline, so a
    buggy or hostile client can upload maximum frames continuously and an echoing or
    compromised container can amplify them back. Every TCP-to-WebSocket 4 KiB read
    additionally allocates a fresh frame. Existing per-user/participation/workload
    counts are local, omit preview tunnels, and do not bound aggregate CPU, worker data
    streams, or network bandwidth.
  - Extend one proxy-admission owner to player, exercise, and preview route classes with
    fail-fast local global, per-subject, per-source, per-event, and per-workload live
    ceilings. Apply a distributed open/churn budget before target/database resolution,
    carry the verified capability subject rather than an anonymous bucket, and hold the
    permit until the upgraded socket is dropped. Return a stable overload close reason
    and retry delay instead of accepting then silently closing.
  - Make preview grants revisioned and continuously revocable through the shared
    invalidation/lease mechanism required for other proxy sessions. Do not create one
    PostgreSQL poller per socket: fan account/role/stamp, container, and capability
    invalidations into local sessions with one bounded reconciliation fallback.
  - Enforce hierarchical ingress/egress byte and frame-rate budgets with backpressure.
    Lease coarse byte credits from a distributed participation/source/workload budget
    so the hot per-frame loop never performs a Redis round trip, retain a strict local
    aggregate ceiling for Redis/outage safety, and close sustained violators while
    preserving a documented interactive/bulk-transfer burst. Add read/write inactivity
    deadlines separately from the absolute session lifetime.
  - Give WSRX one generation-bound reconnect owner and explicitly retire the preceding
    socket. Honor overload retry timing, use capped jitter for transient failures, and
    stop on authorization/revocation closes so the protection itself cannot create a
    reconnect storm. Export active sessions, opens, frames, bytes, throttles, idle
    closes, and per-route saturation metrics.
  - Add line-rate upload/echo, silent socket, reconnect-with-old-socket, reusable
    capability, logout/demotion/rotation/deletion, many-admin, many-source, multi-replica,
    Redis-loss, slow backend, and trusted-worker tests. Assert bounded sockets, tasks,
    streams, bandwidth, frame allocations, and authorization work while legitimate
    interactive and bounded artifact transfers remain usable.
  - Relevant code: `src/controllers/proxy/mod.rs`,
    `src/controllers/proxy/capability.rs`, `src/controllers/proxy/transport.rs`,
    `src/services/proxy_admission.rs`, `src/services/token.rs`,
    `src/middlewares/rate_limiter.rs`, `src/services/worker/data.rs`,
    `web/src/components/WsrxProvider.tsx`, and
    `web/src/components/InstanceEntry.tsx`.

- [ ] Isolate and bound scheduled Docker image cleanup from event-critical
  maintenance.
  - The process-local cleanup timestamp starts at zero, so every new maintenance
    leader runs a full cleanup on its first pass and a restart/failover forgets the
    deployment-wide cadence. This job runs serially before expired-container reaping,
    final A&D evidence sealing, KotH transition recovery, ended-backend teardown, and
    scoreboard finalization.
  - Docker `df`, dangling-image prune, container listing, image inspect/removal, and
    the post-removal inspect have no common absolute deadline. One stuck daemon call
    can therefore freeze the only maintenance pass indefinitely.
  - Cleanup loads every owned image and then, once per candidate, reloads every
    challenge image reference and lists every Docker container. It holds a PostgreSQL
    advisory-lock connection across those daemon calls, so catalog growth multiplies
    database/daemon work and can block builds or runtime image reservations.
  - Move image cleanup to an independently supervised and time-budgeted job with a
    durable cross-replica lease/cadence. Snapshot challenge references and live image
    IDs once per pass, claim a small indexed candidate batch, use bounded concurrency,
    and carry an observable backlog to later passes.
  - Put absolute deadlines and cancellation around every daemon operation. Never hold
    a database transaction/connection during long Docker I/O; revalidate the exact
    image identity under a short lock immediately before committing ownership
    changes.
  - Add real-Docker fixed-rate tests for a hung daemon call, thousands of images and
    challenges, leader restart/failover, concurrent build/reservation, and event
    closeout. Maintenance backlog must not delay final evidence, container reaping,
    or `healthz`.
  - Relevant code: `src/services/cron/mod.rs`,
    `src/services/image_storage.rs`, and
    `src/controllers/admin/builds/images.rs`.

- [ ] Make live anti-cheat reconciliation incremental instead of rescanning complete
  event histories every 30 seconds.
  - Every eligible control/engine replica selects every active game. The advisory
    lock prevents simultaneous work for one game, but contenders still query and open
    transactions, and the winner holds an otherwise idle transaction, pooled
    connection, and advisory lock while six detector families run through separate
    pool connections.
  - The live abnormal-solve pass reloads all canonical solves and competitive wrong
    attempts; correlation reloads all identity observations, submission identities,
    and exemptions; event-security fusion revisits all VPN telemetry and relationship
    joins. The automatic database/CPU cost therefore grows monotonically throughout
    an event even when no evidence changed.
  - The admin “derive findings” button calls `derive_context_findings` directly instead
    of entering the reconciler's per-game claim. Component-local action state is its
    only duplicate guard, so two tabs/operators—or one click racing the scheduled
    pass—can run the same full telemetry scans and long transactions concurrently.
  - Persist per-source/per-detector high-water marks or dirty versions and drive
    incremental work from the existing durable submission and telemetry outboxes.
    Coalesce dirty-game notifications and select only games with new relevant
    evidence. Run population-relative/non-monotonic rules only when their aggregates
    change or during the barrier-backed final snapshot.
  - Replace full in-memory histories with bounded indexed SQL aggregates. Use a short
    durable claim/lease rather than an open transaction around all detectors, enforce
    a per-game deadline and bounded cross-game concurrency, and retain one
    authoritative final sweep after evidence intake closes.
  - Route manual derivation through that same coalescing claim/job with a stable
    operation ID. Return current progress or the already-completed generation instead
    of starting parallel work, and make the browser recover the known operation after
    an ambiguous response.
  - Add multi-replica and fixed-rate tests whose event history grows while new
    evidence stops. Idle reconciliation must approach zero rows/work, failures must
    remain retryable, final findings must match a full reference sweep, and live
    submit/scoreboard/`healthz` latency must stay bounded. Include rapid manual clicks,
    two tabs/operators, and a manual/scheduled race; all must share one effective pass.
  - Relevant code: `src/services/suspicion/outbox.rs`,
    `src/services/suspicion/cheat_checks.rs`,
    `src/services/suspicion/correlation.rs`, `src/services/event_security/fusion.rs`,
    `web/src/pages/admin/games/[id]/Info.tsx`, and
    `src/controllers/admin/anti_cheat.rs`.

- [ ] Repair the traffic-flow inspector contract and stop stale filters from
  repeatedly parsing whole PCAPs.
  - The browser sends payload-regex and peer-IP filters after a 300 ms debounce and
    direction/flag filters on each toggle, but the Rust handler accepts no query
    object and ignores all of them. It returns `{src,dst,packetCount,bytes}` while
    the UI expects `connectionPort`, timestamps, directional counts, flag hits, and
    peer identity, so rows have undefined keys/fields and cannot reliably open a
    detail view.
  - Each keystroke or toggle starts a non-abortable blocking parse of as much as
    256 MiB. Cleanup only ignores stale responses; it does not cancel their server
    work. Two stale requests can occupy the process-wide two-slot inspection
    semaphore and make the newest request fail with 503, after which the UI erases
    its results.
  - The detail route reparses the complete file again and returns empty timestamps and
    payload chunks, so it also violates the generated wire contract.
  - Define one camelCase flow DTO and typed, bounded filter contract end to end.
    Build or cache a bounded flow index once per immutable file identity/size/mtime,
    apply validated filters and pagination to that snapshot, and share it between
    summary and detail reads. If full reassembly is not implemented, remove the
    unsupported UI fields instead of fabricating an empty success.
  - Abort/supersede stale browser requests, admit inspection through weighted query
    work, coalesce identical parses, and return retry metadata without discarding the
    last good view. Keep CPU, bytes, flow count, regex complexity, concurrency, cache
    size, and cache lifetime explicitly bounded.
  - Add real-PCAP contract tests plus rapid-typing, slow-parse, close/unmount,
    concurrent-monitor, 256 MiB, invalid-regex, and file-growth/replacement tests.
    Assert one parse per file version, newest-filter correctness, bounded work, and a
    functional detail payload.
  - Relevant code: `web/src/components/traffic/FlowInspector.tsx`,
    `web/src/components/traffic/FlowDetail.tsx`, `web/src/Api.ts`,
    `src/controllers/game/traffic.rs`, `src/controllers/game/mod.rs`, and
    `src/services/traffic.rs`.

- [ ] Meter and aggregate honeypot telemetry before it becomes an unthrottled
  database and connection-flood endpoint.
  - Every public decoy GET/POST synchronously inserts a new `HoneypotHits` row. The
    bait paths are outside `/api`, so the global limiter deliberately bypasses them;
    an unauthenticated client can issue unlimited writes and grow the table/indexes
    without bound while tying request latency to PostgreSQL.
  - When protocol honeypots are enabled, every accepted TCP connection spawns a task
    with no global or per-source permit. A three-second read timeout limits one task's
    lifetime but not the number of sockets, tasks, or subsequent database inserts, so
    a connection flood can exhaust file descriptors, memory, and the pool.
  - Apply silent telemetry admission before authentication and database work while
    preserving the same plausible 404 response. Aggregate repeated `(source,bait)`
    observations into fixed time buckets with an atomic upsert/count, cap stored
    header fields, sample excess traffic, and enforce a retention/partition budget so
    hostile input cannot create permanent unbounded storage.
  - Put TCP accepts behind global and per-source concurrency/rate limits, set socket
    deadlines/options, and hand observations to a bounded non-blocking queue with
    explicit dropped/sampled counters. A database outage must not retain unlimited
    request or connection tasks.
  - Add unauthenticated HTTP fixed-rate, distributed-replica, TCP slow-loris/flood,
    database-outage, queue-overflow, and retention tests. Bound concurrent tasks,
    inserts, rows, bytes, pool use, and latency while keeping the decoy response
    indistinguishable from an ordinary miss.
  - Relevant code: `src/controllers/honeypot.rs`,
    `src/services/honeypot_listener.rs`,
    `src/services/suspicion/honeypot.rs`, `src/middlewares/rate_limiter.rs`,
    `src/server.rs`, and `src/migrations/m0018_honeypot_hit.rs`.

- [ ] Repair the game-clone route contract and make cloning bounded and replayable.
  - `CloneGameModal` posts to lowercase `/api/edit/games/{id}/clone`, while Axum
    registers only case-sensitive `/api/edit/games/{id}/Clone`. The organizer action
    therefore never reaches `clone_game`; depending on fallback routing it receives
    either a typed 404 or the SPA shell and reports a generic clone failure.
  - Once the route is corrected, a lost response, another tab, or another organizer can
    repeat the same clone with no operation identity and create multiple hidden games.
    Component-local `loading` cannot make that database mutation exactly once.
  - A challenge-inclusive clone holds one transaction while iterating every source
    challenge, querying its flags separately, and inserting every copied flag one at a
    time. Clone latency, retained connection time, and query count therefore grow with
    both challenge and flag counts, and duplicate requests multiply that work.
  - Preserve the established uppercase route for API compatibility, move the modal to
    one generated/canonical client method, and add an exact route-contract test. Validate
    title and time bounds on the backend and ensure unmatched `/api` paths can never
    return the SPA document.
  - Give each clone intent a stable operation ID and source configuration revision.
    Atomically reserve a durable clone job, return/recover the same destination game ID
    for an exact replay, and reject a conflicting revision instead of creating another
    template. Apply documented challenge/flag limits and a job deadline before copying.
  - Copy relational rows with bounded set-based `INSERT ... SELECT` operations (including
    an explicit old-to-new challenge-ID map for flags), or process bounded chunks outside
    one long transaction while the destination stays hidden until an atomic publish.
    Expose progress/failure and make crash recovery resume or clean up the one job.
  - Add lowercase/uppercase route, API-fallback, rapid double-click, two-tab/operator,
    reversed/lost response, multi-replica, crash recovery, maximum challenge/flag, and
    query-count tests. One intent must create one hidden game and return one stable ID
    while normal event reads and `healthz` remain responsive.
  - Relevant code: `web/src/components/admin/CloneGameModal.tsx`, `web/src/Api.ts`,
    `src/controllers/edit/mod.rs`, `src/controllers/edit/games/cloning.rs`,
    `src/server.rs`, and a new registered idempotent forward migration for clone jobs.

- [ ] Bound and idempotently recover admin user imports and password issuance.
  - `UserImportModal` parses every CSV row into React state and sends the complete list;
    neither it nor `ImportRequest` enforces a row count. The default JSON byte limit is
    not a work limit: many minimal rows still cause one serial Argon2 hash, provisioning
    transaction, cache write, and full plaintext result row each.
  - The global Argon2 semaphore bounds hashes actively using CPU/memory, but it does not
    bound accepted batches or handler futures queued for the gate. Several imports can
    therefore retain large request/result state and monopolize credential hashing for a
    long time, delaying login, registration, and recovery hashes needed during an event.
  - Import intentionally updates an existing email with a newly generated password.
    Retrying after a committed-but-lost response recredentials every successful row
    again, invalidates the first returned CSV, and repeats provisioning. The single-user
    admin password-reset endpoint has the same ambiguous-response problem, while the
    older explicit `POST /api/admin/users` batch is also uncapped.
  - Enforce backend row, field-byte, unique-team, request, response, and total-work
    limits before hashing. Add a dedicated try-admission budget for credential jobs so
    overload fails early with `429/503` plus `Retry-After` instead of queueing behind
    Argon2; keep the existing global hash gate as defense in depth.
  - Run imports as durable, resumable jobs keyed by an opaque client operation ID and a
    normalized input digest. Persist per-row completion and an encrypted, short-lived
    credential result so an exact authorized retry/download returns the same passwords
    rather than resetting users. Reject a different operation that races for the same
    targets, and expire plaintext recovery material on a documented schedule.
  - Cap the browser before allocating/editing all rows, use a ref-backed submit owner,
    show job progress, and recover the known operation after timeout/reload. Do not offer
    a blind whole-import retry; retry only rows whose durable status is uncommitted.
  - Add maximum/minimal-row, oversized-field, duplicate-email/team, rapid submit,
    two-tab/admin, lost response after partial and complete progress, reversed response,
    restart, expired-result, and login-under-load tests. Assert bounded Argon2 waiters,
    memory, pool use, result bytes, and one usable credential per committed operation.
  - Relevant code: `web/src/components/admin/UserImportModal.tsx`,
    `web/src/pages/admin/Users.tsx`, `src/controllers/admin/users.rs`,
    `src/controllers/admin/users_mutate.rs`, `src/controllers/admin/users_credentials.rs`,
    `src/utils/crypto_utils.rs`, and a new registered idempotent forward migration for
    credential jobs/results.

- [ ] Fail fast on duplicate credential mutations before they queue Argon2 work.
  - `PasswordChangeModal` has no pending state or synchronous in-flight guard, so rapid
    click/Enter activation can dispatch several `PUT /api/account/changepassword`
    requests. Every concurrent request can load the same security stamp and password
    hash, verify the old password, and hash the new password before the conditional
    update lets only one win. The losing requests therefore consume two memory-hard
    jobs apiece for no useful state change.
  - `Reset` likewise uses React state as its only guard and exposes the same callback
    through input hotkeys and the button. More importantly, every concurrent request
    holding one valid reset link loads its ticket and performs the expensive new-password
    hash *before* `compare_and_remove` lets one request claim the generation. A client
    retry burst can therefore enqueue many memory-hard jobs even though at most one can
    update the account.
  - The profile email-change handler sets the page-level `disabled` state, but the
    retained modal's inputs, Cancel, and Confirm never read it. Every repeated click can
    load the same security stamp and run another Argon2 password verification before an
    immediate-mode compare-and-set lets only one mutation win; confirmation mode lets
    every verifier continue into token construction and SMTP work.
  - The route has no credential-specific rate policy. The global 150-request-per-minute
    account ceiling limits HTTP count, but `ARGON2_GATE.acquire().await` admits an
    unbounded waiter queue and is shared by login, registration, reset, email change,
    and admin credential work. One looping/stuck client—or synchronized clients across
    accounts—can retain handler state and delay event-kickoff logins long after the
    original burst stops.
  - Add a distributed, fail-fast credential-mutation budget keyed by verified account
    and source IP before any user lookup or hash. Permit only one operation for the same
    `(user_id, security_stamp)` at a time, reject a concurrent duplicate with `429` and
    `Retry-After`, and cap aggregate admitted jobs/waiters across credential endpoints.
    Keep the core-sized Argon2 execution semaphore as a memory/CPU backstop, with
    reserved or fairly scheduled capacity so bulk/admin work cannot starve login.
  - Hold one admitted workflow permit across both verify and hash, release it on every
    error, disconnect, panic, and shutdown, and never hold a PostgreSQL connection or
    transaction while waiting for Argon2. Make the limit replica-safe when Redis is
    configured and use a bounded fail-closed local fallback when it is unavailable.
  - Replace cache-only reset consumption with a durable ticket/attempt state whose
    hashed token, account/security generation, request digest, operation ID, lease, and
    terminal result are bounded and indexed. Atomically claim it *before* hashing;
    coalesce an exact in-flight replay, reject competing work promptly, and finalize the
    password/security-stamp update plus ticket consumption in one short transaction.
    A crash or database failure must leave a safe lease-recovery path, while a response
    lost after commit must let the exact operation confirm success without hashing or
    changing the password again.
  - Give the password modal a ref-backed operation generation; disable inputs, Cancel,
    close, and Confirm synchronously until the request settles, ignore stale responses,
    then await logout/session-cache clearing before navigation. Reconcile an ambiguous
    response by requiring login with the intended new credential, not by replaying the
    old-password mutation in a loop.
  - Give the reset page the same synchronous owner and a stable operation ID, and use
    native form submission instead of independent key/button activation paths. Recover
    only that known attempt after timeout; never start another hash while its durable
    status is running or committed.
  - Put email change behind that same ref-backed owner and disable/lock every modal
    activation path immediately. Key server admission by the account security stamp and
    canonical destination intent so two tabs cannot both verify the same credential and
    continue; confirmation delivery then flows through the durable mail intent below.
  - Add rapid click/Enter, two-tab, valid/wrong-old-password, slow Argon2, disconnect,
    many-account, same reset token with same/different operations, database failure
    after claim/before commit, committed-but-lost response, lease expiry, Redis outage,
    multi-replica, and login-under-load tests. Assert one admitted same-account
    mutation, one reset hash/final password, bounded hash jobs and waiters, prompt
    overload responses with `Retry-After`, no database connection held during hashing,
    and bounded legitimate login latency.
  - Relevant code: `web/src/components/PasswordChangeModal.tsx`,
    `web/src/pages/account/Profile.tsx`, `web/src/pages/account/Reset.tsx`,
    `src/controllers/account/mod.rs`,
    `src/controllers/account/recovery.rs`, `src/utils/crypto_utils.rs`, and
    `src/middlewares/rate_limiter.rs`, plus a new registered idempotent forward
    migration for durable recovery attempts.

- [x] Coalesce account-mail intent and deliver it through a bounded durable outbox.
  - Password recovery mints a new token, invalidates the prior link, and starts one
    detached `tokio::spawn` per matching request. Each task constructs a new
    environment-backed `MailSender`, may occupy SMTP for 15 seconds, and then writes an
    audit record; there is no queue, aggregate concurrency cap, durable retry, or task
    ownership at shutdown. The 20-per-five-minute source policy cannot bound concurrent
    SMTP connections/tasks across many sources or replicas.
  - Recovery and confirmed email change call `MailSender::from_env`, while the settings
    UI persists the live SMTP configuration in `Configs` and confirmation delivery
    correctly uses `from_database`. A database-configured installation can therefore
    reject email changes as “SMTP is not configured” and silently drop password-reset
    mail while returning the enumeration-safe success response. Resolve one shared,
    revisioned effective SMTP configuration for every workflow and record delivery
    failure internally without changing the anonymous response shape.
  - `Recovery` does not take a synchronous in-flight owner before awaiting captcha, so
    rapid click/Enter or two tabs can issue multiple recoveries before React commits its
    disabled state. Registration has the dual click/form dispatch described above, and
    an authenticated replay for a pending account verifies the password and sends a
    fresh confirmation synchronously. These duplicate intents can churn the current
    reset/confirmation generation, make links received moments apart invalidate each
    other, repeat captcha/Argon2/database work, and tie account latency to SMTP.
  - Confirmed email change performs a non-atomic cache `get`/old-ticket remove/new-ticket
    set/current-pointer set before synchronous SMTP. Concurrent requests can each send a
    different link while only the last pointer remains current; failure cleanup from
    one generation can also leave another delivered link with no current pointer. The
    profile modal never disables its own Confirm button, so this race is reachable from
    an ordinary rapid click as well as multiple tabs.
  - Record a canonical mail intent in a bounded transactional outbox before responding.
    Key it by purpose, account/security-policy generation, normalized destination, and
    an opaque client operation ID/request digest; an exact retry must return/reconcile
    the same intent and usable token rather than invalidating it or enqueueing another
    message. A deliberate new generation must atomically supersede the old one and make
    that choice visible, without revealing whether an anonymous recovery address
    exists.
  - Drain the outbox through one shared, bounded SMTP worker pool with explicit queue
    byte/count limits, connection/concurrency limits, deadlines, capped jittered retry,
    terminal/dead-letter state, and graceful shutdown/restart recovery. Enforce
    deployment-wide per-source, per-account/destination-digest, and aggregate admission
    before token or message construction; return/honor `Retry-After` while preserving
    enumeration-resistant responses.
  - Give registration, recovery, and confirmed email change one ref-backed form
    operation acquired before captcha, Terms, fingerprint, password verification, or
    API work. Use native submit semantics, keep the operation ID stable across an
    ambiguous response, disable every activation path, and offer an explicit later
    resend rather than silently racing generations.
  - Add double-click, Enter, click-plus-submit, two-tab, same/different operation,
    lost-response-after-outbox-commit, SMTP hang/outage/recovery, replica restart,
    queue-full, many-source/account, superseding generation, and old/new-link tests.
    Assert bounded tasks/connections/rows, one message and one usable link for an exact
    intent, prompt overload behavior, and unaffected login/event latency.
  - Relevant code: `web/src/pages/account/Register.tsx`,
    `web/src/pages/account/Recovery.tsx`, `web/src/components/AccountView.tsx`,
    `src/controllers/account/mod.rs`, `src/controllers/account/recovery.rs`,
    `src/controllers/account/email_confirmation.rs`, `src/services/mail.rs`, and a new
    registered idempotent forward migration for mail intents/outbox delivery.

- [x] Make the platform-settings save one bounded, revisioned operation.
  - The settings page sends every configuration section even when one field changed.
    `update_config` commits the account/captcha/OAuth group first, then applies global,
    container, email, registry, build-registry, and provider keys through many
    independent `find_by_id` plus insert/update calls; donations use another transaction.
    Semantic preflight cannot protect against a database/cache/process failure midway,
    so the endpoint can return an error after a partially live authentication or
    runtime policy has already committed.
  - Branding is a second client request after the configuration response. If logo
    upload fails, the UI reports the whole Save as failed even though every setting is
    live; Retry resends all sections. A committed-but-lost response has the same
    ambiguity. One browser intent can therefore repeat dozens of database operations,
    policy invalidations, and blob work while two stale admin tabs silently overwrite
    each other's complete snapshots.
  - The DTOs and controls also lack authoritative per-field UTF-8 byte and aggregate
    response budgets. Values such as global title/description/footer flow into the
    public configuration read used across the SPA, so an oversized accepted value
    amplifies every later client bootstrap even though the original request consumed
    one global-rate token.
  - Validate and canonicalize every supplied field, secret-preservation rule, URL,
    count, and byte budget before mutation. Apply all relational keys with bound raw
    `sqlx` in one transaction under the existing security-policy lock where applicable,
    using set-based `INSERT ... ON CONFLICT`; publish cache/policy invalidation once only
    after commit. A failed Nth write must leave the entire prior revision visible.
  - Add a monotonically increasing settings revision and require `If-Match` (or its wire
    equivalent) plus a stable client operation ID. Persist the operation/result with the
    revision so an exact ambiguous replay is a no-op and a stale/different edit receives
    `409` with the current revision instead of overwriting it.
  - Stage an optional logo under that operation and consume its blob reference exactly
    once when publishing the revision, with bounded cleanup of abandoned staging. If
    cross-store atomicity is impractical, expose a durable job whose status identifies
    which revision and branding hash became authoritative; never report a generic Save
    failure after one half committed.
  - Send only dirty sections from the client, guard Save synchronously with one
    ref-backed owner, retain the operation ID through timeout/reload, and reconcile the
    authoritative revision/result before offering Retry. Keep all controls tied to the
    same pending generation so keyboard activation cannot bypass it.
  - Add failure-at-every-write, logo storage/commit failure, lost/reversed response,
    rapid click/Enter, two-tab/admin, stale secret placeholder, oversized/multibyte
    fields, Redis/cache outage, restart, and public-bootstrap load tests. Assert either
    the complete old or complete new revision, one blob acquisition/invalidation, and
    bounded queries, bytes, and latency per operation.
  - Relevant code: `web/src/pages/admin/Settings.tsx`,
    `src/controllers/admin/settings.rs`,
    `src/controllers/admin/settings/security_policy.rs`,
    `src/services/blob_refs.rs`, and a new registered idempotent forward migration for
    settings revisions/operations and staged branding.

- [x] Make temporary Event-VPN bypass grants exactly-once and bounded.
  - Creating an override always inserts a fresh UUID and has no client operation ID or
    expected policy revision. A second tab/operator or a retry after a committed but
    lost response creates another independently active bypass and invalidates policy
    again; revoking one grant can then leave the unintended duplicate authorizing
    off-VPN access.
  - Expired/revoked rows have no retention budget and the backend imposes no active
    grant count per game. Repeated client/API use can grow the policy table indefinitely
    while the UI returns only the newest 100, hiding older active grants from the same
    management surface.
  - Require an opaque operation ID and observed policy revision, persist it under a
    unique `(game_id, operation_id)` boundary, and return the same grant ID/expiry for
    an exact authorized replay. Reject a stale competing policy transition and
    invalidate the policy cache only when one row actually changes authorization.
  - Enforce a small documented active-grant limit, expose every active grant regardless
    of history pagination, and retain expired/revoked audit rows under an explicit
    archival/retention budget. Make revoke idempotently return the authoritative state
    so an ambiguous response can be reconciled safely.
  - Give create/revoke one ref-backed browser mutation owner and recover the known
    operation before retry. Display the server's current active set and policy revision
    after every timeout or mutation rather than assuming the local request won.
  - Add rapid click/keyboard, two-tab/operator, lost/reversed response, create/revoke
    race, multi-replica, expiry, maximum-active, more-than-100-history, cache
    invalidation, and off-VPN authorization tests. One intent must create one visible
    grant, and revoking it must close that exact authorization boundary.
  - Relevant code: `web/src/pages/admin/games/[id]/Info.tsx`,
    `src/controllers/admin/anti_cheat.rs`,
    `src/services/event_security/policy.rs`, and a new registered idempotent forward
    migration for override operations/revisions and retention indexes.

- [ ] Replace per-challenge browser fan-out with one bounded bulk mutation.
  - “Select all” enable, disable, and delete use `Promise.allSettled` over every visible
    challenge, so one click immediately creates one HTTP request per selected row with
    no client concurrency bound. Selection spans the whole filtered challenge list, not
    a server-capped page.
  - Every update independently acquires the same game-control lock and can reload flags,
    sync VPN state, flush scoreboards, publish notices, and tear down containers. Every
    delete enters the hard-deletion admission/transition stack and may destroy runtimes
    and artifacts. The fan-out turns one organizer intent into a convoy of queued
    handlers and repeats event-wide side effects; an ambiguous retry sends the convoy
    again.
  - Add backend bulk desired-state and bulk-delete job contracts with a strict ID count,
    stable operation ID, expected game/configuration revision, and per-ID authorization.
    Reserve once, acquire game-level control once, validate the complete intent, mutate
    eligible rows set-wise, and coalesce scoreboard/VPN/realtime invalidation once.
  - Process destructive runtime cleanup through the existing bounded deletion domain as
    one resumable job rather than retaining one HTTP request per challenge. Report
    durable per-ID outcomes and recover the same result after timeout; never silently
    replay already-finished deletion work.
  - Replace `Promise.allSettled` with one request owner. As an interim safety boundary,
    cap selection and client concurrency, but keep backend admission/idempotency
    authoritative because multiple tabs and direct callers bypass UI controls.
  - Add 1/100/maximum-selection request-count tests, rapid keyboard/click, two-tab and
    two-organizer, lost/reversed response, partial failure, live container teardown,
    multi-replica, and fixed-rate load tests. One bulk intent must occupy bounded
    handlers/connections and perform one effective event-wide reconciliation.
  - Relevant code: `web/src/pages/admin/games/[id]/challenges/Index.tsx`,
    `src/controllers/edit/challenges/mod.rs`, `src/controllers/edit/deletion_locks.rs`,
    `src/services/challenge_workloads.rs`, and a new registered idempotent forward
    migration for bulk mutation jobs/results.

- [ ] Bound division-policy replacement and skip event-wide work for metadata-only saves.
  - `DivisionEditDrawer` sends the complete `defaultPermissions` and
    `challengeConfigs` set on every edit, including a name or invite-code-only change.
    During live scoring, an unchanged policy passes the equality guard but is still
    deleted/upserted one row at a time while the per-game engine fence is retained.
    After commit, every save loads all challenge IDs, removes one effective-permission
    cache key per challenge serially, and flushes every scoreboard even when no scoring
    field changed.
  - Neither wire model bounds name/invite bytes or configuration rows, rejects duplicate
    challenge IDs, nor validates the permission bitmask. Validation deduplicates IDs only
    for its existence query; application then executes every repeated entry. A compact
    JSON request can therefore turn one global-rate token into thousands of serial
    upserts under the event-control lock, followed by an event-wide cache sweep.
  - Split metadata and policy patches or carry explicit changed fields. Normalize and
    validate a strict count/UTF-8-byte/permission budget before acquiring the game lock,
    reject duplicates, and compare the canonical desired set with the locked current
    revision. Return immediately for a no-op; update metadata without rewriting policy;
    replace a real policy change with bounded set-based `UNNEST`/upsert/delete SQL.
  - Require the observed division revision plus a stable operation ID. An exact retry
    after an ambiguous response must recover the same revision/result without repeating
    upserts or invalidation, while a conflicting stale editor receives `409`. Increment
    the policy revision only for a real policy change and publish one compact change.
  - Replace per-challenge cache deletion with a revisioned namespace or one bounded
    invalidation primitive. Flush scoreboards once only when a visible name or scoring
    value actually changed. Load the division list through one bounded aggregate query
    rather than one configuration query per division, and page/cap both dimensions.
  - Have the drawer send only actual changes, keep a ref-backed mutation owner, and
    reconcile the authoritative revision after timeout instead of resending the whole
    stale set. Display backend row/field limits before allocation.
  - Add metadata-only, exact no-op, maximum/too-many/duplicate configs, invalid mask,
    oversized UTF-8 fields, rapid submit, two-tab stale revision, lost response,
    multi-replica, live-scoring, large-event query-count, cache-operation-count, and
    fixed-rate tests. Assert bounded lock time and one effective invalidation per change.
  - Relevant code: `web/src/components/admin/DivisionEditDrawer.tsx`,
    `web/src/pages/admin/games/[id]/Divisions.tsx`,
    `src/controllers/edit/divisions.rs`, `src/controllers/edit/mod.rs`, and a new
    registered idempotent forward migration for division revisions/operations.

- [ ] Make team invite rotation one credential transition and one BYOC reconciliation.
  - The captain's invite-code Refresh action never enters a busy state, so rapid clicks
    can issue concurrent PUTs; another tab or a retry after a lost response does the
    same. Every serialized request succeeds with a different random token, and an older
    response can arrive last and display an invite code that is already invalid.
  - Each rotation loads every participation for the team and calls
    `disconnect_participation` serially. Each call revokes tunnels, updates self-hosted
    service rows, waits for endpoint shutdown, and invokes VPN hub synchronization.
    The roster fence is released before this cleanup loop, so repeated rotations can
    overlap their revocation/VPN work, multiply event-wide rebuilds, and continuously
    knock the team's live BYOC services offline.
  - Initial code loading runs even while the retained modal is closed, has no error
    handler or request generation, and the Refresh handler refuses to run while the
    code is empty. One transient GET failure can therefore leave the captain with a
    permanently blank, non-retryable control until the component happens to remount.
  - Require a stable operation ID plus the invite revision observed by the captain.
    Atomically commit one new revision under the roster fence and return the same code
    for an exact authorized replay; reject a competing stale revision before generating
    or revoking anything.
  - Replace the per-participation loop with one bounded team-level revocation that
    updates all affected service rows set-wise, closes local endpoints with bounded
    concurrency, publishes bounded cross-replica invalidations, and performs one VPN
    synchronization after the committed transition.
  - Give the modal a ref-backed mutation owner and response generation, disable Refresh
    immediately, and recover the known operation after timeout. Only the newest
    committed revision may update the displayed/copyable link. Gate the initial secret
    read on an open modal, abort it on team/account/close changes, and expose a safe GET
    retry separately from the destructive rotation action.
  - Add rapid click/keyboard, two-tab/captain, lost/reversed response, multi-replica,
    large participation history, active BYOC tunnels, and VPN outage tests. Assert one
    token revision, one aggregate service update/sync, one valid displayed code, and
    bounded tunnel shutdown and request latency.
  - Relevant code: `web/src/components/TeamEditModal.tsx`,
    `src/controllers/team/mod.rs`, `src/controllers/team/revocation.rs`,
    `src/services/byoc_tunnel/mod.rs`, and a new registered idempotent forward migration
    for team credential revisions/operations.

- [ ] Bound bulk flag edits and remove their challenge-detail amplification loop.
  - Both flag-entry dialogs turn every nonempty input line into one `FlagCreateModel`,
    and `add_flags` accepts the complete JSON vector without row, flag-byte, URL-byte,
    or duplicate limits. It builds optional attachments serially before admission, then
    retains the definition transaction while inserting every flag one at a time.
  - The request has no operation identity or `(challenge_id, flag)` uniqueness boundary.
    A lost-response retry can add duplicate grading rows and attachments while repeating
    all staging and inserts. A large paste can also hold browser state and a database
    connection for work bounded only indirectly by the generic body limit.
  - `load_flags` later reads every flag and performs attachment plus local-file lookups
    per row. The challenge-detail screen polls that full projection every two seconds
    while a build is active, so a large flag import feeds directly into an `O(flags)`
    response and `N+1` query loop even though only build status changed.
  - Enforce backend row and field-byte limits, normalize/deduplicate before side effects,
    and add an idempotent unique index for the chosen flag identity. Reserve an operation
    before attachment work, use atomic/set-based inserts with an explicit duplicate
    result, and clean or reuse staged attachments on retry and cancellation.
  - Fetch flags with one bounded join and page them for editing. Move build progress to
    the compact status owner already required above so it never reloads plaintext flags,
    attachments, test-container state, or storage capability on every tick.
  - Show the accepted limit/count in both dialogs, reject over-limit paste before
    allocating the payload, use one submit owner, and recover the operation rather than
    blindly reposting it.
  - Add maximum/minimal-line, oversized flag/URL, duplicate-in-body/existing, rapid
    submit, lost response, attachment failure/cancellation, many-flag query-count,
    active-build polling, and grading-semantic tests. Bound inserts, attachments,
    queries, response bytes, transaction time, and request count.
  - Relevant code: `web/src/components/admin/FlagCreateModal.tsx`,
    `web/src/components/admin/AttachmentRemoteEditModal.tsx`,
    `web/src/pages/admin/games/[id]/challenges/[chalId]/Index.tsx`,
    `src/controllers/edit/flags.rs`, `src/controllers/edit/helpers.rs`,
    `src/controllers/edit/challenges/mod.rs`, and a new registered idempotent forward
    migration for flag identity/import operations.

- [ ] Enforce compatible canonical flag-byte invariants from authoring through grading.
  - Normal submission rejects an answer over 127 UTF-8 bytes, but direct static-flag
    creation has no value bound, pending Git manifests allow 4 KiB flags, trusted Git
    imports skip that policy entirely, and game archive imports store flags verbatim.
    All of those accepted definitions can therefore create a challenge whose exact
    correct answer the player API will always reject.
  - Dynamic templates are checked only for a randomness placeholder. Their input and
    worst-case expanded output are unbounded, including repeated `[GUID]`, `[UUID]`,
    and `[TEAM_HASH]` tokens, so a valid-looking template can generate the same
    impossible answer. The generated value then spreads into `FlagContexts`, workload
    environment/configuration, and proxy egress scanning. A&D warmup provisioning also
    honors the template even though scored rounds currently generate an exact 38-byte
    grammar; BYOC independently permits as much as 4 KiB, leaving incompatible limits
    at adjacent producer/consumer boundaries and weak defense against invalid legacy
    data.
  - This is also a resource-amplification boundary. `FlagContexts.flag` is indexed by a
    PostgreSQL btree, so sufficiently large values can fail imports at index insertion;
    every admitted proxy socket clones a stored flag and reserves `flag.len() - 1`
    overlap bytes. A bad definition plus reconnect churn multiplies that memory. A&D
    submit rejects malformed values before SQL, but still retains and echoes each raw
    value while its weighted limiter charges a whole oversized malformed batch as one
    unit, allowing the generic JSON-body ceiling to become request/response bandwidth
    and allocation amplification.
  - Define one shared flag-policy validator family: the normal-answer byte envelope
    must match its 127-byte grading ceiling, while A&D retains its stricter exact
    38-byte grammar. Use the applicable policy before a flag is stored, generated,
    delivered, matched, or submitted. Validate a template's worst-case expanded output
    with the production expansion logic and all placeholder occurrences, and reject
    each malformed A&D value before copying it into results or work maps.
  - Apply the boundary to direct add/update, pending and trusted repository import and
    refresh, game archive import, challenge clone, runtime provisioning, A&D/KotH and
    BYOC delivery, proxy matcher construction, and player submissions. Put an explicit
    small body/per-value budget on A&D submit, charge admitted bytes as well as distinct
    plausible flags, and return a bounded ordinal/status for rejected entries rather
    than reflecting attacker-sized strings.
  - Audit existing rows before enforcement. Disable and report affected challenges
    rather than truncating a secret and silently changing its answer, then add a
    registered idempotent forward migration with a byte-length check on stored static
    flags. Have runtime consumers fail closed with a metric if legacy invalid data is
    encountered, without allocating from the invalid length or entering a retry loop.
  - Add 127/128-byte ASCII, multibyte UTF-8, repeated-placeholder expansion, direct
    editor, pending/trusted Git, archive import, clone/legacy-row, index insertion,
    Jeopardy round-trip, A&D/BYOC delivery, malformed A&D batch, and many-proxy-session
    tests. Assert every accepted definition is submittable and bound response bytes,
    allocations, database work, reconnect work, and delivery retries.
  - Relevant code: `src/controllers/game/mod.rs`,
    `src/controllers/game/submit.rs`, `src/controllers/game/ad/submit.rs`,
    `src/controllers/edit/flags.rs`, `src/controllers/edit/challenges/mod.rs`,
    `src/controllers/edit/transfer.rs`, `src/controllers/edit/transfer_import.rs`,
    `src/services/git_sync/policy.rs`, `src/services/git_sync/mod.rs`,
    `src/utils/flag_generator.rs`, `src/services/byoc_tunnel/flag.rs`,
    `src/controllers/proxy/egress.rs`, `src/services/event_security/variants.rs`,
    and a new registered idempotent forward migration.

- [ ] Bound the legacy exercise API so practice traffic cannot consume the event pool.
  - `POST /api/exercise/{id}` is not decorated with the existing Submit policy and
    accepts `FlagSubmit.flag` without the normal 127-byte invariant. Each request trims
    and copies the caller's generic JSON-body-sized string, opens a transaction-backed
    per-user advisory lock, then fetches every eligible static/current exercise flag
    into Rust before comparing. Flag count and response-independent query work are
    unbounded even for an obviously impossible answer.
  - The advisory-lock transaction keeps one pool connection while `user_instance`, flag
    lookup, and solve update/insert acquire other connections through SeaORM/sqlx. Local
    coalescing protects only the same user on one replica; simultaneous submissions from
    distinct users can fill the pool with outer lock transactions whose inner queries
    all need another connection, turning normal practice traffic or a client loop into
    a pool convoy that stalls live-event endpoints.
  - Exercise container create similarly retains a transaction-scoped lock across
    runtime health checks, sequential old-container destruction, flag generation,
    Docker/worker creation, and multiple independent database writes. Its four-slot
    process gate limits one replica but still holds scarce connections during external
    I/O and does not provide a deployment-wide work/byte budget. List reads also load
    every solved instance and every published exercise with no pagination or response
    cap.
  - Apply the canonical normal-flag byte policy before lock or allocation, attach the
    Submit limiter, and add fail-fast per-user/source plus aggregate grading admission.
    Check the caller's current flag or an author-defined static flag with one bounded
    indexed `EXISTS`/conditional solve transaction rather than materializing every
    candidate; make an already-solved replay cheap and idempotent.
  - Run short database-only transitions on the lock owner's connection. For container
    runtime work, use a bounded close-on-drop session lease or durable operation record,
    release database transactions before external calls, and revalidate/publish in a
    short transaction afterward. Coalesce an exact create/destroy operation across
    replicas, cap auto-destroy work per request, and return `Retry-After` instead of
    queueing an unbounded handler.
  - Page the exercise catalog and solved projection in one bounded query. If a practice
    client is restored, give submit/create/destroy one synchronous request owner,
    reconcile ambiguous operations, pause any refresh while hidden/offline, and never
    retry permanent validation responses automatically.
  - Add 127/128-byte and maximum-body flags, many static flags, already solved, rapid
    duplicate submit, many-user pool pressure, multi-replica lock contention, slow/hung
    runtime, create/destroy lost response, maximum auto-destroy backlog, and large-list
    fixed-rate tests. Assert bounded rows/bytes/connections/waiters/runtime jobs and
    responsive event submissions, scoreboard, and `healthz`.
  - Relevant code: `src/controllers/exercise.rs`, `src/utils/single_flight.rs`,
    `src/utils/upload.rs`, `src/middlewares/rate_limiter.rs`, exercise/container models
  and indexes, and the generated exercise client contract in `web/src/Api.ts`.

- [ ] Claim player container operations durably before launching runtime work.
  - Per-team and shared-container creation acquire a transaction-scoped PostgreSQL
    advisory lock and retain that pooled connection while checking runtime health,
    destroying stale workloads, pulling/starting Docker or worker workloads, and
    publishing through additional database connections. The four-slot process gate
    limits one replica, but same-participation/shared-challenge requests on other
    replicas can each occupy a gated connection while waiting for the winner's slow
    external call. A large team, synchronized event start, or client retry regression
    can therefore turn one logical create into a cross-replica pool/runtime convoy.
  - The runtime is created before its `Containers`/`GameInstances` ownership rows are
    durable, and its random operation ID is not persisted first. If the HTTP future is
    cancelled after the runtime accepts the create but before publication, Rust drops
    the transaction and no request cleanup runs; a retry sees no owner, chooses a new
    operation ID, and can launch another workload. Periodic orphan discovery is only
    eventual cleanup and cannot prevent repeated timeout/retry churn from consuming
    runtime capacity during an event.
  - A committed-but-lost response is ambiguous in the other direction: an identical
    retry reaches the "already exists"/cooldown error instead of recovering the
    authoritative endpoint. Shared-container callers likewise serialize merely to
    refresh the same lease, so a request herd still pays queued authorization and
    database work after the first create succeeds.
  - Reserve a durable operation row in a short transaction before runtime I/O, keyed by
    the canonical participation or shared challenge, expected workload/publication
    revision, intent, and opaque client operation ID. Return or join the active/result
    record for an exact replay; reject a competing intent immediately with `409`/`429`
    plus `Retry-After` instead of waiting on a PostgreSQL advisory lock.
  - Let a bounded, cancellation-independent owner perform the external create/destroy
    under an absolute deadline, using the persisted operation ID for worker/runtime
    idempotency. Publish the exact backend in a second short conditional transaction,
    then mark the result recoverable. On crash or stale lease, reconcile that operation
    ID against the runtime before retrying or reclaiming it; never launch a new backend
    merely because the initiating browser disconnected.
  - Keep one ref-backed client owner for create/delete/extend, preserve the operation ID
    across timeout/reload, and reconcile status before offering Retry. Coalesce the
    shared-container result across callers and make unchanged lease renewal cheap rather
    than queueing every opener behind runtime inspection.
  - Add rapid click/keyboard, many-team-member/tab, multi-replica, event-start herd,
    disconnect before/after runtime acceptance and DB publication, lost response,
    worker timeout/restart, stale-definition race, create/delete reversal, shared
    challenge, and orphan-recovery tests. Assert one runtime operation per accepted
    intent, bounded waiters/connections/jobs, recoverable results, and responsive flag
    submission, scoreboard, and `healthz` under fixed-rate retries.
  - Relevant code: `web/src/components/GameChallengeModal.tsx`,
    `src/controllers/game/containers.rs`,
    `src/controllers/game/containers/shared.rs`,
    `src/controllers/game/containers/publication.rs`,
    `src/controllers/game/containers/reaping.rs`, `src/utils/single_flight.rs`, the
    container backend/worker operation store, and a new registered idempotent forward
    migration for container operations.

- [ ] Put anonymous asset authorization, cache fills, and conditional reads behind
  bounded work admission.
  - Both download routes are under `/assets/...`, while `global_middleware` explicitly
    skips every path outside `/api`. An unauthenticated client can therefore send an
    unlimited sequence of syntactically valid 64-hex hashes without consuming any
    global or named rate budget. Each distinct unknown hash misses
    `assetgate:{hash}:{second}`, starts a detached `SingleFlight` leader, queries
    `ASSET_GATE_SQL` across file/public-owner/attachment/flag/instance/participation
    relations, and writes a two-second cache entry before returning 403.
  - Same-key coalescing does not help hash rotation: `SingleFlight.inflight` has no
    distinct-key or leader cap, and each leader can live for 15 seconds. A client bug
    constructing a new bad hash per render/retry—or a distributed hostile client—can
    therefore grow detached tasks/map entries, database waiters, cache commands/keys,
    and log/response work without ever requesting file bytes.
  - Known small/public assets bypass `asset_download_admission` too. Even a correct
    `If-None-Match` request runs `finalize_public_asset`, opening a transaction and
    checking all public-owner tables before returning 304, so a browser/CDN revalidation
    loop can turn a zero-byte cache hit into sustained PostgreSQL work. The current
    large-stream permit is acquired only after authorization, size/storage preparation,
    cache handling, and range parsing.
  - Apply cheap distributed source/session and deployment-wide request/work admission
    to both `/assets` routes before authentication, cache lookup, PostgreSQL, or storage.
    Charge distinct gate misses, authorization queries, and response bytes separately;
    cover malformed, unknown, small, range, 304, signed-URL, and streamed paths, and
    return `429/503` with `Retry-After` without auditing each overload rejection.
  - Bound distinct in-flight cache-fill keys and detached leaders with fail-fast
    admission acquired before insertion. Negative-cache unknown hashes under a strict
    cardinality/TTL budget (and invalidate on ownership publication), prevent
    caller-controlled keys from growing Redis/local state, and expose hit/miss/reject,
    leader, DB, storage, and byte metrics. Redis failure must fall back to a bounded
    local ceiling rather than removing admission.
  - Publish an invalidatable/versioned public-asset grant with the owner mutation, or
    deliver genuinely public immutable bytes through a static/CDN path whose grant can
    be revoked. A cache-valid public 304 must not open a PostgreSQL transaction per
    request; protected assets must retain their exact live roster/policy recheck. Keep
    one browser request owner per resolved asset and stop retrying permanent 403/404s.
  - Add random-valid-hash rotation, same-hash herd, public 304/small-asset loop,
    protected range loop, revoked owner, cache-expiry boundary, slow database/storage,
    disconnect, Redis outage, multi-replica, and fixed-rate tests. Assert bounded cache
    keys, single-flight entries/leaders, tasks, queries, pool waiters, storage opens,
    bytes, and logs while event submissions, scoreboard, and `healthz` stay responsive.
  - Relevant code: `src/server.rs`, `src/middlewares/rate_limiter.rs`,
    `src/controllers/assets.rs`, `src/controllers/assets/authorization.rs`,
    `src/utils/single_flight.rs`, the asset URL consumers in `web/src`, and the cache/
    download admission services.

- [ ] Make generic asset upload ownership atomic with the attachment that consumes it.
  - All three local-attachment flows first call `POST /api/assets`, which stores every
    body and increments `Files.reference_count`, and only afterward call either
    `editUpdateAttachment` or `editAddFlags` with the returned hash. If the second call
    fails, the tab closes, or the upload response is committed but lost, no owner is
    linked and a retry acquires the same hash again. There is no compensating release.
  - The leak is durable by design: `purge_pending` selects only zero-reference rows, and
    attachment reconciliation explicitly treats an unowned positive-reference `Files`
    row as a deliberate standalone asset. Repeated identical bodies inflate the count
    forever; changed/random bodies consume object storage. A multi-file upload also
    commits one independent storage/SQL transaction per file, so a later store failure
    leaves all earlier files acquired even before the attachment request begins.
  - `AttachmentUploadModal` has no file-count cap, and the backend bounds only aggregate
    bytes. One multipart request can therefore contain many tiny nonempty parts and run
    a serial object-store write plus transaction per part while consuming one global
    HTTP-rate token. A looping client can repeatedly spend storage, hash-lock, and pool
    work despite the existing 192-MiB request and process-memory bounds.
  - Prefer one domain-owned multipart endpoint that validates metadata, part count,
    per-name bytes, aggregate bytes, challenge policy, and authorization before storage,
    then atomically publishes the attachment/flag rows with exactly one blob reference.
    If the public API must stay two-step, create a short-lived upload lease keyed by
    authenticated owner, opaque client operation ID, ordered manifest, and content
    digest; consuming it must be atomic and exactly once, and an exact upload retry must
    return the same hashes without incrementing references.
  - Put a hard backend file/field-count and total-work limit before buffering or storing;
    use bounded set-based metadata work and deployment-wide weighted storage admission.
    An RAII worker plus startup/cron sweep must release every unconsumed/failed lease on
    validation error, later-part failure, disconnect, timeout, crash, or expiry. Do not
    let the browser blindly call hash deletion after an ambiguous attach response,
    because the attach may have committed or another owner may share the content.
  - Show the server limits in each editor, reject count/byte overflow before building
    `FormData`, keep one ref-backed upload/consume owner, preserve its operation ID across
    timeout recovery, and abort superseded reads without treating abort as rollback.
  - Add zero/tiny/max/too-many parts, duplicate hashes, later-part storage failure,
    upload-success/attach-failure, lost response at both commits, cancel/disconnect,
    expiry, crash/restart, two-tab, multi-replica, and fixed-rate tests. Assert one
    logical reference per published owner, no positive-reference orphan after lease
    expiry, bounded object writes/transactions/memory, and responsive event traffic.
  - Relevant code: `web/src/components/admin/AttachmentUploadModal.tsx`,
    `web/src/pages/admin/games/[id]/challenges/[chalId]/Flags.tsx`,
    `src/controllers/assets.rs`, `src/controllers/edit/flags.rs`,
    `src/controllers/edit/challenges/attachments.rs`, `src/services/blob_refs.rs`,
    `src/utils/upload.rs`, and a new registered idempotent forward migration for upload
    leases/operations.

- [ ] Stop holding PostgreSQL transactions and event-control locks across blob-store
  writes.
  - `store_and_acquire_in_transaction` takes a transaction-scoped content-hash lock and
    then awaits `storage.store`. Account/team avatars, game posters, and branding call
    it after locking their owner rows; team avatars additionally retain the live-roster
    transaction, and poster changes retain the game control fence. A slow or hung local,
    S3, or compatible object store therefore consumes a pool connection and serializes
    unrelated account, roster, game, or configuration mutations for the entire upload.
  - Writeup replacement is event-critical and worse: it opens a transaction, takes the
    team's shared roster advisory fence, revalidates/locks the game, participation, and
    old/new hashes, and only then stores as much as 20 MiB. Repeated uploads each cost
    one generic HTTP token while holding those locks; the process buffer semaphore
    bounds resident bodies but not database-lock duration or deployment-wide storage
    work. Roster changes and writeup/event operations can convoy behind object-store
    latency and exhaust the pool across distinct teams.
  - Stage immutable bytes outside the domain transaction under fail-fast,
    deployment-wide weighted byte/concurrency admission and an absolute storage
    deadline. Protect the store-before-metadata gap with a durable staging lease or
    temporary namespace keyed by owner, opaque client operation ID, ordered content
    digest, and expiry; deletion/reconciliation must respect that lease, and a bounded
    sweeper must reclaim abandoned bytes after crash or cancellation.
  - After staging succeeds, open one short transaction, reacquire the canonical
    roster/game/owner/hash lock order, revalidate the exact security stamp,
    participation/deadline/config revision, and atomically publish or swap one logical
    reference. An exact operation replay must return the committed hash/result without
    storing or incrementing again; a stale owner revision must reject and release the
    stage. Never await object storage while a PostgreSQL transaction or domain lock is
    held.
  - Keep cleanup crash-safe: publish/promotion failure retains a bounded retryable stage,
    post-commit old-object deletion uses the existing zero-reference tombstone, and
    same-content concurrent stages deduplicate without allowing a sweeper to delete a
    newly published blob. Apply per-purpose file/count/byte budgets before reading the
    body and return overload/timeout with `Retry-After`.
  - Add slow/hung/failing storage, writeup-at-deadline, concurrent roster/game/account
    mutation, same/different content, lost response before/after publish, disconnect,
    crash/restart, lease expiry/sweep, deletion race, multi-replica, and fixed-rate
    upload tests. Assert no transaction or roster/game fence spans storage I/O, bounded
    store jobs/bytes/pool use, one logical reference per operation, no durable orphan,
    and responsive event traffic.
  - Relevant code: `src/services/blob_refs.rs`,
    `src/services/blob_refs/writeups.rs`, `src/controllers/game/writeup.rs`,
    `src/controllers/account/avatar.rs`, `src/controllers/team/avatar.rs`,
    `src/controllers/edit/reviews.rs`, `src/controllers/admin/settings.rs`, storage and
    upload admission services, and a new registered idempotent forward migration for
    staged blobs/operations.

- [ ] Keep traffic-capture archive admission alive until the last response byte.
  - “Download all” opens a fresh archive request on every click with no browser request
    owner. The server correctly caps an archive at 256 files/128 MiB and admits only two
    ZIP builders, but it materializes each ZIP as a `Vec<u8>` and drops the semaphore
    when the handler returns the response, before a slow client has consumed that body.
  - Repeated clicks/tabs or slow connections can therefore let completed 128 MiB bodies
    accumulate outside the advertised two-slot bound while new ZIP jobs reuse the
    permits. The process-local semaphore also does not cap aggregate retained bytes
    across replicas.
  - Stream ZIP output through a bounded channel/body instead of returning one complete
    vector. Acquire deployment-wide weighted admission before scanning/opening files,
    move an owned memory/work permit into the response stream, and release it only on
    EOF, disconnect, or cancellation. Preserve the existing file/byte caps and recheck
    growth while streaming.
  - Rate-limit and optionally single-flight the same authorized `(challenge,
    participation, capture-version)` export. Reject overload before disk work with
    `Retry-After`; ensure queued jobs and temporary output have explicit byte, time, and
    retention bounds.
  - Replace the unobservable `window.open` action with one ref-backed export/download
    owner that disables duplicate intent and reports busy/failure state. Backend
    admission remains authoritative for multiple tabs and direct requests.
  - Add rapid-click, many-tab, multi-replica, maximum-file/byte, growing-file,
    slow-reader, disconnect, storage-error, and fixed-rate tests. Assert bounded ZIP
    workers, retained response bytes, file descriptors, disk throughput, and memory
    while event traffic and `healthz` remain responsive.
  - Relevant code: `web/src/pages/games/[id]/monitor/Traffic.tsx`,
    `src/controllers/game/traffic.rs`, the response-stream/admission utility, and the
    monitor/download rate-limit policy.

- [ ] Stop game-control mutations from reserving the PostgreSQL pool while waiting for
  nested checkouts.
  - `GameControlLock` owns a transaction-scoped advisory lock and therefore one pooled
    connection. Its local coalescer protects one game key, but there is no aggregate
    admission across distinct games. Several handlers then call SeaORM or helpers on
    `st.db`/`st.pg()` while that guard remains live, requiring a second connection for
    the same request.
  - Concrete paths include challenge create/update (`am.insert`, `load_challenge`, and
    `am.update`), game deletion (`load_game` after the lock), challenge rejection/toggle
    (updates and control cleanup outside the lock transaction), and roster-guarded team
    profile/invite updates. The roster path has a small protective semaphore, but the
    game-control path does not; both also expose split-commit ambiguity.
  - With enough concurrent operations on distinct games, every request can acquire its
    first transaction and then wait for a second checkout. The 10-second pool acquire
    timeout eventually breaks the cycle, but until then ordinary submissions, polling,
    workers, and readiness dependencies can be starved. Per-account request limits do
    not bound concurrent retained connections across operators, accounts, or replicas.
  - Make every database read/write protected by a transaction advisory lock accept and
    use that exact `&mut PgConnection`; replace in-lock SeaORM model operations with
    bound raw `sqlx` and explicit row-to-DTO conversion. Commit one atomic unit, release
    the connection, and only then run bounded cache/network/runtime side effects. Apply
    the same rule to `RosterMutationLock` even where its admission currently masks the
    pool risk.
  - Where a truly unavoidable nested resource remains, take a fail-fast aggregate
    work/connection permit *before* the first checkout, size it from guaranteed pool
    headroom, give acquisition a short deadline, and return `503/429` with
    `Retry-After`. Never solve the issue by increasing pool size or retaining a
    transaction across external I/O.
  - Give organizer clients one request owner per canonical game/resource mutation,
    coalesce bulk work server-side, and stop scheduling new work on overload until the
    advertised retry time. This is a usability layer around authoritative server
    admission, not the connection-safety boundary.
  - Add real-PostgreSQL tests using a deliberately small pool and many distinct game
    keys, same-game waiters, slow queries, cancellation at each stage, commit failure,
    multi-replica locks, and concurrent event reads/`healthz`. Assert forward progress,
    one connection per short mutation, no partial commit, bounded pool waiters, and
    prompt overload responses.
  - Relevant code: `src/services/ad/engine/koth_auth.rs`,
    `src/utils/single_flight.rs`, `src/controllers/edit/challenges/mod.rs`,
    `src/controllers/edit/challenges/review.rs`, `src/controllers/edit/games.rs`,
    `src/controllers/edit/ad/mod.rs`, `src/controllers/team/revocation.rs`,
    `src/controllers/team/mod.rs`, and `src/extensions/database.rs`.

- [ ] Make event-settings saves revisioned, no-op aware, and independent of VPN
  reconciliation success.
  - `PUT /api/edit/games/{id}` accepts a complete mutable game model without an
    expected configuration revision. Two open editors can therefore overwrite each
    other's title, schedule, writeup, scoring, or Event-VPN fields with stale snapshots;
    the per-game lock orders requests but cannot detect the stale intent.
  - After the database transaction commits, every request invalidates the game cache,
    flushes every scoreboard family, reloads the row, requests a new global Event-VPN
    reconciliation generation, and waits for that generation to apply. This happens
    even for an exact no-op replay whose settings and VPN policy did not change.
  - A row reload, pool failure, VPN-owner outage, kernel reconciliation error, request
    cancellation, or lost response after commit makes the browser see a failed save
    even though the settings are durable. Retrying the same full stale payload repeats
    the scoreboard/VPN work and can erase a newer operator's update. A buggy client can
    use otherwise valid repeated saves to keep global VPN reconciliation and event
    cache rebuilds busy.
  - Add one general game-configuration revision distinct from the narrower VPN-policy
    audit revision. Require the editor's observed revision and a stable operation ID
    with canonical request digest/result; return `409` for a stale revision and replay
    the original committed result for an exact operation retry.
  - Compute a field-level diff under the existing game-control transaction. Return the
    current row immediately for a true no-op without advancing a revision, invalidating
    a cache, reopening rollups, or requesting VPN work. Increment once and enqueue only
    the post-commit effects selected by a real diff (for example, VPN reconciliation
    only for route/policy changes and scoreboard invalidation only for board-visible
    changes).
  - Persist required post-commit work as bounded, coalesced desired-state generations
    keyed by game/revision and let a reconciler retry it independently. The HTTP result
    must remain recoverable once the database commit succeeds; cache or kernel repair
    failure must not turn that commit into an ambiguous mutation response.
  - Give the editor a ref-backed single-flight owner, retain its operation ID across a
    timeout, and reconcile the known operation/revision before enabling another save.
    Merge or explicitly reject a newer server revision instead of resetting the draft
    from a late revalidation.
  - Add rapid click/keyboard, lost response before/after commit, exact replay, two stale
    editors, reversed responses, no-op, metadata-only, schedule-only, VPN-only,
    VPN-owner outage, cache failure, multi-replica, and fixed-rate tests. Count database
    revisions, scoreboard invalidations, reconciliation generations, and kernel passes;
    one committed semantic change may produce each selected effect at most once.
  - Relevant code: `web/src/pages/admin/games/[id]/Info.tsx`,
    `src/controllers/edit/games.rs`, `src/services/ad/vpn/reconcile.rs`,
    `src/services/ad/vpn/coordination.rs`, `src/controllers/edit/helpers.rs`, and a new
    registered idempotent forward migration for game configuration revisions and
    operation results.

- [ ] Enforce the live-team mutation freeze on the server and bound scoreboard
  invalidation from profile churn.
  - `TeamEditModal` disables name, bio, avatar, and Save while `team.locked`, but the
    authority does not match the UI. `PUT /api/team/{id}` checks only the deletion
    fence and captaincy, while the avatar route rechecks only captaincy and
    `deletion_pending`; neither rejects a locked team participating in an active game.
    A stale/modified client can therefore rename or re-avatar a team during live play.
  - Invite rotation has the same missing authority check. The modal disables Refresh
    for a locked team, but `PUT /api/team/{id}/invite` checks only deletion and
    captaincy. Calling it directly during an active event rotates the credential and
    then disconnects every participation's BYOC tunnels, so a client bug or captain can
    repeatedly disrupt its live services and force the VPN/reconciliation work
    described in the invite-rotation task below.
  - The Save handler has no pending/ref guard and its button never uses the page's
    `disabled` state. Rapid activation can enqueue several serialized updates before a
    render changes anything. The endpoint also has no profile revision or operation
    identity, so two captain tabs and lost-response retries cannot distinguish stale,
    duplicate, and intentional edits.
  - Every distinct rename synchronously loads the team's complete participation
    history and invalidates every standard, KotH, A&D, and combined scoreboard. Each
    A&D hard invalidation additionally executes `UPDATE "Games" SET id = id` as a
    revision barrier before multiple cache deletes. Team history has no corresponding
    bound, so an ordinary captain request can amplify into many PostgreSQL writes and
    cache operations; repeated alternating names can keep live boards cold. Avatar
    changes have the opposite correctness bug: they do not invalidate those cached
    boards at all, so spectators can retain the old identity through event close.
  - Enforce the same locked-and-active predicate used by roster/captain transfer under
    the roster and ordered game fences for every profile, avatar, and invite-credential
    player mutation. Decide and test whether bio-only edits are non-visible and may
    remain allowed. If administrators need an emergency override, expose it as a
    separate explicit, audited, revisioned action rather than trusting the client-side
    disabled control.
  - For multipart avatar uploads, do a cheap authorization/freeze preflight before
    reading the body, acquire bounded upload admission, and repeat the authoritative
    predicate under the final roster/game fences so a race is safe without making a
    known-frozen request buffer megabytes first.
  - Add a team-profile revision plus stable operation ID/request digest. Update the
    profile and avatar reference through bound SQL on the exact roster transaction,
    reject stale revisions, replay exact operations, and make true no-ops produce no
    invalidation. Keep blob storage outside retained database/game locks as required by
    the storage task below.
  - Replace the synchronous history walk with one bounded, coalesced invalidation
    generation keyed by `(team_id, profile_revision)`. Process affected games in
    bounded database batches (or make board cache keys depend on a compact team-profile
    version), collapse superseded revisions, and preserve an immutable historical/final
    board policy explicitly. Do not let one HTTP request retain unbounded pool/cache
    work merely because the team joined many past events.
  - Apply a low, fail-fast per-team profile/credential mutation budget before body or
    database work, returning `429` with `Retry-After`; retain the normal account/source
    limits as defense in depth. Revision idempotency collapses retries, while this
    semantic-change budget bounds a client that alternates otherwise valid values.
  - Give the modal one ref-backed owner shared by Save and avatar publication; disable
    all relevant inputs/close/actions immediately and recover the known operation after
    timeout. Only the matching newest revision may update the visible profile.
  - Add direct-API locked/live, locked-after-close, unlocked, player/admin, rapid click,
    alternating names, no-op, two-tab stale revision, lost/reversed response, avatar,
    large participation history, multi-replica, and fixed-rate tests. Assert the live
    freeze is authoritative, each semantic profile revision commits once, and request
    latency/database/cache work stay bounded independently of historical game count.
  - Relevant code: `web/src/components/TeamEditModal.tsx`,
    `src/controllers/team/mod.rs`, `src/controllers/team/avatar.rs`,
    `src/controllers/team/roster_policy.rs`, `src/controllers/team/revocation.rs`,
    `src/controllers/game/ad/scoreboard.rs`, and a new registered idempotent forward
    migration for team-profile revisions and operation results.

- [x] Fix the deterministic challenge-deadline React hook-order crash.
  - `ChallengeDeadlineNotice` returns before a later `useMemo` only after its ticker
    crosses the deadline, causing a mounted component to render fewer hooks.
  - Call hooks unconditionally and derive the expired UI afterward.
  - Add a fake-timer component regression that mounts before the deadline and advances
    through it without a React runtime error.
  - Relevant code: `web/src/components/ChallengeModal.tsx`.

### P1 — Recovery and lifecycle correctness

- [ ] Make challenge creation and ordinary edits atomic, revisioned, and safe to retry.
  - `add_challenge` holds the per-game control transaction, but inserts the
    `GameChallenges` row through the separate SeaORM pool connection and only then
    seeds `DivisionChallengeConfigs` through the control transaction. A seed/commit
    failure therefore returns an error after the challenge is already visible, without
    its complete division policy. Retrying the create inserts another challenge.
  - `update_challenge` has the same split commit: `am.update(&st.db)` publishes all
    challenge fields before `seed_division_configs(control.transaction_mut(), ...)`.
    A later failure reports that the save failed even though its main row changed, and
    a competing editor has no revision/precondition with which to detect the partial or
    stale write.
  - `ChallengeCreateModal` uses React state as its only duplicate guard. Rapid
    activation can enter twice before the disabled render commits, while another tab or
    an ambiguous/lost response can resend the same intent. The POST carries no stable
    operation identity, so the server cannot distinguish that replay from a genuinely
    new challenge.
  - Insert the challenge and seed every division configuration on the existing game
    control transaction using bound `sqlx`, then commit once. Add a unique, opaque
    create-operation ID scoped to the authorized actor/game plus a canonical request
    digest and persisted result ID; an exact replay returns the same resource, while
    reusing an ID with different input is a conflict.
  - Give challenge definitions an explicit revision. Require the revision observed by
    an editor, update the row and missing division configs in the same short transaction,
    increment once, and return `409` for a stale save. Treat notifications, scoreboard
    invalidation, repository push-back, and VPN/runtime reconciliation as post-commit,
    bounded idempotent work keyed by that committed revision so their failure cannot
    turn a successful definition commit into an ambiguous HTTP mutation.
  - Use a ref-backed create/save owner in the client, abort it on game/modal/unmount
    changes, and keep the operation ID stable across timeout recovery. Reconcile that
    known operation or authoritative revision before allowing another request; only the
    newest generation may navigate, update local state, or show success.
  - Add real-PostgreSQL failure-injection tests before/after challenge insert, division
    seeding, commit, and response serialization; also cover rapid click, Enter/click,
    two tabs/operators, exact and conflicting operation replays, stale revisions,
    multi-replica ordering, cancellation, and event-start contention. Assert one
    challenge, a complete division-policy set, one revision, and at most one bounded
    post-commit effect per revision.
  - Relevant code: `web/src/components/admin/ChallengeCreateModal.tsx`, the challenge
    editor under `web/src/pages/admin/games/[id]/challenges/[chalId]/`,
    `src/controllers/edit/challenges/mod.rs`, `src/controllers/edit/helpers.rs`,
    `src/utils/single_flight.rs`, and a new registered idempotent forward migration for
    challenge revisions/create operations.

- [ ] Make team, game, and post creation recover the original result instead of
  duplicating records.
  - `TeamCreateModal`, `GameCreateModal`, and the new-post editor use component state as
    their only in-flight guard. A second activation before React commits that render,
    another tab, or a retry after an ambiguous response sends an indistinguishable new
    POST. Each backend generates a fresh database identity and has no operation key.
  - Team creation does serialize on the creator account and cap captained teams at
    three, but identical retries are still treated as three intentional teams. A single
    lost-response/retry sequence can therefore consume the player's creation allowance
    with duplicate name/bio rows. Game and post retries similarly leave extra event
    templates/signing keys or independently generated eight-character post IDs.
  - The post editor's “save and view” confirmation starts `onUpdate()` without awaiting
    it and navigates immediately. The destination can fetch/render the old post before
    the save commits, while a failed save is reported after the editing context has
    already disappeared.
  - Accept a stable opaque operation ID on each create, scoped to the authenticated
    actor and resource kind (plus game where applicable), and atomically persist its
    canonical request digest and resulting resource ID with the insert. Return the
    original authorized resource/result for an exact replay and reject an ID reused
    with different content. Bound retention and actor operation count so this ledger
    cannot become unbounded storage.
  - Give each modal/editor a ref-backed owner acquired before validation/API work,
    retain the same operation ID while reconciling a timeout, and ignore stale response
    generations. Disable every activation/close path synchronously. Await post save and
    confirmed cache publication before navigating; remain in the editor on failure.
  - Add rapid click/keyboard, two-tab, exact/different-content replay, lost response
    before/after commit, reversed response, unmount/navigation, user with two existing
    teams, multi-replica, operation expiry, and ledger-bound tests. Assert one intended
    resource, one returned identity, and no consumed team slot for a replay.
  - Relevant code: `web/src/components/TeamCreateModal.tsx`,
    `web/src/components/admin/GameCreateModal.tsx`,
    `web/src/pages/posts/[postId]/Edit.tsx`, `src/controllers/team/mod.rs`,
    `src/controllers/team/account_lifecycle.rs`, `src/controllers/edit/games.rs`,
    `src/controllers/edit/posts.rs`, and a new registered idempotent forward migration
    for bounded create-operation results.

- [ ] Commit emailed account-link consumption with the account mutation and replay its
  terminal result.
  - `Confirm` and `Verify` use a delayed React `disabled` state as their only duplicate
    guard. Two rapid submissions can reach the server: one commits, while the other
    observes the now-consumed/invalidated credential and reports failure. The user can
    see an “invalid link” notification for an email/account change that actually
    succeeded, especially when responses arrive in reverse order.
  - Registration verification changes the security stamp, so the same signed link is
    deliberately invalid immediately after success and has no persisted terminal
    result to recover after a lost response. Email-change confirmation is more fragile:
    it removes both the current-generation pointer and ticket from the cache *before*
    `update_email_serialized` starts/commits its database transaction. A pool error,
    uniqueness race, cancellation, or commit failure permanently strands the delivered
    link even though the account email did not change.
  - Store only a digest of each bounded confirmation credential in a durable table with
    purpose, account/security generation, destination digest, expiry, attempt/result
    state, and retention limit. Atomically validate and consume it in the same short
    transaction that updates the account/security stamp; persist a safe terminal result
    so an exact credential replay after an ambiguous response returns the same outcome
    without repeating the mutation. Preserve enumeration resistance and never store or
    redisclose the plaintext token.
  - Tie generation/supersession to the durable mail intent rather than a separately
    updated cache pointer. A deliberate newer link must supersede the older generation
    atomically; failure to deliver the new message must not destroy the last explicitly
    supported recovery choice.
  - Give both link pages one ref-backed submit owner, native form semantics, and a
    response generation. Disable all activation immediately, abort/ignore stale work on
    route-token changes or unmount, and reconcile the exact link result before showing
    failure or allowing retry.
  - Add rapid click/Enter, two-tab, reversed response, same/newer link, lost response,
    cancellation, database failure before update/commit, uniqueness race, expiry,
    supersession, Redis loss, multi-replica, and enumeration tests. Assert one account
    transition and one stable safe result for every exact replay.
  - Relevant code: `web/src/pages/account/Confirm.tsx`,
    `web/src/pages/account/Verify.tsx`, `src/controllers/account/recovery.rs`,
    `src/controllers/account/email_confirmation.rs`, and a new registered idempotent
    forward migration for hashed account-link attempts/results.

- [ ] Either authenticate managed API tokens end to end or remove the misleading
  credential surface.
  - `/api/tokens` generates and stores opaque bearer secrets advertised for
    programmatic access, including expiry, revocation, and `last_used_at` metadata.
    No authentication path ever reads `ApiTokens`: global middleware recognizes only
    A&D-prefixed credentials or session JWTs, and the only other table uses are token
    CRUD and creator deletion. Every generated secret is therefore rejected as an
    anonymous/invalid session, `last_used_at` can never change, and restore/expiry have
    no runtime meaning.
  - Preserve the public contract only if the product needs personal/admin API tokens.
    Give them an unambiguous versioned prefix and maximum length, a unique indexed
    digest, explicit least-privilege scopes/audience, a live owner/role/security-stamp
    fence, expiry/revocation enforcement, and a distinct authenticated principal. Do
    not silently turn every legacy row into an unrestricted administrator credential;
    migrate/disable it explicitly and reveal any replacement secret only once.
  - Admit the token's source before digest lookup and apply both per-token and source
    global rate limits after authentication, with bounded negative lookup caching and
    throttled `last_used_at` writes. Keep JWT, team A&D, KotH, worker, and personal-token
    grammars non-overlapping so a rejected credential is never reinterpreted in another
    authority domain.
  - If this credential class is intentionally unsupported, remove the router and
    generated client contract, retire existing rows through a forward migration, and
    explain the supported automation credential instead. In either case, paginate and
    cap management reads, bound token creation per owner, make revoke/restore
    conditional and idempotent, and never restore an already expired credential as if
    usable.
  - Add generated-token round-trip, wrong scope/audience, anonymous, non-admin owner,
    role/stamp change, owner deletion, expiry, revoke/restore, malformed/oversized and
    rotating-invalid bearer, concurrent `last_used_at`, legacy-row migration,
    pagination, and multi-replica rate-limit tests. Assert the documented credential
    either works exactly within scope or no longer exists.
  - Relevant code: `src/controllers/api_token.rs`,
    `src/middlewares/rate_limiter.rs`,
    `src/middlewares/privilege_authentication.rs`, `src/services/token.rs`,
    `src/models/data/content.rs`, `src/server.rs`, the generated `web/src/Api.ts`
    contract, and a new registered idempotent forward migration.

- [ ] Implement the promised Repo Bindings scheduler and remove the useless idle
  poll/N+1 query.
  - The UI says active bindings rescan on the configured cadence and displays
    `nextScanUtc`, but no runtime service reads `status`, `interval_seconds`, or
    `next_scan_utc` to claim due bindings. Only create-with-`runImmediately` and the
    manual Scan button call `run_repo_scan`; Active/Pause and “Next scan” are therefore
    decorative, and upstream challenge changes can remain unapplied indefinitely.
  - The page says it polls to keep `currentActivity` live, but the backend hardcodes
    that field to `None`; the poll can never produce the advertised update.
  - Every tick loads every binding without pagination and then performs one games query
    per binding, serially. The cost grows with repository count even while the page is
    idle.
  - Add a durable, database-time scheduler that atomically claims a small due batch
    across replicas, records a lease/attempt, runs with bounded global and per-host
    concurrency, and advances `nextScanUtc` from completion with jittered failure
    backoff. Pause, deletion, shutdown, lease expiry, and manual scans must reconcile
    with the same checkout fence without duplicating imports or leaving a binding stuck.
  - Replace push-on-edit's untracked `tokio::spawn` per save with a bounded, durable,
    coalescing queue keyed by binding/challenge. A save burst or a response-lost client
    retry currently leaves arbitrarily many tasks waiting on the same checkout lock;
    after each waiter enters, it still fetches the remote before discovering that a
    previous task already pushed the latest state. Process only the newest revision,
    batch compatible files into one commit, expose failure/backlog state, and recover
    committed-but-unpushed edits after restart.
  - Enforce the documented 60–86,400 second interval on the backend. Create and update
    currently accept zero through direct API calls; a future scheduler must not turn
    that persisted value into a tight Git-fetch loop. Paginate/retain scan history as
    well, since each unchanged scheduled scan inserts another row and the history route
    currently returns all rows.
  - Stop the browser poll until a real active-scan signal exists. Return a paginated
    binding list from one bounded aggregate/join query, then push activity or poll only
    while a claimed scan is actually active.
  - Add multi-replica fake-clock tests for due claims, unchanged commits, pause/resume,
    zero/out-of-range API intervals, failure backoff, crash/lease recovery, manual-scan
    races, shutdown, bounded history, rapid repeated saves, and push restart recovery.
    Add browser request-count and database query-count tests proving idle traffic is
    zero and work stays bounded as binding/edit count grows.
  - Relevant code: `web/src/pages/admin/repo-bindings.tsx` and
    `src/controllers/admin/repo_bindings.rs`,
    `src/controllers/admin/repo_bindings/mutations.rs`, and the runtime startup/task
    supervision path, plus `src/controllers/edit/challenges/repo_push.rs`.

- [x] Fix the Challenge Reviews refresh button's invalid React hook call.
  - Its click handler calls `useEditGetReviewAnalytics` directly, which deterministically
    throws an “Invalid hook call” instead of refreshing analytics.
  - Destructure the analytics hook's `mutate` function at component render time and
    invoke that callback alongside the review-list mutation.
  - Add an interaction test that clicks Refresh and verifies both requests and the
    updated cards without a runtime error.
  - Relevant code: `web/src/pages/admin/games/[id]/ChallengeReviews.tsx`.

- [x] Keep team/event join dialogs and user input intact when enrollment fails.
  - `GameJoinModal` resets its invite/division fields and closes in `finally`; its parent
    catches and reports the API/fingerprint error without rethrowing, so invalid codes
    and transient failures look terminal and force the player to reopen and re-enter
    the form.
  - Team join has the same unconditional reset/close after fingerprint or server
    failure, including a valid invite code that should be safe to retry.
  - SPA navigation can reuse the open event modal for another game. Its nonempty
    `divisionId` prevents the new game's default from being selected, while team,
    invite-code, disabled, and open state can also carry across the route boundary.
    With global previous-data reuse, a late game-A join-check can briefly validate the
    game-B form; submission then targets game B with stale game-A choices and fails at
    the backend boundary.
  - Return an explicit success result (or propagate failure), close/reset only after
    confirmed success, retain non-secret selections on recoverable errors, and keep one
    submission in flight.
  - On a real game-ID or account transition, close/reset the form immediately, clear the
    invite secret, cancel the old join-check generation, and validate the selected team
    and division against the current response before enabling submit. This route reset
    is distinct from retaining input after a same-event recoverable failure.
  - Add invalid-invite, fingerprint-probe, transient server, duplicate-click, and success
    tests for both dialogs, verifying focus/error placement and retained input. Add
    A→B navigation with the modal open and closed, slow A responses, disjoint division
    IDs, and an account/team-list change.
  - Relevant code: `web/src/components/GameJoinModal.tsx`,
    `web/src/pages/games/[id]/Index.tsx`, and `web/src/pages/Teams.tsx`.

- [x] Preserve challenge-review drafts and report success only after the review commits.
  - `GameChallengeModal.onReviewSubmit` catches a failed API request and resolves its
    promise normally. `ChallengeModal` therefore marks the review submitted anyway and,
    for a just-solved challenge, clears the flag and closes the modal, discarding the
    player's rating/comment even though no review was stored.
  - Make the callback return an explicit committed result or let it reject. Keep error
    presentation in one layer, and wrap the modal's await in `try/finally` so loading is
    always released while `reviewSubmitted`, draft clearing, and close happen only on
    confirmed success. Retain and focus the editable draft after a recoverable failure.
  - Add rejected 400/403/429, transient 5xx, lost connection, success, duplicate action,
    and challenge-switch tests. A failed save must keep the modal and exact draft open;
    one successful save may mark/close it exactly once.
  - Relevant code: `web/src/components/GameChallengeModal.tsx`,
    `web/src/components/ChallengeModal.tsx`, and
    `src/controllers/game/submit_review.rs`.

- [x] Bind fused anti-cheat evidence and review drafts to the exact participation and
  finding being reviewed.
  - `FusedEvidencePanel` retains the previous result when `participationId` changes and
    has no abort/generation check, so it can render the old team's evidence under a new
    selection and a late old response can overwrite the new one.
  - Every row also shares one `status` and `note` state. Closing finding A or switching
    directly to finding B preserves A's draft and even preserves the last disposition
    after a successful save, making it easy to record a note or confirmation against the
    wrong immutable finding.
  - Clear stale evidence at the identity boundary, abort or ignore superseded loads, and
    key each draft by `(gameId, participationId, findingId)` or reset/prefill it whenever
    that identity changes. Bind save completion and notifications to the same immutable
    identity and keep exactly one submission in flight.
  - Make the note limit consistent for Unicode: HTML `maxLength` counts browser string
    units while the backend rejects `str::len()` bytes, so valid-looking multilingual
    notes can fail below the displayed 4,000-character limit.
  - Add late A→B response, close/reopen, switch-with-dirty-draft, confirmed→new-finding,
    duplicate-save, Unicode boundary, and failed-refresh-after-save tests. Assert that no
    evidence or draft is ever displayed or persisted under another finding.
  - Relevant code: `web/src/components/monitor/FusedEvidencePanel.tsx`,
    `web/src/components/monitor/CheatInfo.tsx`,
    `src/controllers/admin/anti_cheat.rs`, and
    `src/services/event_security/fusion.rs`.

- [x] Bind KotH receipt/referee dialogs to the hill whose response populated them.
  - `openReceipts` and `openObserver` replace the selected hill and start a new request
    without aborting or generation-checking the prior one. Clicking hill A then B can
    let A's late response render under B's title; either request's `finally` can also
    clear the shared loading flag while the newer request is still running.
  - The referee dialog is actionable. Stale A configuration shown as hill B can lead an
    operator to rotate or revoke B because those mutations target `observerHill`, not
    the hill identity carried by the displayed response.
  - Give each dialog an immutable `(gameId, challengeId, generation)` request owner,
    abort/ignore superseded loads, and store the returned identity with its data. Enable
    rotate/revoke only when that identity exactly matches the selected hill and one
    current load has completed; clear state on close, game change, and hill removal.
  - Add delayed A→B/B→A, close/reopen, game navigation, hill removal, request failure,
    and rotate/revoke interaction tests. No receipt/configuration or mutation control
    may ever appear under or act on another hill.
  - Relevant code: `web/src/components/admin/KothOpsPanel.tsx` and
    `src/controllers/game/koth/admin.rs`, `src/controllers/game/koth/api/admin.rs`.

- [x] Propagate container-extension failures to `InstanceEntry`.
  - Do not display a success notification or disable retry when the extension request
    failed.
  - Base destroy on the value returned by `mutate()`. `requestDestroy` currently awaits
    revalidation and then checks/mutates the pre-revalidation `challenge` closure, so it
    can skip a newly discovered runtime or issue teardown using expired state.
  - Keep teardown idempotent and revalidate after the authoritative response.
  - Add regression tests for a rejected extension request, stale-to-present runtime,
    already-removed runtime, and rapid create/destroy actions.
  - Relevant code: `web/src/components/GameChallengeModal.tsx` and
    `web/src/components/InstanceEntry.tsx`.

- [x] Stop expired WSRX readiness checks from polling the local daemon forever.
  - When a newly added tunnel still has `latency === -1`, every matching
    `InstanceEntry` starts its own `wsrx.sync()` interval every 1.5 seconds. The
    eight-second timeout only sets `tunnelCheckExpired`; it neither clears that
    interval nor changes an effect dependency, so an unknown latency leaves the
    supposedly temporary loop running until the component unmounts. Multiple entries
    call the same singleton concurrently, and a slow `/pool` response can overlap the
    next interval callback.
  - Move readiness refresh into one provider-owned, single-flight scheduler shared by
    all pending tunnels. Schedule from completion, stop at a real absolute deadline,
    and fall back to the library's existing 15-second synchronization after the
    accelerated window. Unmount, daemon failure, option changes, and expiry must cancel
    queued work; an explicit retry can start one new bounded window.
  - Add fake-timer tests with permanently unknown latency, a response slower than 1.5
    seconds, daemon failure/recovery, unmount before timeout, and many simultaneous
    entries. One and 100 pending entries must produce the same bounded `/pool` request
    count, with no request after the deadline.
  - Relevant code: `web/src/components/InstanceEntry.tsx` and
    `web/src/components/WsrxProvider.tsx`.

- [x] Repair account-profile error handling.
  - Resolve the contradiction between `shouldRetryOnError: false` and `onErrorRetry`.
    In the installed SWR behavior, the false predicate prevents `onErrorRetry` from
    being called at all, so its 403 logout/ban notification and intended five-attempt
    transient recovery are unreachable.
  - Put terminal 401/403 session effects in an error path that always executes, and use
    a status-aware retry predicate/scheduler only for transient failures. Cap and
    jitter retries, honor `Retry-After`, cancel pending work on unmount/session change,
    and avoid turning an optional anonymous profile probe into a login redirect.
  - Add tests for transient 5xx recovery/exhaustion, persistent 429, expired sessions,
    banned users, unmount, and account replacement while a retry is pending.
  - Relevant code: `web/src/hooks/useUser.tsx`.

- [ ] Stop passive account-stat and team-roster refreshes from rebuilding unbounded
  user histories.
  - The stats panel's raw SWR hook inherits the global one-minute interval and default
    error retry. Each tick loads every accepted submission model for the account
    (including the answer) through a filter with no `(user_id,status)` index, expands
    large challenge/game/submission ID lists, and returns an unpaged lifetime game
    history. A 401/403/429 is only a generic `Error`, so it is not classified as a
    terminal or back-pressure response.
  - `useTeams` explicitly polls every two minutes on game and team screens. The server
    first loads all joined teams and then performs two serial roster queries per team,
    returning as many as 100 full member profiles for each. Captaincy is capped but
    team membership is not, while game join UI only needs a compact team ID/name
    selector.
  - The event landing page mounts that poll in both `GameDetail` and its closed
    `GameJoinModal`; the team page likewise mounts a second team-info hook inside its
    closed editor. Those duplicate timers rely on SWR dedupe and can drift into two
    expensive roster reads per cadence. Pass one parent-owned snapshot into dialogs
    and do not mount dialog-only reads until the dialog is open.
  - Make stats mutation-driven (accepted solve/account change/manual refresh) and
    compute its compact aggregates in indexed raw SQL without selecting flag answers.
    Add the supporting partial/composite index and page or cap lifetime history.
  - Split the compact team selector from on-demand roster detail, fetch required teams
    and members in bounded joined queries, cap/page memberships, share one cache owner,
    and invalidate it after team mutations instead of polling idle pages.
  - Add large-ledger query-plan and response-bound tests, a user with many 100-member
    teams, expired-session/429 tests, many synchronized game tabs, and browser
    request-count assertions. No unchanged stats/team page should emit periodic work.
  - Relevant code: `web/src/components/account/StatsPanel.tsx`,
    `web/src/hooks/useUser.tsx`, `web/src/components/GameJoinModal.tsx`,
    `web/src/components/TeamEditModal.tsx`, `web/src/pages/games/[id]/Index.tsx`,
    `web/src/pages/Teams.tsx`,
    `src/controllers/account/mod.rs`, `src/controllers/team/mod.rs`, and
    `src/migrations/m0021_hot_indexes.rs`.

- [ ] Make SWR refresh and retry behavior opt-in instead of hidden defaults.
  - The application-level `refreshInterval: 60000` silently turns every new SWR read
    into a poller unless its caller remembers `OnceSWRConfig`.
  - Gate modal-owned reads on `opened`; for example, the always-mounted A&D toolkit
    polls the SSH-key database endpoint while the toolkit is closed, and the always
    mounted team editor inherits a second team-info refresh schedule.
  - Give A&D data one route owner. The challenges page already polls `adState`, but an
    open A&D challenge mounts another ten-second owner and another SSH-key hook; the
    closed toolkit keeps its own SSH-key minute poll. Pass the route snapshot/mutation
    and compact SSH metadata into both panels instead of relying on SWR's short dedupe
    window to collapse independently scheduled timers.
  - The post-event `snapshotOnly` A&D branch is checked only after both hooks run. It
    returns before using SSH metadata, yet still starts that key's global minute refresh;
    it also creates another state owner just to derive the snapshot link. Gate the unused
    SSH key, and pass the already-owned route/service snapshot into the download-only
    component instead of mounting live hooks behind an early return.
  - The pending-challenge review page also passes no SWR policy, so an idle tab reloads
    every Pending and Rejected challenge once per minute even though approve, reject,
    and delete already call `mutate`. Make that list mutation-driven and paginate the
    backend projection instead of selecting every full challenge model.
  - `OnceSWRConfig` is not actually once: it disables interval/focus refresh but leaves
    SWR's automatic error retry enabled. Missing games, revoked permissions, pre-start
    notices, deleted resources, and other permanent 400/401/403/404 responses can
    therefore keep retrying indefinitely from hooks explicitly presented as one-shot.
    Centralize typed HTTP status extraction, stop terminal responses, honor
    `Retry-After`, and cap/jitter transient retries; expose an explicit manual retry
    where a one-shot screen must recover.
  - Give each genuinely live read an explicit cadence, visibility/offline policy,
    retry ceiling, and ownership point. Default all other reads to no interval.
  - Add source/request-count tests for closed modals, persistent 403/404/429 and
    transient 5xx responses, one versus many A&D challenge opens, and an inventory test
    that fails when a new hot-path poller or one-shot read omits an explicit retry
    policy.
  - Relevant code: `web/src/App.tsx`, `web/src/hooks/useConfig.ts`,
    `web/src/hooks/useGame.ts`, `web/src/pages/games/[id]/Challenges.tsx`,
    `web/src/components/AdGuideModal.tsx`,
    `web/src/components/AdChallengePanel.tsx`, and
    `web/src/components/TeamEditModal.tsx`.

- [ ] Stop the joined-challenge catalog from periodically rescanning a player's entire
  event history.
  - `/challenges` sends an identical request every minute while open. Its response is
    capped to 24 rows, but PostgreSQL first builds candidates from every accepted event
    the player has joined, every visible challenge in those started events, and a
    correlated accepted-submission existence check per candidate; `COUNT(*) OVER ()`
    still visits the complete filtered result before pagination.
  - The exact one-minute cadence synchronizes ordinary tabs, and work grows with retained
    event/challenge history even when nothing changed. Query admission limits concurrent
    damage but does not make this normal distributed client traffic cheap.
  - Refresh after an accepted solve, participation/configuration mutation, explicit user
    action, or a versioned server notification instead of on idle time. If a fallback is
    required, make it visible/online-only, completion-scheduled, jittered, conditional,
    and bounded by event state.
  - Join the canonical `FirstSolves` projection for solved state instead of probing the
    submissions ledger repeatedly, push filters before count/sort where semantics allow,
    and verify the composite-key/index plan. Cache only with an authorization- and
    solve-version key so participation removal is never hidden by stale data.
  - Add real-PostgreSQL plans for a player with many historical events and challenges,
    mutation invalidation tests, and a many-tab fixed-rate test. Bound scanned rows,
    query time, pool waiters, and requests when the catalog is unchanged.
  - Relevant code: `web/src/pages/challenges/Index.tsx`,
    `src/controllers/game/catalog.rs`, `src/controllers/game/routes.rs`, and the next
    idempotent forward index migration if plan evidence requires one.

- [x] Cancel superseded game-manager autocomplete reads and bound their database search.
  - The manager selector starts a new `/api/admin/users` request after every 300-ms typing
    pause but never aborts or generation-checks the previous request. Clearing the field
    does not cancel it, so a slow old response can repopulate an empty/new query and its
    `finally` can hide the loading state while the latest request is still running.
  - Asking for ten rows does not bound the backend work: it first runs an exact total
    count over case-insensitive contains matches across username/email, then runs the
    limited row query. The endpoint accepts empty/long substring searches and has no
    named query-work admission, so normal rapid typing or a client regression can stack
    full user-table scans even though the dropdown needs only a few identities and no
    total.
  - Use a compact manager-autocomplete endpoint with normalized minimum/maximum query
    length, no full count, a strict result cap, an index-supported prefix or trigram plan,
    and `Policy::Query`. Abort superseded browser work and generation-bind results and
    loading state; clearing, route changes, and unmount must invalidate every old result.
  - Add fake slow/out-of-order `a`→`ab`→clear browser tests and real-PostgreSQL plans for
    a large user table, long/wildcard-like terms, concurrent admins, 429/`Retry-After`,
    and route changes. Only the newest query may update the selector, and scanned rows,
    in-flight queries, response bytes, and latency must remain bounded.
  - Relevant code: `web/src/pages/admin/games/[id]/Managers.tsx`,
    `src/controllers/admin/users.rs`, `src/controllers/admin/mod.rs`, and an idempotent
    forward index migration if query-plan evidence requires one.

- [x] Keep previous SWR data from crossing game, query, or account boundaries.
  - Global `keepPreviousData: true` lets a reused route component render game A's
    challenge/team/scoreboard response under game B's URL until the new request
    settles—and can leave the stale view in place when the new request is rejected.
  - The challenge panel also preserves `challenge`, `detailOpened`, and the writeup
    modal across `gameId`; an empty or nonmatching hash never closes the old selection.
    After B loads, an open challenge from A is remounted as `(game B, challenge A)` and
    starts B/A detail and solver schedules every 120/30 seconds. Besides showing the
    wrong title and score, a permanent B/A failure can inherit automatic retries and
    keep issuing invalid background traffic.
  - Both desktop and mobile scoreboards preserve the selected team/detail modal across
    the same transition; the mobile table also keeps its old search. That combines a
    stale A team with B's board/challenge projection even when the IDs do not belong to
    the same event.
  - Clear route-local selections, modal-open flags, searches, and live buffers
    immediately on `gameId`; disable previous-data reuse for authorization- or
    identity-scoped keys, and render an explicit loading or error state during key
    transitions. Reopen a hash-selected challenge only after verifying it belongs to
    the current loaded response, and gate detail/solver keys on both that ownership and
    the modal being open.
  - Add SPA navigation tests for A→B with challenge, writeup, and team modals open,
    colliding and noncolliding IDs, an empty/invalid hash, B forbidden/missing,
    desktop/mobile search state, accepted→non-member, admin→player account switch,
    slow responses, and rejected requests. Assert zero invalid B/A detail/solver reads.
  - Relevant code: `web/src/App.tsx`, `web/src/hooks/useGame.ts`,
    `web/src/components/ChallengePanel.tsx`,
    `web/src/components/GameChallengeModal.tsx`,
    `web/src/components/ScoreboardTable.tsx`, and
    `web/src/components/mobile/ScoreboardTable.tsx`.

- [x] Make the shared ticker cancel-safe.
  - Track and cancel the whole-second alignment timeout.
  - Prevent visibility and mount/unmount races from creating orphan or duplicate
    intervals.
  - Add fake-timer tests covering unmount-before-alignment and rapid hide/show cycles.
  - Relevant code: `web/src/hooks/useTicker.ts`.

- [x] Reconcile resources after accepted-participation provisioning fails.
  - Keep join persistence atomic, but record or enqueue failed provisioning so generic
    challenge instances are retried automatically.
  - Verify that an accepted team eventually receives every required attachment,
    instance, and service after a transient database or container-runtime outage.
  - Add real-PostgreSQL recovery tests and container-runtime tests for the applicable
    paths.
  - Relevant code: `src/controllers/game/play.rs` and
    `src/controllers/edit/ad/provision.rs`.

- [x] Publish the monitor event feed that the UI subscribes to.
  - Emit `ReceivedGameEvent` when `GameEvents` rows are committed, with a stable event
    identity for deduplication and an HTTP backfill cursor.
  - Add the target to the Redis event-bus allowlist so cross-replica delivery is not
    silently discarded.
  - Add local and multi-replica tests for container/open/download/solve/cheat events,
    reconnect gaps, and game isolation.
  - Relevant code: `web/src/pages/games/[id]/monitor/Events.tsx`,
    `src/hubs/monitor.rs`, `src/services/event_bus.rs`, and the `GameEvents` writers
    under `src/controllers/game/`.

- [x] Make the live arena recover and reconcile its roster after startup.
  - Retry an initial A&D-board failure with bounded backoff; the current `NO LIVE DATA`
    path starts only the clock and animation loop and can never recover.
  - Rebuild or reconcile teams and hills when accepted teams are added, participants
    are suspended/reinstated, or KotH data becomes available after the initial fetch.
  - Build a union of A&D and KotH rosters, remove or hide teams no longer on the
    official board, and do not rank stale `onOfficialBoard=false` rows.
  - Register every deferred animation/preview callback with the arena teardown owner.
    Several nested first-blood and simulated-SLA `setTimeout` callbacks are not in the
    cleared timer list, and some do not check `killed`; navigating away mid-cinematic can
    mutate destroyed renderer/DOM state after teardown.
  - Add lifecycle tests for opening early, transient startup failure, late admission,
    suspension/reinstatement, a failed-then-recovered KotH fetch, and unmounting at each
    cinematic/deferred-recovery phase with fake timers.
  - Relevant code: `web/src/pages/games/[id]/Attack.tsx` and
    `src/controllers/edit/helpers.rs`.

- [ ] Make Event-VPN failures visible and consistent across every HTTP client.
  - Route native `fetch` calls through the same proof-aware wrapper as Axios. Solver
    and rating requests currently omit the proof and silently render empty data for
    ordinary players in VPN-required events.
  - Distinguish Event-VPN 401 responses from expired authentication; a proof-mint or
    VPN disconnect must not redirect a still-authenticated player to login.
  - Add browser tests for connected, disconnected, expired-proof, monitor-bypass, and
    session-expiry cases.
  - Coalesce failed proof minting behind a bounded backoff/circuit breaker. Otherwise
    every protected poll that receives 401 can start another challenge/proof exchange
    after the previous mint flight fails, multiplying an outage into extra server and
    VPN-gateway traffic.
  - Back that client guard with a named server-side mint budget. The challenge and
    proof routes currently rely only on the global HTTP allowance; every cycle reloads
    policy/participation state and proof minting also resolves the live VPN peer. Many
    tabs, accounts, or a broken interceptor can therefore repeat the database work even
    though a challenge remains valid for 60 seconds and a proof for 30. Coalesce an
    exact live subject/game/peer generation, reject excess work before its first query
    with `429` plus `Retry-After`, and bound mint concurrency across replicas without
    caching through stamp, roster, peer-generation, or policy revocation.
  - Relevant code: `web/src/utils/EventVpnProof.ts`,
    `web/src/components/GameChallengeModal.tsx`,
    `web/src/components/ChallengePanel.tsx`, `web/src/App.tsx`,
    `web/src/utils/AuthState.ts`, `src/controllers/game/vpn_access.rs`, and
    `src/middlewares/rate_limiter.rs`.

- [x] Bound the news feed before normal homepage refreshes become full-table work.
  - `/api/posts/latest` sorts and loads every post model and only then truncates the
    vector to 20. Every open homepage requests that endpoint every five minutes, so
    old post growth increases PostgreSQL rows, Rust allocations, and sort/transfer
    work even though none of those rows can reach the response.
  - `/api/posts` likewise returns the complete history while the news page renders
    only ten rows with client-side slicing. A single request therefore transfers and
    retains every summary merely to show one page.
  - Apply `ORDER BY is_pinned DESC, update_time_utc DESC LIMIT 20` in PostgreSQL for
    the homepage and add the supporting ordered index. Make the full feed a
    server-paginated, capped projection with an explicit total, fetching author data
    only for the selected page.
  - Prefer mutation-driven invalidation or a stable version/ETag for the infrequently
    changing homepage feed. If a fallback poll remains, pause it while hidden/offline,
    jitter it, and return no body when the version is unchanged.
  - Add large-post-table query-plan, response-size, pagination, pin-order, conditional
    request, and many-homepage fixed-rate tests. Work for the latest feed must remain
    bounded at 20 rows regardless of retained history.
  - Relevant code: `src/controllers/info.rs`, `web/src/pages/Index.tsx`, and
    `web/src/pages/posts/Index.tsx`, plus a new forward index migration.

- [x] Bound recent-game selection in PostgreSQL instead of loading and sorting every
  public game in Rust.
  - `GET /api/game/recent` currently fetches every non-hidden game, sorts the full set
    in memory, and truncates afterward; every client revisits the endpoint periodically.
  - Express the established proximity ordering as a bounded SQL `ORDER BY ... LIMIT
    50`, add the supporting index/plan evidence, and keep the caller's requested limit
    clamped.
  - Add a large-game-table query-plan and fixed-rate test with bounded rows, memory, and
    latency.
  - Relevant code: `src/controllers/game/play.rs` and
    `web/src/hooks/useGame.ts`.

- [x] Bound public scoreboard bandwidth and client parse work for large events.
  - A&D and combined boards use the negotiated precompressed cache bundle, but the
    standard and KotH endpoints repeatedly return raw JSON. The KotH body grows with
    teams × hills (plus epoch detail) and is polled every ten seconds, so legitimate
    spectators can multiply egress and browser JSON/React work even when the version is
    unchanged.
  - Reuse the existing off-thread raw/gzip/Brotli bundle path, expose a stable
    version/ETag, support conditional reads, and avoid replacing/rendering an unchanged
    board on the client. Keep cache payload and encoded-size limits explicit.
  - Add a maximum-roster/hill fixed-rate test that measures encoded bytes, CPU, memory,
    JSON parse/render time, and 304/cache-hit ratios for many synchronized spectators.
  - Relevant code: `src/controllers/game/scoreboard_board.rs`,
    `src/controllers/game/koth/mod.rs`,
    `src/controllers/game/scoreboard_encoding.rs`, `web/src/hooks/useGame.ts`, and
    `web/src/components/KothScoreboardTable.tsx`.

- [x] Paginate and minimize the participation-review payload on the server.
  - The admin page downloads every participation, every registration link, every team
    roster, and every referenced user—including full profile fields—then filters and
    slices one visible page in the browser. Large registration rounds therefore create
    an avoidable response, allocation, and render spike.
  - Move status/division/search/pagination into one bounded joined SQL query, return a
    compact list DTO, and lazy-load full roster/profile detail only when an operator
    opens one participation.
  - Add large-event response-size, query-plan, PII-minimization, filter, and browser
    responsiveness tests.
  - Relevant code: `web/src/pages/admin/games/[id]/Review.tsx` and
    `src/controllers/game/scoreboard.rs`.

- [x] Bound and offload monitor spreadsheet generation and make downloads single-flight.
  - The submission export loads every submission into both model and projected vectors;
    both it and the scoreboard export run `rust_xlsxwriter::save_to_buffer`
    synchronously on a Tokio request worker. Neither GET route has weighted query/work
    admission, so concurrent exports multiply database, memory, and event-loop pressure.
  - `downloadBlob` accepts an already-started promise and only then schedules a React
    disabled-state update. Rapid duplicate actions or two mounted monitor controls can
    therefore launch overlapping exports before the rendered button becomes disabled;
    server correctness cannot depend on that UI state anyway.
  - Stream/page an explicitly bounded snapshot, run spreadsheet generation through
    `spawn_blocking`, and acquire weighted admission plus a small export semaphore before
    loading rows. Return a typed 429/503 with `Retry-After` when full.
  - Change the download helper to accept a request factory guarded by an immediate ref
    or shared single-flight key, and keep one in-flight owner per export kind/game until
    the body has settled. Treat this only as a usability guard around backend admission.
  - Add large real-PostgreSQL exports, rapid double-click/two-control tests, and a
    concurrent-export fixed-rate test that keeps `healthz` responsive and verifies
    bounded memory, tasks, timeout, and row integrity.
  - Relevant code: `web/src/utils/ApiHelper.tsx`,
    `web/src/components/WithGameMonitor.tsx`,
    `web/src/pages/games/[id]/monitor/Submissions.tsx`,
    `src/controllers/game/scoreboard.rs`, `src/controllers/game/routes.rs`, and
    `src/controllers/game/scoreboard_encoding.rs`.

### P2 — Final scoreboard consistency

- [x] Correct the combined-scoreboard keys used by the end-of-event cache sweep.
  - Evict `_CombinedScoreBoardByChallenge_{game_id}` and
    `_CombinedScoreBoardByChallengeFrozen_{game_id}`, matching the keys produced by the
    combined scoreboard controller.
  - Add a regression test that seeds the real keys, runs the sweep, and verifies both
    entries are removed.
  - Relevant code: `src/controllers/game/combined_scoreboard.rs` and
    `src/services/cron/mod.rs`.

- [x] Keep deadline-dependent challenge and writeup controls reactive.
  - Recompute writeup eligibility as the deadline passes and do not re-enable upload
    after either the preliminary 400 or transactional 409 rejection from the
    authoritative backend. Freshly mounted and resumed controls synchronously sample
    current time rather than briefly inheriting a stale ticker snapshot.
  - Drive deadline-bearing `ChallengeCard` instances from the shared ticker without
    subscribing every static card in a large challenge grid.
  - Add fake-timer tests for both transitions, stale mount/resume, static-card render
    amplification, and both authoritative rejection paths.
  - Relevant code: `web/src/components/WriteupSubmitModal.tsx` and
    `web/src/components/ChallengeCard.tsx`, plus `web/src/hooks/useTicker.ts`.

- [x] Make the interactive team checkpoint require a meaningful form action.
  - Highlight the complete Create/Join workflow rather than describing a text input as
    a clickable action. Tell the player to type and submit, keep the real form usable
    inside the spotlight, and support keyboard submission.
  - Advance only after team creation/join succeeds or the live team list proves the
    player already belongs to a team; focusing or typing alone must not claim progress.
  - Add a mounted real-component regression that types into the Mantine input, verifies
    the submit state, crosses the API boundary, and observes completion only after a
    successful response.
  - Relevant code: `web/src/components/guide/PlayerGuide.tsx`,
    `web/src/utils/GuideState.ts`, `web/src/pages/Teams.tsx`, and
    `web/src/components/TeamCreateModal.tsx`.

- [x] Avoid `NaN` event progress for valid sub-minute events.
  - Compute progress from milliseconds or guard a zero whole-minute duration, then
    clamp the visible result to a finite range.
  - Add boundary tests for a 30-second event, exact start/end instants, and malformed
    timestamps.
  - Relevant code: `web/src/hooks/useGame.ts` and
    `src/services/game_config.rs`.

- [x] Count visible challenges rather than categories in team solve progress.
  - After applying division visibility, `/api/game/{id}/details` sets
    `challengeCount` to `challenges.len()`, which is the number of category-map keys.
    `TeamRank` divides the team's solved-challenge count by that value. Two or more
    challenges in one category can therefore render progress above 100%, while an
    empty visible set computes `0 / 0` and passes `NaN` to Mantine's progress bar.
  - Return the flattened visible challenge count and derive the numerator from those
    same visible IDs (rather than an unfiltered scoreboard row). Defensively handle a
    zero denominator and clamp only a finite ratio.
  - Add wire/UI tests for no visible challenges, several challenges in one category,
    multiple categories, division-hidden challenges, and a permission change while
    the page remains open.
  - Relevant code: `src/controllers/game/play.rs`,
    `src/controllers/game/scoreboard_board.rs`, and
    `web/src/components/TeamRank.tsx`.

### Completion gate

- [ ] Run `cargo build` with zero warnings and `cargo test` with the required
  PostgreSQL/container environment available.
- [ ] Run the strict frontend typecheck, lint, tests, production build, and relevant
  browser/Axe checks.
- [ ] Run the fixed-rate event load workflow for polling, realtime-feed, submission, or
  resource-usage changes and compare latency, errors, throughput, and resource growth.
- [ ] Release and deploy one immutable digest to every applicable `tcp.1pc.tf` replica,
  then verify exact health, version/digest, changed behavior, recent logs, and installer
  endpoints where applicable.
