# Load / stress tests

Behaviour-mimicking load against a running rsctf stack — to find architecture/
implementation flaws and capacity limits under real concurrency (not just peak req/s).

**Everything is JavaScript.** k6 scripts (`k6/`) generate the HTTP load; Node
orchestrators (`*.mjs`) discover state, spin up/tear down BYOC tunnels, and run the k6
scenarios. Run via npm:

```sh
cd tests/load
N=60  npm run byoc          # BYOC scale + request flood
      npm run player        # A&D + KotH player poll/submit load
      npm run ad-submit-batch # explicit fixed-rate, max-batch A&D submit micro-harness
      npm run redis-outage  # disposable Redis failure/recovery micro-harness
N=120 npm run worst-case    # mass BYOC reconnect storm (restarts rsctf)
FLEET=10 npm run worker      # trusted worker create/proxy/destroy + lease gate
FLEET=5  npm run worker-local # isolated current-tree rsctf + native Linux agent
```

Requires `k6`, `node`, and `docker exec <PG>` / `docker` access; the stack up with a
running game (default `GAME=10`, BYOC challenge `CID=68`). BYOC runs require at least
`N` distinct Accepted participations; the harness fails before spawning when fewer are
available rather than fabricating participation IDs that production authorization rejects.

## Layout

```
tests/load/
  lib.mjs           shared: config, docker/psql shells, JWT + BYOC-token minting, discovery, k6 runner
  byoc-agents.mjs   BYOC tunnel fleet: seed rows, start/stop N relay agents, list listeners
  fixtures.mjs      materializes the exact checker + shared flag service used by lifecycle
  player-model.js   deterministic competitive player profiles and A&D/Jeopardy/KotH decisions
  team-clients.mjs  one WireGuard+k6 container per team, plus verified teardown
  observe.mjs       read-only health/resource/evidence sampler for long event runs
  cheat-event.mjs   retained anti-cheat drill: deterministic offenders + clean controls
  player.mjs        → runs k6/player.js         (npm run player)
  ad-submit-batch.mjs → runs k6/ad-submit-batch.js (npm run ad-submit-batch)
  redis-outage.mjs  → stops/restores one acknowledged disposable Redis + runs k6/redis-outage.js
  byoc.mjs          → runs k6/byoc-requests.js  (npm run byoc)
  worst-case.mjs    → reconnect-storm harness   (npm run worst-case)
  worker-plane.mjs  → trusted worker lifecycle  (npm run worker)
  worker-plane-local.mjs → isolated native-agent acceptance wrapper (npm run worker-local)
  k6/
    player.js         A&D + KotH player: poll boards/timelines, tokens/state, submit flags
    ad-submit-batch.js fixed-rate 100-entry repeated/distinct A&D submit batches
    redis-outage.js   fixed-rate malformed requests while Redis is unavailable
    byoc-requests.js  flood BYOC tunnel listeners
    worker-plane.js   fixed-rate trusted-worker TCP proxy streams + health polls
    team-event.js     one isolated, VPN-connected player process per team
    cheat-event.js    stolen flags, bot-like wrong submissions, honeypot probes, controls
  test/
    *.test.mjs        Node unit and regression tests for the load harness
    fixtures/         subprocess fixtures used only by those tests
```

The player scenario records separate trends for the main, A&D epoch, and KotH
boards, plus A&D State, A&D Targets, KotH timeline, token, and State. The
lifecycle scenario also keeps every official `/Ad/Scoreboard` poll in the
separate `ad_epoch_board_ms` trend. This prevents one combined distribution from
hiding which endpoint changed. The standalone player scenario uses a constant
arrival rate (`RATE`, default `VUS/2` iterations/s). Board trends accept valid
HTTP 200 responses; the A&D epoch trend additionally requires a semantically
valid started board. The run fails if the frozen roster has fewer than two teams.

Every knob is env-overridable: `TARGET`, `GAME`, `CID`, `VUS`, `RATE`, `DURATION`, `N`,
`RSCTF_JWT_SECRET`, `PG_CONTAINER`, `RSCTF_CONTAINER`, `NET`, `AD_NET`,
`LOAD_FIXTURE_ROOT`. The standalone player scenario also accepts
`THINK_MIN_SECONDS` / `THINK_MAX_SECONDS` (defaults 3–5 seconds) and sends each
real player session on its public-board polls. This keeps a normal reverse proxy's
anti-spoofing of forwarding headers from turning a logged-in player load into one
anonymous source-IP bucket. Lifecycle provisioning also accepts a paired immutable
`KOTH_CONTAINER_IMAGE` and `KOTH_CONTAINER_PORT`, allowing the default functional
hill fixture to be replaced independently of the Jeopardy `CONTAINER_IMAGE`. Long distributed runs also use `FLEET`,
`DISTRIBUTED_TEAM_CLIENTS`, `LIFECYCLE_ISOLATED_SERVICES`, `TEAM_THINK_SECONDS`,
`TEAM_START_DELAY_SECONDS`, `EVENT_END_GRACE_SECONDS`, `TEAM_EVIDENCE_DIR`, and
`TEAM_CLIENT_CPUS`/`TEAM_CLIENT_MEMORY`. Competitive runs add
`REALISTIC_COMPETITION`, `SIMULATION_SEED`, `INTEGRATED_CHEAT_SIMULATION`,
`CHEAT_AT_FRACTION`, `RETAIN_EVENT`, and `LIFECYCLE_STATE_TAG`. Set
`RSCTF_BYOC_AGENT_IMAGE` to test a specific local RSCTF agent tag or immutable digest;
the network-fetched default is pinned to the attested GHCR digest used by the server.

### Redis outage (`npm run redis-outage`)

This fixed-rate micro-harness stops one explicitly acknowledged Compose Redis service,
waits for `/livez` to remain 200 while `/healthz` reports the dependency outage, sends
malformed registration requests that must remain HTTP 400 responses with p95 latency
below 1,000 ms, and restores Redis in a `finally` block. It then waits for `/healthz`
to recover without restarting rsctf. Use only a disposable stack; the exact container
name must be repeated as the acknowledgement:

```sh
cd tests/load
TARGET=http://127.0.0.1:58080 \
REDIS_CONTAINER=rsctf-test-redis-1 \
CONFIRM_REDIS_OUTAGE=rsctf-test-redis-1 \
RATE=1 DURATION=15s SUMMARY_JSON=/tmp/rsctf-redis-outage.json \
npm run redis-outage
```

Non-loopback targets require a second exact acknowledgement through
`CONFIRM_REMOTE_REDIS_OUTAGE=<target origin>`. The scenario fails on a dropped
iteration, a response other than 400, p95 latency at or above 1,000 ms, or an
unsuccessful dependency recovery.

### A&D submit batch (`npm run ad-submit-batch`)

This narrow micro-harness measures the A&D flag lookup, eligibility, deduplication, and
result-encoding paths with `ad_submit_ms` at a held arrival rate. Every request contains
the endpoint's maximum 100-entry batch. `BATCH_SHAPE=repeated` repeats one supplied
current flag; a correctly scoped attacker accepts it at most once and every remaining
result must be `duplicate`. `BATCH_SHAPE=distinct-known` requires `FLAGS_JSON` with
exactly 100 distinct engine-shaped flags that the attacker already captured and expects
100 `duplicate` results. `BATCH_SHAPE=distinct` generates 100 deterministic,
valid-shaped but deliberately unknown flags and expects 100 `wrong` results. Per-status
counters are exported as `ad_submit_status_*`; any unexpected result, 429, or 5xx fails
the run.

It never queries PostgreSQL, discovers a flag, or mints a credential. Use only a
disposable live game, explicitly supply an Accepted attacker's bearer token and another
team's current active flag, and acknowledge that a known, uncaptured flag can change
scoring:

```sh
cd tests/load
read -rsp 'Attacker bearer token: ' TOKEN; export TOKEN; printf '\n'
read -rsp 'Current opponent flag: ' FLAG; export FLAG; printf '\n'
CONFIRM_MUTATING_LOAD=1 GAME=42 RATE=1 VUS=4 DURATION=30s \
  SUMMARY_JSON=/tmp/rsctf-ad-submit-batch.json npm run ad-submit-batch
unset TOKEN FLAG
```

For the two controlled variants, add `BATCH_SHAPE=distinct`, or read the known
fixture flags without putting them in shell history:

```sh
read -rsp 'Attacker bearer token: ' TOKEN; export TOKEN; printf '\n'
read -rsp 'JSON array of 100 known flags: ' FLAGS_JSON; export FLAGS_JSON; printf '\n'
FLAG="$(node -e 'process.stdout.write(JSON.parse(process.env.FLAGS_JSON)[0])')"; export FLAG
CONFIRM_MUTATING_LOAD=1 GAME=42 RATE=1 VUS=4 DURATION=30s \
  BATCH_SHAPE=distinct-known npm run ad-submit-batch
unset TOKEN FLAG FLAGS_JSON
```

The known array must contain exactly 100 unique values and is intended for a
pre-seeded disposable fixture; do not put live flags or bearer tokens in
repository files or command history.

The defaults hold one batch request per second for 30 seconds; `RATE` is bounded to 20
and duration to 10 minutes to prevent an accidental flood. For a before/after claim,
use the same disposable fixture, rate, VUs, duration, current flag window, and resource
sampler for both release builds. Do not compare runs that cross a round-expiry boundary.
Repeated batches cost one rate-limit token because they contain one distinct plausible
flag. Each `distinct` or `distinct-known` request costs 100. A 30-second distinct campaign
therefore requires starting that isolated rsctf process with
`RSCTF_AD_SUBMIT_BURST_FLAGS=3200`; never copy that benchmark override to production.

### Trusted worker plane (`npm run worker`)

This gate targets the outbound mTLS worker plane, not the player-facing BYOC relay.
Start rsctf with `RSCTF_CONTAINER_BACKEND=worker`, complete one-time enrollment, and
leave one or more native Linux agents online. The selected Jeopardy challenge
must be enabled, use a per-team container, and reference immutable images that the
selected workers can run. Its exposed service should answer the configured TCP probe;
the default probe is a minimal HTTP/1.1 request.

```sh
GAME=42 CID=317 FLEET=10 RATE=20 VUS=20 DURATION=30s npm run worker

# Optional, safe server-side disconnect/reconnect drill before workloads are placed.
# It refuses a worker with active workloads unless explicitly overridden.
WORKER_IDS=018f3c6a-d79b-7cc0-8f68-8fdbad0f57bb \
RECONNECT_WORKER=1 FLEET=2 npm run worker
```

The Node orchestrator uses distinct Accepted participations to create real Jeopardy
containers concurrently, verifies their exact assignment/generation rows reach
`Present/Ready`, and hands their authenticated `/api/proxy/{id}` entries to k6. k6
opens fresh WebSocket/TCP streams at a constant arrival rate while polling the admin
worker list, `/livez`, and `/healthz`. The orchestrator then destroys every container,
requires `Absent/Absent`, and checks that worker leases stayed current and session
epochs never moved backwards. Cleanup runs after failures too. No enrollment secret,
client certificate, or worker private key enters k6.

Worker-specific knobs are all environment-overridable: `FLEET` (or `N`), `CYCLES`,
`MIN_WORKERS`, `WORKER_IDS`, `WORKER_OS`, `RATE`, `VUS`, `MAX_VUS`,
`HEALTH_POLL_RATE` (or the legacy `WORKER_POLL_RATE`),
`WORKER_INVENTORY_INTERVAL_SECONDS`, `DURATION`, `STREAM_TIMEOUT_MS`, `PROBE_PAYLOAD`,
`EXPECT_PROXY_RESPONSE`, `EXPECTED_RESPONSE_MARKER`, `MAX_PROXY_FAILURE_RATE`,
`MAX_PROXY_RESPONSE_MISSING_RATE`, `MAX_PROXY_RESPONSE_INVALID_RATE`,
`MAX_PROXY_STREAM_FAILURE_RATE`,
`MAX_PROXY_RATE_PER_IDENTITY`, `DEBUG_PROXY_ERRORS`,
`OPERATION_COOLDOWN_MS`, `ABSENT_TIMEOUT_MS`,
`API_TIMEOUT_MS`, `PROXY_READINESS_DELAY_MS`, `RECONNECT_WORKER`, `RECONNECT_WORKER_ID`,
`RECONNECT_TIMEOUT_MS`, `ALLOW_ACTIVE_RECONNECT`, `ALLOW_SESSION_CHANGES`,
`EXPECTED_SERVICE_COUNT`, `EXPECTED_REPLICA_COUNT`, and `SAMPLE_RESOURCES`.
`ROLLOUT_UP_SPEC_JSON` plus `ROLLOUT_DOWN_SPEC_JSON` opt into a deliberate live
definition rollout; both require the direct database audit. The gate runs the same
fixed-rate proxy/health phase before scale-up, after scale-up, and after scale-down.
Worker `Ready` means every declared TCP port on every replica accepted a connection, so
the gate opens proxy streams immediately by default. `PROXY_READINESS_DELAY_MS` can add
an optional 0–60 second post-Ready diagnostic delay; it defaults to zero and is not
needed for listener startup correctness.
Platform liveness/readiness remains sampled once per second by default, while the
authenticated worker inventory is sampled once per 10 seconds (configurable from
10–300 seconds). This stays within the inventory endpoint's sustained query budget;
any HTTP 429 is retained separately as `worker_list_429` and fails the phase. Proxy
upgrade failures remain `proxy_handshake_failure`; errors after a successful upgrade
are reported independently as `proxy_stream_failure`.
Missing-response, invalid-marker, and stream-failure measurements begin only after an
HTTP 101 upgrade. Marker validation accepts text or binary WebSocket frames and a
marker split across frames; the isolated fixture defaults to
`Shared rsctf demo service`.
Every proxy endpoint must use a distinct player identity. The orchestrator fails before
provisioning unless fixed `RATE / FLEET` is at most 2 requests/s per identity, leaving
headroom below the platform's authenticated 150-request/60-second limit across repeated
base, scaled, and restored phases. Increase `FLEET` or reduce `RATE`; use
`MAX_PROXY_RATE_PER_IDENTITY` only when the tested deployment deliberately has a
different authenticated limit. HTTP 429 upgrades are counted separately as
`proxy_upgrade_429` and fail the phase.
Set `PLAYER_TOKENS` / `ADMIN_TOKEN` to explicit JWTs when desired.
`SKIP_DB_AUDIT=1` supports a remotely hosted API when direct PostgreSQL inspection is
unavailable, at the cost of skipping durable desired/observed-state assertions.
`WORKER_IDS` scopes the gate and fails if the scheduler places a workload elsewhere;
it does not change scheduler policy. Pin the challenge to a worker-local image or drain
other compatible workers when an exact-host run is required.

**Current trusted-worker baseline** (18 July 2026): one native Linux Docker agent,
12 workload slots, `FLEET=12 CYCLES=2 RATE=20 VUS=20 DURATION=20s`, and a two-service
workload scaled from 3 → 5 → 3 replicas. All 2,405 proxy streams returned the expected
application marker with zero handshake, stream, response, 429, 5xx, health, or inventory
failures. Across the six fixed-rate phases, proxy p50 was 49–51 ms and p95 was 52–59 ms.
The 29 warning-free resource samples completed inside those six load windows measured
21.80% total CPU on average (35.60% nearest-rank p95, 38.78% max) and 890.23 MiB peak
total RAM. This is a first held-rate resource baseline, not a before/after CPU result.
The same-shape readiness diagnostic improved valid delivery from 137/201 (68.159%) to
201/201 (100%); its old latency distribution was failure-censored, so this is a
correctness result rather than a CPU or latency optimization claim. See exact image and
binary hashes, every phase percentile, lifecycle convergence, the held-load table, the
complete 77-sample CPU/RAM series, and limitations in
[`REPORT.md`](REPORT.md#trusted-worker-functional-readiness-and-replica-campaign--18-july-2026).

Set `SUMMARY_JSON=/absolute/path.json` to retain the complete k6 metric distribution.
With `CYCLES>1`, the orchestrator inserts `.cycle-N` before the extension so one cycle
cannot overwrite another. A rollout run additionally inserts `.base`, `.scaled`, or
`.restored` for its three fixed-rate phases. For the remote `worker-plane.mjs` runner,
`SAMPLE_RESOURCES` is intentionally only a pre/post point sample. A performance claim
still requires a CPU/RAM time series or bracketed cumulative cgroup CPU at the same
fixed rate, and an optimization-ledger row requires before and after runs from the same
harness and workload shape. The isolated `worker-local` runner instead writes a
one-second series when `RESOURCE_JSON` is set.

#### Isolated current-tree Linux acceptance (`npm run worker-local`)

The local wrapper is the reproducible smoke/E2E path when no worker fleet is already
available. It builds the current Dockerfile and `worker-agent` release binary, generates
a seven-day throwaway worker PKI, and starts a uniquely named Compose project on two
random loopback ports. It then bootstraps an admin, enrolls one native agent through
`--token-stdin`, builds a worker-local HTTP fixture, provisions a real Jeopardy game and
Accepted teams, and invokes the same fixed-rate `worker-plane.mjs` gate. The challenge
is an aggregate two-service workload: two stateless primary replicas plus one sidecar
replica. Each cycle verifies the initial 2-service/3-replica topology, deliberately
rolls the live fleet up to five replicas (three primary plus two sidecars), verifies
Ready state and proxy traffic, then rolls back to three and proves the surplus Docker
replicas disappeared. Rollout convergence is measured separately from create/destroy.
The wrapper and gate both assert that the saved definition finishes at the base shape.
It passes the agent's host-network-boundary acknowledgement and unbounded-storage
development override only for this known local fixture; this acceptance run is not an
adversarial container-isolation or writable-layer quota test.

```sh
cd tests/load
export E2E_EXPECTED_TRACKED_SHA256="$(
  cd ../.. &&
  ! git submodule status --recursive | grep -q '^[+-U]' &&
  git submodule foreach --quiet --recursive \
    'git diff HEAD --quiet -- && test -z "$(git ls-files --others --exclude-standard)"' &&
  {
    printf 'HEAD\n'
    git rev-parse --verify HEAD
    printf 'DIFF_HEAD_BINARY\n'
    git diff HEAD --binary
    printf 'SUBMODULE_STATUS\n'
    git submodule status --recursive
  } | sha256sum | cut -d' ' -f1
)"
export E2E_EXPECTED_UNTRACKED_SHA256="$(cd ../.. && git ls-files --others --exclude-standard -z | LC_ALL=C sort -z | xargs -0 sha256sum | sha256sum | cut -d' ' -f1)"
SUMMARY_JSON=/tmp/rsctf-worker-e2e.json \
RESOURCE_JSON=/tmp/rsctf-worker-e2e-resources.json \
FLEET=5 RATE=10 VUS=10 DURATION=10s \
npm run worker-local
```

Both source fingerprints are mandatory. The wrapper verifies them before the
build, after the Docker build has consumed its context, and immediately before
resource sampling and k6 start. Any drift fails closed without accepting a
mixed-revision result. The tracked value covers the exact `HEAD`, staged and
unstaged changes relative to it, and recursive submodule commit/status output;
fingerprinting refuses a missing, mismatched, dirty, or untracked submodule.
The untracked value is the SHA-256 of the sorted `sha256sum` manifest, matching
the command above.

This requires `cargo`, `openssl`, `docker compose`, Node, k6, and an initialized
fixture submodule (`git submodule update --init --recursive examples/challenge-repository`).
The default project is
`rsctf-worker-e2e-<pid>-<random>`; its containers, network, volumes, PKI, agent state, exact
worker-labelled workloads, and temporary image tags are removed on success or failure.
It rejects the reserved `rsctf` name and any project/image collision before building,
and never runs `compose down`, SQL, or Docker cleanup without an in-process ownership
claim. Set `E2E_PROJECT` to another fresh lowercase name when desired. `E2E_HTTP_PORT` and
`E2E_WORKER_PORT` pin otherwise-random loopback ports. Repeated runs may reuse explicit
artifacts with `E2E_RSCTF_IMAGE`, `E2E_AGENT_BIN`, and `E2E_FIXTURE_IMAGE`; using those
overrides means the runner no longer proves that artifact came from the current tree.
`E2E_KEEP_IMAGES=1` retains only image tags built by the wrapper for inspection.
The Compose environment is allowlisted, ignores deployment `.env` files, pins local
storage and the all-in-one role, and defaults to the Docker `default` context; use
`E2E_DOCKER_CONTEXT`, `E2E_POSTGRES_IMAGE`, or `E2E_REDIS_IMAGE` only as deliberate test
overrides. The resource artifact records and verifies the actual running rsctf,
PostgreSQL, and Redis image IDs plus the running agent executable hash.

Direct database audits accept `PG_CONTAINER`, `PG_USER`, and `PG_DATABASE`; their
defaults remain compatible with the historical load stack. The isolated wrapper sets
all three explicitly for its private PostgreSQL container.

For a reportable fixed-rate local run, `RESOURCE_JSON` records a timestamped series
for the isolated rsctf/PostgreSQL/Redis containers, every worker-owned replica
container present at each sample, and the native agent process. The default interval
is 1 second; override it with `RESOURCE_INTERVAL_MS`. Pair this artifact with
`SUMMARY_JSON`, and retain the gate's Ready/Absent plus lease/session integrity result.
Every required rsctf/PostgreSQL/Redis/agent sample must contain complete CPU/RAM data;
otherwise the artifact is marked `invalid-metrics` and the runner exits nonzero.

For cleanup safety, `TEAM_EVIDENCE_DIR` must be
`/tmp/rsctf-team-event-evidence` or a suffixed sibling. WireGuard/JWT client material
lives in the run-scoped
`/tmp/rsctf-load-vpn-clients-g<game>-<run>` directory. Every client is labeled with
the exact owner, game, run, participation, and cohort index; creation records the full
container identities, and start, status, recovery, and teardown validate that ownership
before acting. Cleanup never sweeps a global team-client label or name prefix. A final
cohort-wide `/Ad/Targets` probe also verifies each automation token immediately before
container creation and rotates only a rejected credential, preventing an intervening
rotation from leaving one player on `401` responses for the event.

Lifecycle state can be isolated with `LIFECYCLE_STATE_TAG` (1–32 lowercase letters,
digits, or hyphens). The default manifest is `.lifecycle-state.json`; a tag such as
`competitive-100` selects `.lifecycle-state-competitive-100.json`. Use the same tag for
provision, lifecycle, and teardown. Manifests are written atomically with mode `0600`.
Unless `TEAM_EVIDENCE_DIR` is explicit, the same tag also selects
`/tmp/rsctf-team-event-evidence-<tag>`, so a later tagged run cannot erase a retained
event's per-team evidence.
Teardown scans every lifecycle manifest before deleting games, so a game retained by
any manifest is protected unless `DELETE_RETAINED_EVENT=1` is explicit. `RETAIN_EVENT=1`
also requires `KEEP=1`. Tags isolate saved state; BYOC relay names are still shared, so
provision and lifecycle hold one host-local exclusive lease. Distributed player names
and configuration directories are run-scoped. Every relay,
service, and flag volume also carries immutable game/challenge/participation ownership
labels; adoption and teardown require that exact identity and never sweep a name prefix.
A second
orchestrator fails before it can mutate state; the lifecycle's own provision child
inherits the same lease. A Linux abstract Unix socket is the authority, so the kernel
releases a dead owner's lease without stale-PID cleanup or a successor race; the owner
file remains mode `0600` diagnostic metadata only.

The sanitized findings and five-minute resource series from the one-hour, 100-team
distributed event are in [`REPORT.md`](REPORT.md).

## Baselines & findings (single-node, docker)

**Current immutable deployment acceptance** (20 July 2026) used two web replicas,
one singleton control replica, PostgreSQL 18.4, Redis, Caddy, 100 Jeopardy teams,
400 A&D/KotH teams, and 80 real BYOC tunnels. The exact
`rsctf-local:deploy-20260720-1` image served 627,022 requests at 1,995.612
requests/s over the five-minute fixed load. All 627,618 checks passed, with zero
unexpected non-2xx responses, zero server 5xx, zero PostgreSQL deadlocks, zero
restarts or OOM kills, and clean duplicate, overlap, cadence, evidence, and
cleanup gates. Expected quota enforcement accounted for 179,875 HTTP 429
responses (28.687%). The run also exercised 36 real container lifecycle
operations, attachment upload/download, 80 scoped KotH captures, five crown
cycles, stale-capability rejection, and two confirmed acquisitions.

The final proxy policy addresses a separate mutation-only Caddy failure found by
two strict pre-acceptance runs. Reads and stateful network routes retain pooled
upstreams, while ordinary `POST`, `PUT`, `PATCH`, and `DELETE` requests use the
dynamic Docker-DNS transport with keepalive disabled; ambiguous mutations are
not blindly retried. A focused 120-second POST storm served 143,429 requests at
1,195.195 requests/s with zero proxy 5xx and an 8.75 ms p95, and the full
lifecycle recorded no Caddy 5xx or closed-idle errors. The accepted image ID,
binary hash, Caddy digest, endpoint distributions, resource series, and complete
failure-to-acceptance sequence are in
[`REPORT.md`](REPORT.md#final-immutable-two-replica-lifecycle-acceptance--20-july-2026).

The original 19 July diagnosis remains relevant: `/admin/teams` changed from a
502 in 3,143.541 ms to a 200 in 54.955 ms, and repository rescans now update
challenges in place by stable manifest identity while preserving solves, first
solves, IDs, counters, attachments, and runtime evidence. Solve rows already
removed by the former delete/recreate sync cannot be reconstructed without a
backup. The earlier fixed-rate replica-churn and Redis-outage comparisons remain
documented in
[`REPORT.md`](REPORT.md#repository-sync-and-event-integrity-acceptance--19-july-2026).
The 20 July acceptance adds no optimization-ledger row because it is correctness
work without a same-harness before/after CPU bracket.

**Earlier replicated player-load baseline** (`npm run player`, 16 July 2026): two
web replicas plus singleton control, 400 A&D/KotH teams, 100 Jeopardy teams,
`RATE=70 VUS=400 DURATION=300s`, and public TLS. The final measured campaign
image sustained **429.70 req/s** with **0 failed requests, 0 server 5xx, and
clean integrity**.
Overall HTTP p95 was **6.88 ms** and combined-board p95 was **7.44 ms**. At the
same rate and fixture shape, seven optimizations reduced measured stack CPU from
345.88 to 232.48 CPU-seconds (**−32.79%**). See the full endpoint distributions,
resource series, image digests, SQL evidence, and limitations in
[`REPORT.md`](REPORT.md#fixed-rate-replicated-hot-path-optimization-campaign--16-july-2026).

The exact final image from that campaign also passed the comprehensive lifecycle gate at
86.35 req/s for five minutes: zero 5xx, zero invalid A&D/KotH models, 513/513
liveness and readiness probes, all integrity checks clean, 20/20 BYOC delivery
and checker verification, and complete container, attachment, capture,
crown-cycle, and stale-token coverage. This is functional acceptance, not an
extra row in the fixed-rate optimization ledger; details and the two retained
diagnostic attempts are in [`REPORT.md`](REPORT.md#final-exact-image-lifecycle-acceptance).

An earlier 19 July singleton campaign used 100 distinct player tokens at
`RATE=20 VUS=128 DURATION=60s`: 7,552 requests completed at 117.057 requests/s
with zero failed requests, server 5xx responses, or semantic board errors.
Against the same-shape operational candidate, overall HTTP p95 improved
8.236 → 5.768 ms (−30.0%), combined-board p95 improved 6.972 → 4.411 ms
(−36.7%), and sampled whole-stack CPU fell 31.82 → 29.10% of one core
(−8.5%). The representative lifecycle rerun also passed every integrity gate
at 86.340 requests/s with 4/4 live checker-verified tunnels, a 100-team frozen
roster, zero 5xx, 319/319 liveness/readiness probes, real container and asset
operations, a confirmed KotH acquisition, and stale-token rejection. This
bundled operational comparison has no causal optimization-ledger row; exact
endpoint distributions, the CPU/RAM series, image identities, saturation
diagnostic, harness corrections, and limitations are in
[`REPORT.md`](REPORT.md#historical-singleton-operational-acceptance--19-july-2026).

### Optimization ledger

All 16 July rows compare adjacent images with the common workload above. The
max-batch 19 July row is a frozen pre-submit-fence campaign using the isolated
A&D harness documented in
[`REPORT.md`](REPORT.md#attack-defense-max-batch-hardening-and-fixed-rate-optimization--19-july-2026),
so its held throughput and CPU window are not comparable to the replicated
player-load rows. App CPU is both web replicas plus control for the 16 July
campaign and the single web-only rsctf container for the 19 July campaign;
stack CPU adds PostgreSQL and Redis. The Caddy and Redis incident rows compare
the same fixed scheduler setting before and after, but CPU was not bracketed and
is therefore shown as unavailable. A row is retained even when one secondary
metric regresses, so the ledger does not hide the cost of an optimization.

| Date | Change | Held-rate throughput | Direct work reduction | App CPU-s | Stack CPU-s | Relevant p95 | Result |
| --- | --- | ---: | --- | ---: | ---: | ---: | --- |
| 2026-07-16 | Batch authenticated limiter policies | 429.20 → 429.72 req/s | Redis commands −12.01% | 157.47 → 155.20 | 345.88 → 339.48 | HTTP 9.13 → 9.17 ms | 0 5xx; clean |
| 2026-07-16 | Cache KotH lifecycle with round fencing | 429.72 → 429.34 req/s | SQL calls −98.52% | 155.20 → 151.51 | 339.48 → 316.10 | KotH State 9.20 → 7.83 ms | 0 5xx; clean |
| 2026-07-16 | Set-based closing-SLA evidence query | 429.34 → 429.11 req/s | Snapshot median −57.31% | 151.51 → 150.92 | 316.10 → 309.80 | HTTP 9.32 → 8.97 ms | 0 5xx; clean |
| 2026-07-16 | Fetch the A&D State live tail in one query | 429.11 → 429.46 req/s | Tail statements −75.02% | 150.92 → 148.25 | 309.80 → 301.35 | A&D State 12.85 → 10.51 ms | 0 5xx; clean |
| 2026-07-16 | Coalesce and batch activity writes | 429.46 → 429.44 req/s | Update statements −93.54% | 148.25 → 152.78 | 301.35 → 295.48 | HTTP 8.80 → 9.02 ms | 0 5xx; clean |
| 2026-07-16 | Cache narrow KotH eligibility sets | 429.44 → 429.41 req/s | Challenge-query calls −99.10% | 152.78 → 126.42 | 295.48 → 244.08 | KotH token 9.21 → 6.61 ms | 0 5xx; clean |
| 2026-07-16 | Replace two participation reads with one join | 429.41 → 429.70 req/s | Statements −51.73% | 126.42 → 121.14 | 244.08 → 232.48 | HTTP 7.30 → 6.88 ms | 0 5xx; clean |
| 2026-07-19 | Memoize repeated A&D batch work and bound victim lookup | 1 → 1 batch/s target | Adjudications 100 → 1/repeated batch | 6.033 → 0.135 | 20.742 → 0.850 | Repeated-batch 694.49 → 38.74 ms | 0 429/5xx; attacks 107 → 107; unknown p95 +6.42% |
| 2026-07-19 | Refresh Caddy Docker DNS and retry replica connection failures | 20 → 20 iterations/s target | Retired-address 5xx 800 → 0; drops 33 → 0 | — | — | HTTP 3,001.818 → 7.521 ms | 0 5xx after; over-quota 429 retained |
| 2026-07-19 | Bound Redis commands and fall back to the local limiter during outage | 1 → 1 request/s target | Outage responses 15/15 → 16/16 expected 400 | — | — | HTTP 18,084.879 → 207.823 ms | 0 drops after; recovered without restart |

At the same one-batch/s load, the 100-distinct-known case also improved: p95
790.76 → 367.97 ms and stack CPU 21.644 → 10.811 CPU-seconds. The
distinct-unknown control regressed to p95 68.92 → 73.35 ms and stack CPU 1.757
→ 2.079 CPU-seconds because the miss now performs the authoritative active
service eligibility lookup. Full trial distributions, exact image/binary
digests, fixture controls, and the two-replica limiter drill are retained in the
report rather than compressed into the ledger row.

The Caddy row's 20-identity run intentionally exceeded the authenticated
per-user quota; its 429 responses are disclosed and the row claims only the
retired-upstream 5xx/drop/p95 change. The Redis row compares the same scheduled
arrival rate: the old requests blocked long enough that k6 drained at only
0.509901 requests/s, so it is not presented as an achieved-throughput win. No
CPU claim is inferred for either incident row.

### Historical comparison

| Date | Change and fixed load | Throughput | rsctf CPU time | Whole-stack CPU time | Board p95 | 5xx |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 2026-07-13 | KotH query/serialization refactor; game 119, `RATE=20 VUS=60 DURATION=60s npm run player` | 120.44 → 120.71 req/s | 6.84 → 5.41 CPU-s (−20.9%) | 9.68 → 9.52 CPU-s (−1.7%) | 49.11 → 47.19 ms | 0 → 0 |

That localhost comparison was dominated by expected rate-limit responses because the
direct `:8080` source is not the configured trusted proxy; it is retained only as a
same-rate CPU/throughput comparison. Functional acceptance comes from the lifecycle gate,
which exercises confirmed capture, reset, stale-token rejection, and integrity checks.

**BYOC scale — idle** (`npm run byoc`, `npm run scale`): idle tunnels are cheap and
**linear** at ~0.08 % CPU/tunnel (median of many samples, 30→300: 3.4 % → 10.5 % → 20 %;
300 idle tunnels = 0.2 of one core), a few MiB each — each tunnel's re-drive is lock-free
and touches no shared state, so there is no O(N²) site (an earlier "sub-linear / 60→120
jump" was single-sample noise). Requests route cleanly through the tunnels (k6:
**~4 700 req/s, 0 non-200, 0 5xx**). The real ceiling isn't idle — it's the **per-tick
checker sweep**: one `'S'` probe per BYOC service each tick, concurrency capped at 8, so it
_lengthens_ rather than heats (~6 cores for its burst at 300 services, idle between ticks).

**BYOC under attack traffic** (`npm run busy`; `NOKEEPALIVE=1` = a fresh `'S'` stream per
request — the realistic pattern, since HTTP keep-alive doesn't persist across the tunnel):
a busy tunnel costs **~0.8 % CPU/tunnel, ~9× the idle floor**. rsctf serves **3,400
churn-req/s through 80 tunnels at ~0.7 cores, 0 errors, p99 271 ms**. Pushed to ~5,000
req/s (120 tunnels, 1200 VUs) throughput plateaus and the excess is shed as
**transport-level failures (0 % 5xx, ~50 % non-200) while rsctf CPU stays ~71 %** — the
limit is the connection-churn rate (OS TIME_WAIT / ephemeral ports), not rsctf compute.
rsctf never errors or falls over; it backpressures. Not a DoS.

**Worst case — reconnect storm** (`npm run worst-case`, 120 tunnels): restarting rsctf
drops every tunnel; the agents reconnect on their 3 s backoff. rsctf **stays
responsive** — healthz p95 **3 ms**, peak CPU ~10 %, RAM ~205 MiB (bounded by the
16 MiB/tunnel yamux window cap), all 120 re-registered in ~12 s, **0 panics**. The only
failures are the ~1–2 s restart downtime itself, not the reconnect load. So legitimate
BYOC at scale is **not a DoS vector** — the storm is absorbed. (An attacker can't forge
it: each agent capability needs both the game secret and that Accepted team's rotatable
secret, and the server revalidates the live participation.)

Push harder with `N`, `VUS`, `DURATION`; the signals to watch are `server_5xx` (must
stay ~0), healthz responsiveness during a storm, and duplicate rounds/KotH rows.

## Whole-platform lifecycle (`npm run provision` → `npm run lifecycle`)

The realistic full-event test: two seeded events (a Jeopardy game + a combined A&D/KotH
game), hundreds of teams, and every user journey at once — register→login→team→join
(live), jeopardy poll/detail/submit, A&D state/targets/submit/board, KotH board/token/
state, anonymous browsing, an admin monitor feed, and a concurrent same-flag dedup burst.

- `applib.mjs` — organizer setup (`/api/edit`), atomic bulk cohort SQL seed, official epoch
  readiness assertions, live auth/team/join, namespaced teardown (`@load.test` / `LT_` /
  fresh game ids — never touches games 9/10).
- `team-clients.mjs` — creates, monitors, and safely removes the distributed per-team
  WireGuard+k6 clients used by realistic A&D event runs. Each client mints an official
  `ad_` automation token and uses it for target polling and captured-flag submission;
  user-only state remains authenticated by its session JWT. The start barrier verifies
  a handshake for every exact expected WireGuard public key, rather than accepting an
  equal count of unrelated peers.
- `k6/team-event.js` validates the A&D state readiness contract and waits while
  `flagsReady` is false. Synchronization waits are recorded as `flag_sync_waits`, not
  VPN failures, and clients never attack a flag left over from the previous round.
  The zero-tolerance `server_5xx`, `unexpected_non_2xx`, and `request_timeout` gates
  cover only RSCTF API calls after one bounded retry of a transient GET (timeout, 429,
  or 5xx). Schema-v9 evidence retains first failures, retry attempts, recoveries, and
  exhaustions instead of hiding them. A failed idempotent VPN flag read is likewise
  retried once after 100 ms; `vpn_first_attempt_failure` and `vpn_retry_attempts` retain
  that transient evidence, while `vpn_attack_failure` remains the zero-tolerance
  post-retry gate.
- `provision.mjs` — stands the two events up, installs a prepared exact checker, seeds
  `TEAMS_JEO`/`TEAMS_AD`, then waits for the automatic scheduler to freeze the complete
  A&D roster and settle the current publication pipeline → `.lifecycle-state.json` or
  its tagged variant.
  Default capacity mode does not start a relay for every seeded team. Competitive mode
  pauses official scoring, starts the complete relay and isolated-service cohort, and
  resumes scoring only after every service is reachable. `-- --down` tears the namespace
  down. It writes an atomic recovery manifest before creating resources, so a later
  teardown can find a namespace left by an interrupted provision. Provision warms durable
  finalized-epoch rollups before any timed measurement; lifecycle repeats that assertion
  after restart.
- `k6/lifecycle.js` — the multi-scenario load (onboarding / jeopardy / ad / koth / browse
  / monitor / dedupBurst), each VU on a distinct `X-Real-IP`.
- `lifecycle.mjs` — preflight (JWT secret), fail-fast checks for `startRound`, the frozen
  roster, current planted flags, and an active crown cycle, then starts exactly `FLEET`
  relays and requires a post-connect round with durable delivery and exact checker
  evidence for those selected participations. Seeded non-fleet services remain
  platform-void rather than forcing hundreds of idle relay containers. It then runs k6,
  healthz sampling, integrity checks, and teardown. Before distributed clients start,
  scoring is paused again; it resumes at their common start barrier after every expected
  WireGuard peer is authenticated. The event deadline is aligned to that barrier with a
  configurable 45-second grace (`EVENT_END_GRACE_SECONDS`). This exceeds k6's default
  30-second graceful-stop allowance, so in-flight work cannot consume the entire
  settlement window. In capacity mode, the host
  capture driver holds one exact cycle-scoped capability across checker rounds so the load
  covers provisional → confirmed acquisition, then switches to an eligible challenger
  after a pristine reset. Before the next valid capture it plants the revoked prior-cycle
  token and requires a scorable checker observation that rejects it. It never calls the
  disabled manual round endpoint.

```sh
npm run provision                         # 300 + 300 teams by default
VUS=400 DURATION=300s KEEP=1 npm run lifecycle
npm run teardown                          # if KEEP=1 was set
```

### Competitive player simulation

`REALISTIC_COMPETITION=1` changes the distributed lifecycle from uniform request load
into a replayable model-v2 competition. `player-model.js` is shared by Node and k6; run
its unit tests with `npm test`. Each client receives only its own profile. Engagement and
specialty are allocated independently, so attendance is not a proxy for technical skill.
For exactly 100 teams, the engagement mix is 10 always-on, 25 committed, 45 part-time,
and 20 casual teams; offense, defense, KotH, Jeopardy, and balanced specialties each have
20 teams. Specialty offsets are constrained by a per-profile skill budget instead of
placing every ability on one strongest-to-weakest axis.

Teams alternate between active and idle 90-second session blocks and use different think
times. Every scoring round gives a player a finite action-credit budget. An attack, KotH
claim, or Jeopardy research attempt costs one credit; patching or starting an attachment
or container journey costs two. A team therefore cannot attack, defend, capture the hill,
and solve every challenge without tradeoffs. A&D target selection uses only the public scoreboard and
that team's local history of captures, patched techniques, unavailable services, and
transport failures. Rank pressure, rivalry, exploration, and a rotating scan diversify
targets without revealing another team's private profile. A player who steps away stops
making decisions, while an open browser tab keeps one low-frequency background state
refresh; this preserves realistic passive traffic without turning an idle team into a bot.

Defense changes exercise more than a successful patch response. A patch can temporarily
leave the fixture service healthy, mumbling, or offline according to the team's defense
skill and risk; exploit attempts observe that state, and the defender must spend later
credits to repair it. Every Jeopardy player sees the same public catalog, then chooses live
work from its skill, category affinity, persistence, prior attempts, and remaining credits.
There is no assigned solve count, challenge subset, winner, or unlock time. Players may
abandon a difficult problem, research without submitting, make an ordinary wrong guess,
or discover the answer. Each player independently chooses occasional Jeopardy focus
rounds and performs at most one Jeopardy action in such a round, preventing a synchronized
opening solve dump while keeping activity across the full event. Attachment and container
journeys begin only when that player chooses to spend the extra credits; created containers
are held briefly and then deleted.

KotH clients write their current scoped token through the hill's real network `/capture`
endpoint. Each team independently decides whether and when to challenge from the live
cycle state, public rank, cooldown eligibility, local observations, remaining credits,
and its own KotH skill, risk, and persistence. Different reaction delays and probabilistic
decisions create opening claims, takeovers, interrupted confirmations, and longer control
periods. Skill shifts a player toward a later one of three observation slots; independent
seeded jitter still decides the order among players in the same slot. After observing a
provisional claim, each player independently weighs a late challenge against saving its
remaining credits. The nonlinear skill term gives specialists a durable advantage without
making a decision for them. This creates contested and uncontested confirmation
opportunities without a shared truce schedule. No winner is preselected, and the
orchestrator never selects a contender or outcome. A failed network write remains
pending and is retried once on a later observation after refreshing the target. The token,
state, and target must agree on the authoritative round, and state and target must publish
the same crown-cycle generation, so a concurrent reset cannot send a player to the
destroyed hill without exposing the underlying Docker identity.
Authoritative cycle transitions must account for every pending attempt.

Competitive evidence uses schema v9. Every mandatory counter is written explicitly,
including zeroes, in an isolated directory for that run and team. The collector binds
each file to the competition run ID, event creation time, mixed and Jeopardy games, KotH
challenge, scoring-start round, frozen participation, expected filename, and generation
window. Platform first-attempt failures are split into timeout, rate-limit, and server-error
counters whose sum must match the retry ledger. A recovered 429 therefore remains visible
without failing the run, while any first-attempt platform 5xx fails its dedicated threshold.
Every iteration is classified as active or idle before its work begins. A caught runtime
error increments `iteration_runtime_errors`, is written to that team's `runner.log`, and
fails a zero-tolerance threshold. The conservation rule is `classified = completed +
runtime errors + unclassified hard-stop tail`. Because each client has exactly one VU,
only one unexplained tail iteration is allowed per client when k6 stops in-flight work;
there is no percentage-based tolerance. Every competitive team directory must retain a
regular `runner.log` of at most 1 MiB beside `summary.json`, including an empty log when no
runtime messages were emitted. Every distributed runner, including capacity mode, inherits
a POSIX file-size limit before k6 starts, so the kernel refuses writes beyond the boundary
instead of allowing the log to grow until the post-run audit. A caught iteration error
records its counter and sanitized message once, then aborts the k6 run immediately. Both
evidence collectors also reject missing, non-regular, or oversized runner logs.
A&D evidence counts one logical capture when an exact flag is discovered, records every
identical-flag replay, and requires every logical capture to end in an accepted, duplicate,
or terminal verdict. Pending submissions are retried before new attacks, even while the
player is idle or draining at event close, so unresolved captures cannot silently pass.
Per-team and fleet-wide KotH conservation checks require attempts, successes, classified
failures, claims, and pending resolutions to balance exactly.
The current checker-observed controller may spend the same finite action credits used by
other defenses to patch its hill. Patch, repair, blocked/bypassed takeover, healthy-hold,
and replacement-observed patch-loss counters are conserved separately. A healthy hold
requires an uninterrupted same-holder observation into a later scoring round plus a fresh
`/status` response from the same instance with the applied patch still healthy. After a
reset, a different instance identity proves that the old patched runtime is gone; the new
runtime may already have been patched again before that player-side sample arrives. The
post-run database gate separately validates the durable destroy, same-image create, and
functional-readiness receipt chain that establishes pristine recreation. Immediately
before a patch or repair, the client refreshes authoritative state and proceeds only when
the same round and cycle still identify it as the controller. A final network race can
still fail, so the combined operation failure rate remains capped at 25%; status-proof
failures and retained reset state remain zero-tolerance.

Settlement also waits for the post-deadline KotH cleanup receipt and proves that every
hill capability is revoked, claim and target runtime state is cleared, container
bookkeeping is gone, and enforced cooldowns are released. Finalized cycle evidence remains
intact while this mutable runtime state is removed. Jeopardy acceptance counts distinct
team/challenge first-solve pairs, not repeat submissions, and reconciles client attachment
and container counters with durable download/start/destroy events in PostgreSQL.

Acceptance does not rely on client counters alone. Baseline-scoped SQL requires broad A&D
competition, varied attacker counts, temporally distributed A&D and Jeopardy activity,
Jeopardy solve dispersion and full catalog coverage, durable KotH control/acquisitions,
leader changes, and clean duplicate/cross-cycle checks. Final A&D, KotH, and Jeopardy
boards must contain the exact roster in the platform's deterministic order with ordinal
ranks. A long, single-hill A&D run must still produce at least 20 distinct final scores
and a three-point field range; the latter reflects the fixed normalized 100-point budget
without manufacturing a winner merely to satisfy the harness. Outcome gates also check
that each technical specialty produces the expected
field-relative lift, at least 90% of defense incidents have a recorded repair, action
scarcity is observable, and attachment/container journeys complete. Late unresolved
incidents remain visible evidence instead of being erased. Health/readiness, zero unexpected API responses,
exact BYOC delivery/checker evidence, and exact WireGuard peers remain mandatory.
KotH specialty lift is evaluated only for runs of at least 30 minutes: a shorter run has
too few confirmed windows to demand that a particular cohort win without preselecting an
outcome. Its capture, control, confirmation, and leader-competition gates still apply.

Example retained one-hour setup (this is a command template, not a recorded result):

```sh
export LIFECYCLE_STATE_TAG=competitive-100
export REALISTIC_COMPETITION=1
export LIFECYCLE_ISOLATED_SERVICES=1

TEAMS_JEO=100 TEAMS_AD=100 EVENT_DURATION_SECONDS=10800 npm run provision

TARGET=https://tcp.1pc.tf FLEET=100 DURATION=1h KEEP=1 RETAIN_EVENT=1 \
  DISTRIBUTED_TEAM_CLIENTS=1 REQUIRE_ISOLATED_SERVICES=1 \
  TEAM_START_DELAY_SECONDS=90 EVENT_END_GRACE_SECONDS=45 \
  SIMULATION_SEED=rsctf-competitive-v2 \
  INTEGRATED_CHEAT_SIMULATION=1 CHEAT_AT_FRACTION=0.45 npm run lifecycle
```

With `INTEGRATED_CHEAT_SIMULATION=1`, the anti-cheat drill starts at the chosen
event-progress fraction (default `0.45`) while ordinary team clients continue playing.
Its schema-v3 result is bound to the same run, event, challenge, child-process window,
six simulated offenders, and clean-control cohort. The exact stolen-flag,
wrong-submission, honeypot, risk-band, duplicate-evidence, and clean-control checks must
all pass before the result is merged into lifecycle evidence. Use a value strictly
between `0.1` and `0.9`; the drill requires at least 100 mixed-event teams.

The retained event remains available for review. Delete it only when that evidence is no
longer needed:

```sh
LIFECYCLE_STATE_TAG=competitive-100 DELETE_RETAINED_EVENT=1 npm run teardown
```

This mode is still a deterministic bot model, not a claim that bots reproduce human
adaptation, communication, mistakes, or novel exploit development. Fixture patching
now exercises patch-induced service degradation and repair, and the distributed player
path exercises Jeopardy detail, attachment, and container journeys. The generic
`k6/lifecycle.js` path remains useful for organizer setup, onboarding, anonymous browsing,
attachment upload, and other platform-wide operations that are not player decisions.

### Retained anti-cheat drill

The anti-cheat scenario runs only against the fresh lifecycle namespace. It requires at
least 100 mixed-event teams and two explicit safety switches: `CHEAT_SIMULATION=1`
authorizes the controlled bad behaviour, while `KEEP=1` guarantees the event remains
available for review.

```sh
TEAMS_JEO=20 TEAMS_AD=100 EVENT_DURATION_SECONDS=86400 npm run provision
CHEAT_SIMULATION=1 KEEP=1 TARGET=https://tcp.1pc.tf npm run cheat
```

The drill freezes the roster and evidence baseline before it mutates the event. It selects
six participants with no prior actionable evidence, submits four
other-team dynamic flags, coordinates 40 rapid wrong submissions from one team across
five authenticated accounts, and visits three same-origin honeypot routes from another
team. Every other frozen roster member is a clean control, including a team with
actionable evidence from ordinary play before the baseline. It runs the monitor sweep
three times concurrently, then
requires `StolenFlag`, `HighWrongRate`, `AutomatedPattern`, `HoneypotHit`, and
`HoneypotChain` evidence, the expected risk bands, no actionable clean-team false
positive, no duplicate evidence, and no unexpected HTTP response. Context-only network
correlations remain non-accusatory; shared addresses spanning more than four teams are
suppressed as event-NAT/campus noise.

For a 100-team integrated run, the result must name exactly six offenders and all 94
controls; a standalone run covers the exact non-offender complement of its frozen roster.
Each run baselines submissions, honeypot rows, and suspicion events before it creates any
drill fixture or starts k6. It
then requires exactly four actor-and-answer-matched stolen submissions, 40 distinct
actor-and-answer-matched wrong submissions, three total rows covering the three expected
baits, and new detector evidence bound to those submissions, actors, challenge, and
evidence keys. The five fixture-only bot accounts move to the selected brute-force team
and receive fresh security stamps, which creates fresh authenticated limiter partitions
without weakening the production rate policy. The fresh-actor selection and exact
post-baseline evidence checks prevent retained findings from making a broken rerun pass.
Standalone drills own the host orchestration lease; an integrated child validates and
inherits the lifecycle parent's lease token.

The runner prints direct admin links and adds only non-secret result metadata to
`.lifecycle-state.json`; the ignored mode-0600 lifecycle file itself remains sensitive
because provisioning stores flags and security stamps there. The temporary k6 JWT/flag
input is removed from `/tmp` immediately after k6 finishes. A retained namespace cannot
be torn down or reprovisioned accidentally. When
review is genuinely complete, deletion requires the explicit override:

```sh
DELETE_RETAINED_EVENT=1 npm run teardown
```

For a long event, distributed mode starts one constrained k6 container per team. Each
client gets a distinct Traefik-network source IP, downloads its own WireGuard
configuration, and attacks the other teams' real BYOC listeners over the VPN. Isolated
service mode gives every team a separate flag-service container instead of sharing one
fixture process:

```sh
TEAMS_JEO=20 TEAMS_AD=100 EVENT_DURATION_SECONDS=10800 npm run provision

TARGET=https://tcp.1pc.tf FLEET=100 VUS=100 DURATION=1h KEEP=1 \
  DISTRIBUTED_TEAM_CLIENTS=1 LIFECYCLE_ISOLATED_SERVICES=1 \
  REQUIRE_ISOLATED_SERVICES=1 TEAM_THINK_SECONDS=5 npm run lifecycle

npm run teardown
```

Run `observe.mjs` alongside a long lifecycle run to sample the public and local health
paths, core containers, full workload fleets, PostgreSQL, Redis, and the host. Its
metadata uses an explicit non-secret allowlist; JWTs, database passwords, WireGuard
keys, and capabilities are never serialized.

The 13 July 2026 one-hour run kept the public health path available for 360/360 attack-
window probes, but found event-critical round drift, an 11.1% VPN attack-path failure
rate during full reconciliation, nine scoreboard 500 responses, and a KotH epoch that
could not settle. Treat that run as a diagnostic baseline, not a passing capacity claim;
see [`REPORT.md`](REPORT.md) for the full evidence and prioritized remediation list.

Historical crown-cycle acceptance run (13 July 2026, `VUS=400 FLEET=4
DURATION=300s`) sustained approximately **9,555 requests/s** with **0 % unexpected
non-2xx responses** and **0 % server 5xx responses**. The health probe succeeded
493/493 times (p95 8 ms, max 41 ms). Endpoint p95 values were 308 ms for the
combined board poll, 252 ms for the A&D epoch board, 97 ms for Jeopardy details,
and 166 ms for A&D submit. The driver completed two crown cycles, confirmed one
acquisition, and accepted 45 KotH capture writes. Every duplicate, overlapping
cycle, stale-container, cross-cycle token, void-evidence, cooldown-window, holder,
runtime-operation, and rollup integrity check was zero after the run.

KotH provisioning builds a dependency-free competitive hill fixture whose
port-8080 response matches the functional checker. A paired immutable
`KOTH_CONTAINER_IMAGE`/`KOTH_CONTAINER_PORT` override can replace it. The first
bootstrap hill carries the same hashed `rsctf.managed` and `rsctf.scope`
installation identity as the isolated server; an unlabeled or foreign-scoped hill is
correctly rejected by runtime reconciliation. The first
official boundary snapshots the 12-tick epoch, 3-tick crown cycle,
1-tick champion cooldown, 2-tick claim confirmation, roster, hill image, and service
weight. The lifecycle worker then destroys that bootstrap hill and adopts one managed
replacement from the same image. Because round advancement waits for the checker pipeline,
the full lifecycle gate uses a conservative 300-second run: `DURATION>=120s` requires at
least one confirmed acquisition, `DURATION>=240s` also requires a completed crown cycle,
and `DURATION>=300s` requires the checker to reject a revoked prior-cycle token. Override
only for a deliberately shorter diagnostic run with `CROWN_MIN_ACQUISITIONS=0`,
`CROWN_MIN_COMPLETED=0`, or
`CROWN_MIN_STALE_REJECTIONS=0`.

Post-load integrity gates cover duplicate cycle rows, simultaneous active cycles,
duplicate acquisitions/control observations/scoped tokens, duplicate runtime operation
identities, cross-cycle token attribution, stale-container evidence, scorable platform
voids, malformed cooldown spans, and a holder projection that disagrees with the current
active cycle. The k6 player and admin KotH pollers separately fail on malformed
formula/cycle/confirmation, reset, provisional, cooldown, recovery, container-identity,
or receipt fields.

Setup notes: captcha (`AccountPolicy:UseCaptcha`) must be off in `Configs` for the live
onboarding cohort (a bot gate, not a load concern — the runner assumes it's disabled;
restore it after); register needs a 64-hex `fingerprint` (any well-formed one is
accepted). A&D/KotH accepted participations are DB-seeded (an API accept on a self-hosted
game would spawn a container per team). `EPOCH_READY_TIMEOUT_SECONDS` controls the
automatic-boundary wait (default 360 seconds); a timeout reports the observed checker,
roster, round, and flag counts instead of starting k6 against partial state.

Historical findings (600 teams, 400 VUs, 120s, single node): **~1,270 req/s, 0 % server 5xx, 0 %
non-2xx**, every integrity check clean (0 duplicate rounds/attacks/KotH-tokens/
participations, 0 container leak, 0 panics), and healthz stayed at **p95 10 ms**
throughout. rsctf ~2.7 cores / 2 GB, Postgres ~2.5
cores / 296 MB — the RAM + the 20 s onboarding p95 are the **argon2 register flood**
(memory-hard by design, on `spawn_blocking`, so healthz never blocked). This run **found
a real bug** — a first-blood race (concurrent correct submits 500'd on `pk-FirstSolves`),
fixed with an `ON CONFLICT` claim.
