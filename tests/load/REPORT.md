# RSCTF load-test report

> Storage status (16 July 2026): the user-authorized PostgreSQL 18 reset removed
> the previously retained event rows. Historical sections below describe the
> database state at the time of each run; those games are no longer visible in
> the live platform.

## Exhaustive admin lifecycle reporting protocol

`npm run admin-lifecycle` is the destructive, disposable-stack acceptance gate for the
privileged namespaces `/api/admin`, `/api/ad/admin`, `/api/admin/workers`, and
`/api/workers/enroll`; it does not claim complete `/api/edit` organizer coverage. The
pure catalog is source-checked against those registered routers and requires **61/61 HTTP
method/path operations** plus both admin SignalR surfaces. All 59 Admin-only operations
must reject anonymous, User, and Monitor principals; participation and enrollment use
their separate manager/token matrices. A passing execution must also retain zero unauthorized successes, server 5xx,
invalid response models, HTTP 429 responses, dropped iterations, failed public/direct
health probes, panic/fatal log records, and namespaced resources after cleanup. The
positive flow includes real Docker build/image/container mutations, a real repository
binding scan, and one-time worker certificate enrollment; these are not mocked route
smokes.

The reproduction contract and exact destructive acknowledgements are documented in
[`README.md`](README.md#exhaustive-admin-lifecycle-npm-run-admin-lifecycle). Every
accepted execution is recorded here with all of the following evidence:

1. Source commit plus tracked/untracked source fingerprint and exact release image/binary
   identity.
2. Isolated topology: public origin, two direct web replicas, singleton control replica,
   PostgreSQL, Redis, Docker backend, worker issuer, repository fixture, and SMTP mode.
3. One-shot route coverage count and latency p50/p95/max; the 59-operation Admin
   authorization matrix; manager/enrollment matrices; SignalR negotiate/connect result;
   all 74 read/origin preflight pairs; replica-projection result; fatal-log count; and both
   stable cleanup/leak snapshots.
4. The retained `SUMMARY_JSON` distribution: scheduled/achieved rate, VUs, duration,
   request/check counts, 5xx/429/unexpected/invalid/dropped counts, and admin/health plus
   per-operation p50/p90/p95/p99/max.
5. An aligned CPU/RAM time series for every web/control replica, PostgreSQL, Redis, and
   proxy over the same fixed-rate window when making any performance claim.

The one-shot destructive route timings and pre/post resource points are functional
diagnostics, not an optimization comparison. Do not add an optimization-ledger row until
one isolated fixture has been run before and after at the same fixed arrival rate and the
same workload shape. Retain regressions and secondary-metric costs rather than reporting
only improved endpoints. The first current-tree execution record belongs immediately
below this protocol after its isolated run; no unexecuted latency or CPU values are
claimed here.

## Production admin-instance ownership rollout — 24 July 2026

This bounded production diagnostic covers the `/api/admin/instances` ownership fix, not
the exhaustive disposable admin lifecycle. The old deployment was
`0.1.12` / `a39caf2c72e35236b94e93bbca6a6e1c1fcacf06` /
`ghcr.io/dimasma0305/rsctf@sha256:ecba5305ef7022bd0c91b42a58da7ee120dcf730a8a5ccf20cf5ec2297223de0`.
The accepted deployment is `0.1.13` /
`b6c6e820b4be51e06aca6f8284a6bc1a40350418` /
`ghcr.io/dimasma0305/rsctf@sha256:8034cdae914e662b4abaa6c68434096d3e1abe5fdc1c5f0cfbc8b96a849e826f`.
Both used the same public TLS origin, two web replicas, singleton control replica,
PostgreSQL data, Redis instance, and one-row live container inventory.

The measured request was
`GET /api/admin/instances?count=100&skip=0` with an Admin token. Each pass scheduled
exactly one request per second for 30 seconds. All 90 measured requests returned HTTP
200 and valid JSON; there were no 429 or 5xx responses. Web CPU is the bracketed
`usage_usec` delta summed across the two web replicas. The old replicas were warm. The
first candidate pass began shortly after the rollout; the second retained candidate
pass records the subsequently warm service. Values are milliseconds except where
labelled.

| Metric | Before | After 1 | After 2 |
| --- | ---: | ---: | ---: |
| Scheduled/accepted requests | 30/30 | 30/30 | 30/30 |
| Average | 9.176 | 11.513 | 14.269 |
| p50 | 6.146 | 8.594 | 9.167 |
| p90 | 9.964 | 13.502 | 21.851 |
| p95 | 11.352 | 21.118 | 29.599 |
| p99 | 61.400 | 61.897 | 75.075 |
| Max | 81.803 | 78.079 | 91.981 |
| Two-web CPU time | 1.275 s | 1.135 s | 2.019 s |
| Two-web aggregate CPU | 4.251% | 3.782% | 6.730% |

This is deliberately not presented as a latency or CPU improvement. At one request per
second the endpoint contributes too little work to separate from production traffic and
the active A&D/KotH scheduler; the two candidate CPU brackets moving in opposite
directions demonstrate that noise. The absolute p95 remained below 30 ms, but the
measured latency regression is retained rather than filtered out. No optimization-ledger
row is added because this production inventory is not an isolated fixture.

The deterministic work reduction is still material for a populated admin page. The old
SeaORM path issued a count and page query, then up to four ownership queries per row. The
live shared-KotH row took four statements and still returned both `team` and `challenge`
as null. The new raw-SQL projection uses one count plus one bounded page query regardless
of row count: four statements became two for the live row, and the upper-bound page shape
changed from `2 + 4N` statements to two. It also resolves per-team game instances, A&D
services, shared challenges, admin tests, exercises, and unassigned rows in that
projection.

The live correctness assertion for container
`c1944551-850c-4c24-b1bf-a4720debb6ae` changed from null ownership metadata to challenge
`the-hill`, `ownerKind: "Shared"`, `team: null`, and `isProxy: false`. The teamless result
is intentional: this KotH workload is shared by all teams. The UI now renders that scope
and does not offer a WSS-copy action for the non-proxy container. The public health path
returned exact body `ok` with HTTP 200, and all three application replicas were healthy
on the same image. Recent application logs contained no panic, migration failure, or
restart loop.

The first Compose invocation exposed pre-existing deployment-environment drift:
`POSTGRES_USER` was absent from the host env, so Compose selected its `rsctf` default
while the retained database uses `postgres`. The control health check failed closed on
authentication. The external PostgreSQL volume remained intact, the original role and
database settings were restored, and only the application services were restarted for
the accepted image rollout. No data migration failed. The database container metadata
was then reconciled to the real `postgres`/`rsctf` identity against the same external
volume. That deliberate database restart produced a roughly five-second window of 503
responses; the singleton control process exited fail-closed and restarted once, while
both web replicas stayed running. Six subsequent five-second public probes all returned
HTTP 200, the two web replicas remained at zero restarts, the control replica remained
stable at one restart, and no error, panic, migration-failure, or HTTP-5xx record appeared
after recovery. Production logs continue to report retained load-test workloads with
foreign installation scopes or removed legacy definitions; those warnings predate this
ownership-display change and are not counted as release regressions.

## Archived exhaustive admin lifecycle acceptance — 20 July 2026

This record is retained as historical performance and cleanup evidence. It predates the
current expanded admin/edit/organizer harnesses and does not claim acceptance of the
current tree; a fresh isolated run must replace it as the current execution record.

This is the first execution under the protocol above. Production at `tcp.1pc.tf` was not
used, restarted, migrated, or sent test traffic. Both measurements ran against the
marker-fenced `rsctf-final-719b` Compose project at `http://127.0.0.1:58080`, with two web
replicas, one singleton control replica, the same PostgreSQL/Redis/Caddy containers, and
the same disposable Docker and repository fixtures.

### Exact artifacts and topology

| Artifact | Exact identity |
| --- | --- |
| Source commit | `97e602d3285cacaa774570d69adbe019b3aac30d` (dirty measured tree) |
| Before application image | `rsctf-local:admin-lifecycle-20260720` / `sha256:b02801ce8a2af3f090d581aa1b5732312eaea76804725c5338f9aa85b16c9170` |
| Before image binary | `sha256:607262393890b75ce51dd1c131efc05cb7bea02803e06867c59410e48ac2043c` |
| After application image | `rsctf-local:admin-lifecycle-20260720-final` / `sha256:0eaa0bdf179d7c735eeb009f0de939db35d36d02fe7a81c6bb828e055375953f` |
| After image binary | `sha256:364eacc5e3271ed15d50cac79ee39cc83724fe010bfca10b3e6c74edd9ec1abf` |
| Frozen after build source | tracked `48889190e490a0085fe3a73f44d6d64a1d1daf3e37f4af296296ed23b7803a4b`; untracked `7eca72a98c83ab5be350483f9dd5eb72854b3158c41e8a23b555a045043f8a39` |
| Accepted harness worktree | observer `gitWorktreeSha256=a16c0caecdd29f2716cf6d849a5078aa437a57ae0923e871d4b043b4e3b65b9d` |
| PostgreSQL | `postgres:18.4-alpine3.24` / `sha256:bd1890816ae0b8ad4644f05728570d4be774e1f1490d7232f5084b52ea335183` |
| Redis | `redis:7-alpine` / `sha256:487efc0616382465781b8fdc3d6d1db449e6fd80ae23bf48432a2da6b6929908` |
| Caddy | `caddy:2-alpine` / `sha256:af555904a0961945f16bb323a501457b13a4f7e9bde969b145b97da80b38ecbe` |
| Host | Linux `6.8.0-124-generic`, Docker `28.4.0`, 8 logical CPUs, 31.34 GiB RAM |
| Isolation marker | `final-719b-admin-20260720` on both application roles, PostgreSQL, and Redis |
| Origins | public `127.0.0.1:58080`; web `172.30.253.131:8080`, `172.30.253.132:8080`; control `172.30.253.130:8080` |
| Before evidence | `/tmp/rsctf-admin-rate1-before-20260720.json`; manifest `admmrsvia2j`; load `06:59:41.260–07:04:45.447 UTC` |
| Accepted after evidence | `/tmp/rsctf-admin-rate1-after-20260720.json`; manifest `admmrswu5id`; load `07:36:34.597–07:41:38.318 UTC` |

The after image was built with the repository Dockerfile: the React production
type-check/bundle and Rust `cargo build --release --locked` both passed. A bounded
SignalR-probe transport retry and this report were changed after the image freeze; they
are load-harness/documentation changes and are not copied into the server image. The
accepted observer fingerprint identifies the worktree at observer start. The current
executable admin-harness files still match their recorded per-file hashes; this report
and its README were updated afterward. The older image's source fingerprint was not
retained, so its exact image and binary digests—not an invented source digest—identify
the baseline.

### Functional and security result

The accepted candidate run completed all **63/63** catalogued HTTP and realtime
surfaces. All 60 applicable operations rejected missing and ordinary-user credentials;
all 59 Admin-only operations rejected Monitor credentials. Same-game manager access
succeeded while unrelated/cross-game manager access failed, enrollment rejected invalid
and replayed tokens, and anonymous/User/Monitor SignalR clients were rejected before an
Admin JSON handshake completed.

The finite 74-operation/origin matrix passed exactly. The whole k6 execution completed
1,054/1,054 checks and 2,783 HTTP requests with zero 5xx, 429, unexpected statuses,
invalid response bodies, health failures, SignalR failures, dropped iterations, or
failed HTTP requests. The fixed-rate scenarios contributed 2,709 requests and 903
checks; setup contributed 74 requests and 148 checks, and SignalR contributed three
checks/sessions. Each of the three independent observers recorded 195/195 public
liveness, direct liveness, and direct readiness successes with no collector error. A
separate post-run Docker inspection reported zero restarts and zero OOM kills for every
application/support container; the three server logs contained zero panic/fatal records
from the run window.

Cleanup produced two delayed, identical all-zero snapshots. In particular, the old
image left one repository checkout and two credential-cache keys after its otherwise
complete load phase; the candidate left zero of both, along with zero games, users,
teams, participations, workers/workloads, bindings, build records, containers,
submissions, anti-cheat evidence, blobs, and checker directories. The old residuals were
then removed by their exact manifest identities. The candidate also returned HTTP 401
for anonymous admin SignalR negotiate. The old post-load authorization probe did not
produce a trustworthy status because its Node connection failed, so no old HTTP status
is inferred from that transport error.

Static verification for the measured tree also passed: `npm test` 217/217; `cargo fmt
--all -- --check`; `cargo check --locked --all-targets`; `cargo clippy --locked
--all-targets -- -D warnings`; and the full `cargo test --locked` run (626 library tests
plus the main/checker/logic/routes/storage integration suites, with only declared ignored
tests). The release Docker build completed with zero Rust warnings and performed the
frontend production build.

The first candidate measurement completed the full 300-second k6 phase and zero-resource
cleanup, but the outer Node gate then reused one stale proxy keep-alive socket. The
harness now permits one retry only for the idempotent rejected-negotiate request and
still requires the exact 401/403 response. The accepted rerun recorded one such retry;
k6 and all observers recorded zero network/health failures. This is retained as harness
evidence rather than silently discarded.

### Same-shape fixed-rate before and after

Both runs used `RATE=1 HEALTH_RATE=1 VUS=4 MAX_VUS=4 DURATION=300s`. The scheduled
admin-read and platform-health scenarios each ran at one iteration/s; an iteration makes
multiple requests. Including the finite setup matrix, each k6 execution made the same
2,783 requests at an aggregate rate of about 9.23 requests/s. Values are milliseconds.
The before k6 phase completed cleanly, but its manifest remained `completed=false` after
the later authorization/cleanup failures, so it is a performance baseline—not an
accepted functional run.

| Metric | Before | After | Change |
| --- | ---: | ---: | ---: |
| Aggregate k6 HTTP rate | 9.228 req/s | 9.235 req/s | +0.1% |
| Checks | 1,054/1,054 | 1,054/1,054 | unchanged |
| Overall HTTP avg | 7.579 | 4.598 | -39.3% |
| Overall HTTP p50 | 2.983 | 1.608 | -46.1% |
| Overall HTTP p90 | 12.976 | 7.989 | -38.4% |
| Overall HTTP p95 | 19.915 | 13.601 | -31.7% |
| Overall HTTP p99 | 91.896 | 48.861 | -46.8% |
| Overall HTTP max | 1,333.743 | 691.305 | -48.2% |
| Admin-read avg | 31.169 | 20.022 | -35.8% |
| Admin-read p50 | 13.349 | 7.656 | -42.7% |
| Admin-read p90 | 52.589 | 40.875 | -22.3% |
| Admin-read p95 | 139.635 | 65.373 | -53.2% |
| Admin-read p99 | 223.955 | 171.824 | -23.3% |
| Admin-read max | 1,333.743 | 691.305 | -48.2% |
| 74-pair matrix p95 | 59.846 | 51.364 | -14.2% |
| Health p95 | 17.891 | 13.617 | -23.9% |
| SignalR handshake p95 | 5.000 | 7.500 | +50.0% (three samples) |

Individual endpoint trends receive only about 12–13 steady samples in 300 seconds, so
their tail percentiles are diagnostics rather than standalone capacity claims. The
aggregate admin-read trend is the reliable comparison. Regressions are included below;
the table is not filtered to favorable endpoints.

| Metric | Before p50/p90/p95/p99/max | After p50/p90/p95/p99/max | p95 change |
| --- | ---: | ---: | ---: |
| `ad_admin_rounds_get_ms` | 13.99 / 27.52 / 116.10 / 202.38 / 223.95 | 7.07 / 12.53 / 17.76 / 22.80 / 24.06 | -84.7% |
| `ad_admin_services_get_ms` | 13.82 / 21.62 / 30.33 / 38.77 / 40.88 | 7.28 / 11.93 / 15.80 / 19.56 / 20.50 | -47.9% |
| `admin_anticheat_blocks_get_ms` | 8.37 / 19.14 / 23.79 / 27.95 / 28.99 | 5.36 / 8.85 / 19.44 / 29.61 / 32.15 | -18.3% |
| `admin_build_images_get_ms` | 62.16 / 107.67 / 660.11 / 1,199.02 / 1,333.74 | 47.84 / 67.27 / 103.45 / 138.62 / 147.41 | -84.3% |
| `admin_builds_get_ms` | 9.65 / 21.80 / 36.92 / 50.73 / 54.18 | 4.69 / 7.88 / 8.50 / 9.11 / 9.26 | -77.0% |
| `admin_builds_inprogress_get_ms` | 11.25 / 15.56 / 16.53 / 17.29 / 17.47 | 5.59 / 10.95 / 19.37 / 27.43 / 29.45 | +17.2% |
| `admin_cheat_reports_get_ms` | 179.85 / 237.30 / 254.69 / 269.52 / 273.22 | 136.48 / 224.97 / 438.06 / 640.66 / 691.31 | +72.0% |
| `admin_config_get_ms` | 8.88 / 14.72 / 17.35 / 19.86 / 20.49 | 6.71 / 11.34 / 14.05 / 16.54 / 17.16 | -19.0% |
| `admin_dashboard_get_ms` | 23.36 / 35.33 / 38.72 / 41.58 / 42.30 | 19.34 / 34.23 / 41.35 / 48.03 / 49.71 | +6.8% |
| `admin_files_get_ms` | 10.15 / 16.18 / 16.51 / 16.75 / 16.81 | 6.76 / 9.03 / 10.06 / 11.05 / 11.29 | -39.1% |
| `admin_flag_egress_get_ms` | 14.37 / 22.55 / 26.13 / 29.05 / 29.79 | 9.43 / 13.31 / 18.33 / 23.12 / 24.32 | -29.8% |
| `admin_game_writeups_get_ms` | 14.33 / 29.10 / 38.74 / 47.94 / 50.24 | 8.17 / 10.65 / 10.83 / 10.95 / 10.99 | -72.0% |
| `admin_instance_stats_get_ms` | 17.74 / 34.81 / 66.47 / 95.93 / 103.29 | 10.98 / 26.10 / 37.66 / 48.10 / 50.71 | -43.3% |
| `admin_instances_get_ms` | 12.31 / 23.91 / 32.33 / 40.30 / 42.29 | 7.04 / 12.15 / 13.31 / 14.06 / 14.25 | -58.8% |
| `admin_logs_get_ms` | 9.88 / 14.25 / 16.46 / 18.57 / 19.10 | 6.52 / 10.67 / 11.43 / 12.08 / 12.25 | -30.6% |
| `admin_my_ip_get_ms` | 5.33 / 8.70 / 75.36 / 155.35 / 175.34 | 4.23 / 22.15 / 27.10 / 30.39 / 31.21 | -64.0% |
| `admin_repo_binding_scans_get_ms` | 10.82 / 19.02 / 88.09 / 155.42 / 172.25 | 6.50 / 9.68 / 28.14 / 45.97 / 50.42 | -68.1% |
| `admin_repo_bindings_get_ms` | 10.86 / 20.10 / 60.66 / 99.91 / 109.72 | 7.58 / 16.20 / 22.54 / 28.23 / 29.65 | -62.8% |
| `admin_reviews_get_ms` | 11.73 / 16.50 / 18.11 / 19.55 / 19.92 | 7.79 / 14.08 / 23.46 / 32.48 / 34.73 | +29.5% |
| `admin_submission_trend_get_ms` | 45.59 / 96.34 / 103.71 / 106.20 / 106.82 | 31.07 / 57.64 / 109.30 / 159.32 / 171.82 | +5.4% |
| `admin_teams_get_ms` | 15.02 / 24.07 / 25.35 / 26.38 / 26.63 | 9.47 / 16.55 / 36.24 / 55.15 / 59.88 | +42.9% |
| `admin_user_get_ms` | 11.65 / 18.48 / 31.74 / 44.26 / 47.39 | 6.57 / 10.87 / 12.15 / 13.12 / 13.36 | -61.7% |
| `admin_users_get_ms` | 12.43 / 16.46 / 23.97 / 31.27 / 33.09 | 8.04 / 12.94 / 13.28 / 13.58 / 13.66 | -44.6% |
| `admin_workers_get_ms` | 12.65 / 36.45 / 45.22 / 52.74 / 54.61 | 7.45 / 8.94 / 15.91 / 22.66 / 24.34 | -64.8% |
| `admin_writeups_get_ms` | 12.20 / 20.35 / 21.55 / 22.60 / 22.86 | 7.13 / 9.39 / 22.08 / 34.49 / 37.59 | +2.5% |

### CPU and memory at the held rate

Docker CPU percentages use one logical core as 100%. The application total below is the
sum of the three process means; per-process p95 values are not added because their sample
timestamps are independent. Support-service values come only from the web-1 observer so
the same PostgreSQL/Redis/Caddy container is not counted three times.

| Process | Before CPU avg/p95/max | After CPU avg/p95/max | Before RAM avg/max | After RAM avg/max |
| --- | ---: | ---: | ---: | ---: |
| Web replica 1 | 1.708 / 11.750 / 21.790% | 1.877 / 10.320 / 15.750% | 59.844 / 71.940 MiB | 71.812 / 81.390 MiB |
| Web replica 2 | 2.466 / 12.300 / 86.590% | 1.918 / 10.270 / 17.640% | 35.304 / 41.700 MiB | 41.456 / 49.740 MiB |
| Control replica | 2.114 / 11.510 / 21.720% | 2.635 / 12.140 / 20.130% | 42.948 / 47.960 MiB | 33.452 / 35.340 MiB |
| Application total | 6.288% mean | 6.430% mean (+2.3%) | 138.096 MiB mean / 161.600 MiB summed peaks | 146.720 MiB mean / 166.470 MiB summed peaks |
| PostgreSQL | 13.179 / 25.220 / 120.160% | 4.627 / 16.470 / 55.360% | 166.130 / 189.200 MiB | 184.172 / 201.800 MiB |
| Redis | 5.968 / 10.220 / 63.560% | 1.733 / 4.240 / 31.840% | 3.790 / 4.035 MiB | 4.318 / 4.570 MiB |
| Caddy | 0.647 / 1.380 / 3.980% | 0.358 / 0.640 / 4.980% | 28.708 / 33.000 MiB | 40.965 / 44.970 MiB |

Application CPU is effectively flat at this low held rate; this is not a CPU optimization
claim. Application mean RAM increased 8.624 MiB (+6.2%) and the sum of process peaks
increased 4.870 MiB (+3.0%). PostgreSQL and proxy processes were not restarted between
runs, while application processes were recreated for the candidate; their RAM levels are
therefore descriptive and include process-age/cache effects. The lower database/Redis CPU
and improved aggregate latency are directionally consistent with less work, but only an
isolated optimization experiment should attribute those changes to one code path.

Thirty-second aligned series follow. Each cell is mean CPU / mean RAM for that bucket;
the final 300–304/305 second buckets are partial tails and are not weighted as full
intervals.

| Before offset s | Web 1 | Web 2 | Control | PostgreSQL | Redis | Caddy |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 0–30 | 5.65% / 61.5 MiB | 8.08% / 35.8 MiB | 2.56% / 42.6 MiB | 25.17% / 184.1 MiB | 8.11% / 3.8 MiB | 0.79% / 30.7 MiB |
| 30–60 | 2.77% / 59.4 MiB | 3.42% / 35.6 MiB | 0.79% / 42.7 MiB | 13.89% / 154.6 MiB | 6.12% / 3.8 MiB | 0.57% / 31.6 MiB |
| 60–90 | 1.38% / 59.5 MiB | 2.03% / 36.6 MiB | 0.84% / 42.8 MiB | 13.50% / 143.3 MiB | 6.33% / 3.8 MiB | 0.56% / 31.2 MiB |
| 90–120 | 1.37% / 59.6 MiB | 0.28% / 34.3 MiB | 1.01% / 42.9 MiB | 11.28% / 156.4 MiB | 5.31% / 3.8 MiB | 0.79% / 30.9 MiB |
| 120–150 | 0.26% / 59.0 MiB | 1.26% / 35.3 MiB | 2.60% / 43.2 MiB | 12.79% / 163.2 MiB | 5.43% / 3.8 MiB | 0.61% / 29.1 MiB |
| 150–180 | 0.39% / 59.1 MiB | 1.72% / 34.5 MiB | 4.06% / 43.0 MiB | 10.51% / 166.5 MiB | 5.09% / 3.8 MiB | 0.78% / 27.5 MiB |
| 180–210 | 2.57% / 59.8 MiB | 2.69% / 34.0 MiB | 2.20% / 43.0 MiB | 11.81% / 169.7 MiB | 5.44% / 3.8 MiB | 0.48% / 26.6 MiB |
| 210–240 | 1.74% / 60.7 MiB | 4.21% / 35.9 MiB | 1.92% / 43.1 MiB | 11.83% / 172.3 MiB | 6.70% / 3.8 MiB | 0.76% / 26.6 MiB |
| 240–270 | 0.62% / 60.7 MiB | 1.71% / 34.6 MiB | 1.46% / 43.1 MiB | 12.27% / 174.8 MiB | 6.50% / 3.8 MiB | 0.62% / 26.6 MiB |
| 270–300 | 0.49% / 59.2 MiB | 0.51% / 36.2 MiB | 4.08% / 43.1 MiB | 9.83% / 176.4 MiB | 5.17% / 3.8 MiB | 0.51% / 26.8 MiB |
| 300–305 | 3.48% / 60.6 MiB | 0.55% / 36.5 MiB | 0.35% / 43.1 MiB | 15.77% / 177.6 MiB | 4.14% / 3.8 MiB | 0.62% / 26.4 MiB |

| After offset s | Web 1 | Web 2 | Control | PostgreSQL | Redis | Caddy |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 0–30 | 3.63% / 71.6 MiB | 2.28% / 40.7 MiB | 4.27% / 33.1 MiB | 9.61% / 173.4 MiB | 1.93% / 4.3 MiB | 0.66% / 42.1 MiB |
| 30–60 | 2.15% / 71.5 MiB | 2.42% / 40.2 MiB | 2.93% / 33.2 MiB | 2.67% / 175.0 MiB | 1.26% / 4.3 MiB | 0.24% / 42.6 MiB |
| 60–90 | 2.02% / 72.3 MiB | 1.74% / 39.9 MiB | 2.75% / 33.2 MiB | 3.02% / 176.9 MiB | 1.01% / 4.3 MiB | 0.25% / 44.2 MiB |
| 90–120 | 1.08% / 71.4 MiB | 2.48% / 39.9 MiB | 3.38% / 33.2 MiB | 3.25% / 180.5 MiB | 1.30% / 4.3 MiB | 0.26% / 44.9 MiB |
| 120–150 | 0.21% / 70.0 MiB | 2.30% / 40.8 MiB | 2.23% / 33.2 MiB | 5.26% / 182.3 MiB | 2.53% / 4.3 MiB | 0.38% / 39.3 MiB |
| 150–180 | 3.01% / 70.3 MiB | 1.71% / 41.3 MiB | 1.63% / 33.2 MiB | 3.85% / 183.9 MiB | 1.28% / 4.3 MiB | 0.39% / 39.3 MiB |
| 180–210 | 2.35% / 72.5 MiB | 2.04% / 41.3 MiB | 1.55% / 33.2 MiB | 5.74% / 185.3 MiB | 1.89% / 4.3 MiB | 0.24% / 39.3 MiB |
| 210–240 | 0.33% / 72.1 MiB | 2.64% / 43.2 MiB | 4.11% / 33.2 MiB | 2.79% / 189.7 MiB | 1.39% / 4.3 MiB | 0.41% / 39.5 MiB |
| 240–270 | 1.05% / 72.8 MiB | 0.90% / 43.2 MiB | 2.21% / 33.5 MiB | 6.30% / 195.3 MiB | 3.37% / 4.3 MiB | 0.34% / 39.4 MiB |
| 270–300 | 2.39% / 73.5 MiB | 0.42% / 43.4 MiB | 1.53% / 35.3 MiB | 2.74% / 197.9 MiB | 1.46% / 4.3 MiB | 0.27% / 39.2 MiB |
| 300–304 | 4.75% / 72.8 MiB | 3.37% / 46.4 MiB | 0.88% / 35.3 MiB | 10.28% / 200.2 MiB | 0.71% / 4.3 MiB | 1.21% / 39.3 MiB |

### One-shot orchestration cost and remaining limits

The accepted run's one-shot route median was 277.83 ms, p95 1,254.14 ms, and maximum
5,405.67 ms. The slowest operations were build re-enqueue (5,405.67 ms), game bulk
rebuild (2,389.53 ms), repository scan (1,254.14 ms), cheat-report read (894.71 ms),
and container deletion (757.52 ms). These execute real Docker/git work and are outside
the steady read window.

Bulk rebuild is intentionally truthful but currently synchronous. It is safe under a
per-game session advisory lease and leaves unstarted records retryable on cancellation,
but a large game can exceed an HTTP/proxy timeout. The scalable follow-up is a durable
database-backed job claimed by the singleton control worker; an in-memory background task
would break cancellation and multi-replica ownership guarantees.

The source-checked `/api/edit` catalog covers all 64 registered method/path operations and
all response contracts in unit tests, but it is not yet a live exhaustive organizer
runner. The whole-platform lifecycle exercises many of those routes, not all 64. Archive
image label stamping is unit-tested; this admin lifecycle's positive image flow uses a
real pull/rebuild plus real image/container mutations. SMTP-disabled delivery and the
negative diagnostic path are live-covered; successful SMTP delivery requires a
capture-only sink. These boundaries are explicit so “all admin endpoints” is not
misreported as “all organizer endpoints.”

No optimization-ledger row is added for this mixed correctness/security change set.
Aggregate latency improved, but application CPU was flat and individual endpoint tails
are sparse; a ledger entry should follow only an isolated optimization experiment.

## Repository-sync and event-integrity acceptance — 19 July 2026

This is the current incident-focused acceptance record. Production inspection
was read-only; fixes, migrations, destructive-path regressions, replica churn,
Redis failure, and load were exercised only in the disposable
`rsctf-final-719b-*` environment. No live service was restarted, migrated, or
used as a stress-test target.

### Read-only production diagnosis

The `/admin/teams` 502 was a deterministic decode failure, not a proxy-capacity
failure. The full user model decoded PostgreSQL `+infinity` from `lockout_end`
through Chrono, which panicked. At inspection time 31 of 74 live user rows had a
non-finite value. The repaired list endpoint projects only the roster fields it
needs, and migration `m0073_finite_lockout_end` converts positive infinity to
the finite application sentinel, converts negative infinity to `NULL`, and
adds a validated finite-value constraint.

Repository sync used to delete and recreate matched challenges. Those deletes
cascaded through submissions and first-solve evidence, so a manifest rescan
could reset solves even when the challenge was conceptually unchanged. The
live repository-bound game inspected after the incident, game 67, had no
remaining accepted submission or `FirstSolves` row from which prior state could
be reconstructed. The repair prevents another destructive rescan, but it
cannot recover evidence already deleted; restoration of historical solves
requires a database backup or another authoritative record.

The new sync path keys a challenge by stable binding-relative manifest
identity, updates mutable fields in place, rejects ambiguous legacy adoption
and type changes, and does not use a matching title as identity. Missing active
definitions are rejected when removal would destroy event evidence; safe
historical removals retain their identity and evidence. Repository checkout,
event-definition mutation, push-on-edit, binding update, and binding deletion
are serialized and re-read under their locks so an older checkout cannot win a
race with a newer remote head. Incomplete processing of the same commit is
retryable instead of being silently treated as complete.

Related destructive paths now fence challenge, game, team, roster,
participation, attachment, runtime, A&D, and KotH evidence. In particular, a
submission that commits concurrently with team deletion is visible before the
delete decision; scored participation can only make the reversible
`Accepted`/`Suspended` transition; a suspended scored team cannot bypass the
roster freeze; and challenge/game deletion cannot erase retained submissions,
first solves, rounds, captures, runtime operations, or referenced blobs. These
are event-integrity controls, not scoring-version changes: A&D and KotH scoring
constants and wire behavior remain unchanged.

### Isolated admin regression

The same isolated request changed from a failing response to a successful
roster response. Because the before request returned 502 and the after request
returned 200, this is a correctness comparison, not a latency-optimization
claim.

| Variant | Status | Wall latency |
| --- | ---: | ---: |
| Before | 502 | 3,143.541 ms |
| After | 200 | 54.955 ms |

The regression fixture includes both PostgreSQL positive and negative infinity.
Repository-sync tests retain challenge IDs, solves, first-solve rows, counters,
attachments, and runtime state across mutable rescans; they also exercise
ambiguous identity, concurrent update/delete, same-SHA retry, newer-HEAD
push-on-edit, unsafe removal, and rollback behavior.

### Caddy replica churn at a held arrival rate

The first 2 → 4 → 2 replica rehearsal exposed a proxy-side scale-down defect.
Caddy's dynamic A upstream cache retained removed Docker addresses long enough
to route 800 requests to retired replicas. The proxy now refreshes Docker DNS
every second, uses a 500 ms dial timeout, and retries connection failures for up
to three seconds. The before and after runs used the same `RATE=20 VUS=128`
90-second schedule and server image.

| Metric | Before | After |
| --- | ---: | ---: |
| Scheduled iteration target | 20/s | 20/s |
| Completed / dropped iterations | 1,767 / 33 | 1,800 / 0 |
| HTTP requests / achieved rate | 11,247 / 107.968153/s | 11,304 / 119.392465/s |
| Server 5xx | 800 (7.113008%) | 0 |
| HTTP p50 / p95 / p99 / max | 3.906 / 3,001.818 / 3,003.403 / 3,253.312 ms | 3.072 / 7.521 / 14.969 / 609.602 ms |
| A&D submit p95 | 3,001.346 ms | 15.862 ms |

This held-rate comparison removes the retired-address failure mode: 5xx fell
800 → 0, dropped iterations fell 33 → 0, and overall p95 fell 3,001.818 →
7.521 ms (−99.75%). It is not a clean player-success comparison. Only 20 player
identities drove the 20-iteration/s diagnostic, exceeding their intentional
150-request/60-second authenticated quota; the remaining approximately 47%
failed/semantic metrics after the fix are expected HTTP 429 responses, not
proxy errors or corrupt boards.

A quota-safe `RATE=5` confirmation completed the same 2 → 4 → 2 shape with 450
iterations and 2,960 requests at 31.584639 requests/s. It recorded zero 5xx,
failed requests, semantic errors, invalid boards, and dropped iterations;
overall p95 was 13.470 ms, board p95 12.110 ms, and submit p95 21.325 ms. The
surviving replicas had zero restarts and were not OOM-killed. Two direct
`/healthz` probes observed the deliberately draining replicas' correct 503
`shutting down` state; `/livez`, the proxy, and all player traffic remained
available. A temporary wrapper that demanded readiness from containers being
removed therefore exited nonzero, but the measured scale transition itself was
clean.

### Redis-outage before and after

The disposable outage harness schedules one malformed registration request per
second while Redis is stopped. HTTP 400 is the expected application result;
`/livez` must stay 200, `/healthz` must expose the dependency outage, and
readiness must recover after Redis restarts without restarting rsctf.

| Metric | Before | After |
| --- | ---: | ---: |
| Scheduled request rate / duration | 1/s / 15 s | 1/s / 15 s |
| Expected HTTP 400 responses | 15/15 | 16/16 |
| Dropped iterations | not recorded by the old scenario | 0 |
| Average | 13,809.782 ms | 180.626 ms |
| p50 / p90 | 14,942.941 / 17,543.204 ms | 205.079 / 206.980 ms |
| p95 / p99 / max | 18,084.879 / 18,349.387 / 18,415.514 ms | 207.823 / 209.313 / 209.685 ms |

Outage p95 fell 18,084.879 → 207.823 ms (−98.85%). The old run drained queued
work after the nominal window and achieved only 0.509901 requests/s, so the
table compares latency at the same scheduler setting rather than claiming a
throughput result. The repaired run had no restart or OOM, and both health
endpoints returned 200 again after Redis recovered. A direct single-replica
check with fresh source identities returned 20 bounded local-fallback HTTP 400
responses followed by HTTP 429, confirming that dependency failure does not
remove admission control. The k6 gate now enforces `http_req_duration`
`p(95)<1000` in addition to exact status, zero failures, zero unexpected status,
and zero dropped iterations. CPU was not bracketed for either outage run, so no
CPU reduction is claimed.

### Clean steady-state snapshot

After the failure drills, a quota-safe `RATE=5 VUS=128 DURATION=60s` run served
2,017 requests in 301 iterations at 31.358884 requests/s. It had zero HTTP
failures, 5xx, semantic errors, invalid boards, and dropped iterations; all 63
health probes passed. Overall HTTP p50/p90/p95/p99/max was
4.884/9.174/11.393/28.880/92.260 ms. Board p95 was 11.025 ms, A&D epoch-board
p95 9.010 ms, and submit p95 12.629 ms.

Cgroup deltas over the fixed window were 2.501963 CPU-seconds for web replica
one, 2.448783 for web replica two, and 1.755374 for control: 6.706120 app
CPU-seconds. PostgreSQL used 4.838365, Redis 1.864884, and Caddy 2.733272, for
16.142641 CPU-seconds across the measured stack. This is a clean operational
snapshot, not a before/after CPU optimization row.

The complete JavaScript load-harness regression suite passed **190/190** after
adding the Caddy scale-churn, Redis-outage latency, and lifecycle deadline
guards. The focused run of those three files passed 4/4 tests. These counts
cover harness logic; Rust, PostgreSQL integration, release-build, and browser
client verification are reported with the final source-tree acceptance.

### Final immutable two-replica lifecycle acceptance — 20 July 2026

The deployment candidate passed the comprehensive lifecycle gate in a fresh
PostgreSQL 18.4 environment after all 75 migrations. The topology was two web
replicas, one singleton control replica, Redis, and Caddy. The fixture contained
100 Jeopardy teams, 400 A&D/KotH teams, 80 real BYOC relay tunnels, eight static
challenges, one real container challenge, one A&D service, and one KotH hill.
The fixed load used `FLEET=80 VUS=400 JEO_VUS=220 AD_VUS=80 KOTH_VUS=40
CONTAINER_VUS=3 DURATION=300s`, no player think time, and the corrected
45-second event-end grace.

The accepted run completed **627,022 HTTP requests at 1,995.612 requests/s** and
208,716 iterations. There were zero unexpected non-2xx responses, zero server
5xx responses, and zero failed checks across 627,618 checks. The 179,875 HTTP
429 responses (28.687%) were expected quota enforcement and were excluded from
the unexpected-status gate. One browsing iteration was dropped because its
configured 40-VU cap was exhausted; no request or integrity assertion was
dropped. The in-scenario `/livez` and `/healthz` probes each passed 364/364.
Independent observers collected 97, 97, and 96 samples from the two web replicas
and control replica respectively; all 290 public, local, and readiness checks in
each category passed.

The heavy-path checks completed 36 container lifecycle operations, including
create and destroy, with a 4,271.904 ms p95. Attachment upload and download p95
was 1,154.822 ms. The run accepted 80 scoped KotH captures and performed 37
timed token writes, rejected the prior-cycle capability 1/1 after reset, crossed
five complete crown cycles, and confirmed two acquisitions. Duplicate rounds,
KotH rows, overlapping rounds, cadence violations, missing evidence, and cleanup
leaks were all zero. PostgreSQL recorded zero deadlocks; every rsctf process
finished healthy with zero restarts and no OOM kill.

| Endpoint trend | Average | p50 | p90 | p95 | p99 | Maximum |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| All HTTP | 167.216 ms | 131.032 ms | 309.387 ms | 400.359 ms | 768.335 ms | 5,753.686 ms |
| Asset upload/download | 318.792 ms | 194.394 ms | 698.737 ms | 1,154.822 ms | 1,954.840 ms | 2,589.448 ms |
| Jeopardy submit | 613.826 ms | 528.652 ms | 1,068.422 ms | 1,298.534 ms | 1,783.250 ms | 2,858.242 ms |
| A&D State | 201.184 ms | 175.387 ms | 363.294 ms | 439.604 ms | 635.458 ms | 1,120.362 ms |
| A&D Targets | 192.971 ms | 164.992 ms | 356.868 ms | 433.377 ms | 624.978 ms | 1,171.055 ms |
| Container lifecycle | 2,464.848 ms | 2,318.378 ms | 3,828.102 ms | 4,271.904 ms | 5,451.191 ms | 5,753.686 ms |
| A&D submit | 303.579 ms | 265.694 ms | 527.487 ms | 633.038 ms | 875.315 ms | 3,538.040 ms |
| KotH hills | 183.574 ms | 140.862 ms | 348.249 ms | 444.289 ms | 996.498 ms | 2,247.556 ms |
| Combined board | 147.802 ms | 112.449 ms | 276.627 ms | 352.143 ms | 760.879 ms | 2,740.535 ms |
| Jeopardy details | 239.541 ms | 175.749 ms | 483.845 ms | 664.980 ms | 1,096.123 ms | 2,539.487 ms |
| A&D epoch board | 185.845 ms | 155.787 ms | 332.156 ms | 406.765 ms | 666.387 ms | 1,801.248 ms |
| BYOC onboarding | 4,357.838 ms | 4,823.000 ms | 6,919.400 ms | 7,171.000 ms | 7,723.480 ms | 8,004.000 ms |

The fixed 02:14:08–02:19:08 UTC resource window contained 59 samples per
component on an eight-core host. Docker CPU percentages below use 100% for one
core. Aggregate rsctf CPU averaged 151.358%, or 1.51 cores, and aggregate rsctf
memory averaged 516.0 MiB.

| Component | CPU average | CPU p95 | CPU maximum | RAM average | RAM maximum |
| --- | ---: | ---: | ---: | ---: | ---: |
| Web replica 1 | 55.923% | 72.809% | 76.730% | 196.468 MiB | 229.400 MiB |
| Web replica 2 | 55.546% | 74.511% | 86.870% | 168.775 MiB | 222.200 MiB |
| Control | 39.889% | 213.304% | 234.650% | 150.759 MiB | 353.500 MiB |
| PostgreSQL | 120.035% | 153.810% | 221.570% | 651.614 MiB | 699.500 MiB |
| Redis | 21.117% | 25.562% | 28.930% | 6.789 MiB | 9.957 MiB |

PostgreSQL peaked at 65 connections: 22 active, 31 waiting, and eight waiting on
locks, with a longest transaction of 4.565 seconds and zero deadlocks. Redis
peaked at 5.035 MiB, retained 4,963 keys, and had zero evictions, rejected
connections, or error-reply deltas. At deliberate BYOC fleet teardown, each
observer logged one Docker-stats read error and two fleet-stats read errors as
the measured containers disappeared. The 59-sample fixed resource window and
all health series remained complete.

#### Failure-to-acceptance sequence

The first exact-shape diagnostic served 660,349 requests at 2,198.52 requests/s
but exposed 64 PostgreSQL deadlocks and 21 server 5xx responses: 17 application
deadlock responses and four proxy responses. Submit and suspicion processing had
opposite row-lock order. A participation-scoped transaction advisory lock now
serializes those paths before either takes its row locks; the accepted run
recorded zero deadlocks.

Two later strict runs each exposed one Caddy 502 after tens of thousands of
otherwise successful requests. Caddy reported `server closed idle connection`
for an ordinary mutation whose upstream connection had been reused; both rsctf
replicas were healthy and logged no corresponding application failure. Reducing
the pooled keepalive to 30 seconds did not eliminate it. The final proxy policy
keeps reads and explicitly stateful network routes pooled, sends ordinary
`POST`, `PUT`, `PATCH`, and `DELETE` requests through a dynamic Docker-DNS
transport with keepalive disabled, and does not blindly replay ambiguous
mutations. A focused two-minute POST storm then served 143,429 requests at
1,195.195 requests/s with zero proxy 5xx and an 8.75 ms p95. The accepted full
lifecycle had zero Caddy access-log 5xx entries and zero closed-idle errors.

An otherwise-clean intermediate invocation omitted `HOSTPORT`, so its wrapper
probed unused port 8080 and failed 469/469 wrapper checks while the independent
observers proved that the configured endpoint stayed healthy. Supplying the
actual isolated port produced the accepted run above; that invocation error is
not counted as an application failure.

This campaign validates correctness and deployment fitness. It does **not** add
an optimization-ledger row: there is no same-harness before/after CPU bracket
from which to make a causal performance claim.

### Acceptance boundary and exact provenance

| Artifact | Exact identity |
| --- | --- |
| Git base for the uncommitted repair tree | `a901ea29e11aaf29802565d906d33e00c9a94237` |
| Before application image | `rsctf-local:final-6634c42df829-fb9cda9cab17` / `sha256:157ed4ec1edb6fc31bcdab0f56e4dd051e817b0d614955fa561fb0866aa28122` |
| Before image binary | `sha256:7b84e145c17964f515baf167f9fdf82851e5f75f72e02de6e8bee47d2002b19b` |
| Accepted application image | `rsctf-local:deploy-20260720-1` / `sha256:9cdb86a9b98ee0febba3f512d6c50d680a9ba9769c1cdfea6f7ace50417643ee` |
| Accepted image binary | `sha256:cdc4e618eb35fc350af189b2698ccbda82a4e5f8efbaa1a143e85464b23a8f5b` |
| Accepted Caddy image | `caddy@sha256:5f5c8640aae01df9654968d946d8f1a56c497f1dd5c5cda4cf95ab7c14d58648` / image ID `sha256:af555904a0961945f16bb323a501457b13a4f7e9bde969b145b97da80b38ecbe` |

The finite-lockout migration upgrade was first exercised on the immediately
preceding near-final image, then repeated from a fresh database by the accepted
immutable image. That image contains the final Rust server and web client used
by the lifecycle run. Only the JavaScript harness, documentation, and the
mounted Caddy configuration changed after the application image was built; none
of those changes alter the server or client bytes identified above. The Caddy
digest and mounted mutation-transport policy are therefore recorded separately
as part of the exact tested deployment composition.

One 180-second lifecycle diagnostic reached the configured event end at the
same instant k6 exited: 16,052 successful operations, 195 4xx responses, zero
5xx, and valid semantic checks. It is not counted as a passing benchmark because
settlement had no post-traffic margin. k6 may use up to its default 30-second
graceful-stop interval, so the lifecycle default event-end grace is now 45
seconds. The accepted 300-second lifecycle above used that 45-second boundary
and supersedes the diagnostic as deployment evidence.

Trivy 0.72.0 scanned the exact accepted application image with its vulnerability
database updated at 2026-07-19 18:43:16 UTC. The fixable-only gate found **zero
fixable HIGH or CRITICAL vulnerabilities** and exited successfully. This is not
a claim of zero total findings: the image still reports 65 unfixed HIGH records
(27 unique CVEs) and 14 unfixed CRITICAL records (five unique CVEs), all in
Alpine packages for which the scanner reported no fixed version at scan time.

> Historical boundary: sections below preserve earlier campaigns and their
> exact artifacts. They are useful comparison evidence but are superseded as a
> description of the current repair tree.

## Historical singleton operational acceptance — 19 July 2026

The final server-affecting tree passed both a held-rate player comparison and
the comprehensive lifecycle emulator. The fixed-rate run used 100 distinct
player identities, one combined A&D/KotH game with 100 teams, one service and
one hill, `RATE=20 VUS=128 DURATION=60s`, and a uniformly distributed
three-to-five-second think time. The lifecycle run retained the same 100-team
roster, selected four real outbound BYOC tunnels and four isolated services for
checker evidence, and used `VUS=60 DURATION=180s PLAYER_THINK_SECONDS=5`.

The final release-mode image was built before the report and two load-harness
repairs below. Those later edits affect installer delivery, documentation, and
test evidence only; they do not change the Rust server or web client embedded in
the image.

| Artifact | Exact identity |
| --- | --- |
| Operational candidate image | `rsctf-local:final-accept` / `sha256:2bab0283f1d5e06754070aa4ebcc74e4f3a59553b2d8b1d0c395ddcd3dfc5a63` |
| Candidate binary | `sha256:b188177a6bad66948ca1dd11b931d9a826d9eaa4cecd29a977a054092cbd711b` |
| Final image | `rsctf-local:final-accept2` / `sha256:6b2b454446f7cd37733fd4de548dac1fe91208f9862522c5db3c2e42dfc1ffa8` |
| Final binary | `sha256:2dad01fcf274522dc52acfbf52d134c01c01ecd4122421022e94d25a858c1072` |
| Source base | commit `6d389181959b7c69c7502efb77bfcf90de8b8f86` |
| Final server-source freeze | relevant tracked diff `5c1f3d244df865de7c93786cdb5d96af21a3cc77e20897aea61eb19ea6b57855`; relevant untracked manifest `38d98f09e62a1c3e33f6d48d813634d6dc5337ec4252855e8417061d57317756` |

### Same-shape held-rate before and after

Both images completed essentially the same held traffic: the candidate served
7,569 requests at 116.933 requests/s and 1,201 iterations; the final image
served 7,552 at 117.057 requests/s and 1,200 iterations. Both had zero failed
HTTP requests, server 5xx responses, client error metrics, and invalid official
A&D boards. Values below are milliseconds.

| Endpoint | Variant | Average | p50 | p90 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| A&D epoch board | Candidate | 3.652 | 2.679 | 4.283 | 6.468 | 26.962 | 130.242 |
| A&D epoch board | Final | 2.617 | 2.265 | 3.419 | 4.233 | 6.956 | 54.484 |
| A&D State | Candidate | 5.144 | 3.710 | 7.862 | 11.385 | 30.161 | 85.661 |
| A&D State | Final | 3.544 | 3.179 | 5.433 | 6.492 | 10.184 | 13.524 |
| A&D Targets | Candidate | 4.722 | 2.854 | 7.042 | 11.398 | 31.771 | 107.907 |
| A&D Targets | Final | 4.158 | 3.438 | 6.031 | 7.391 | 11.889 | 89.765 |
| Combined board | Candidate | 3.658 | 2.699 | 4.408 | 6.972 | 26.955 | 130.242 |
| Combined board | Final | 2.758 | 2.302 | 3.587 | 4.411 | 10.700 | 54.484 |
| KotH board | Candidate | 3.725 | 2.662 | 4.438 | 7.214 | 30.036 | 78.333 |
| KotH board | Final | 2.904 | 2.277 | 3.631 | 4.550 | 23.356 | 50.305 |
| KotH State | Candidate | 2.755 | 2.030 | 4.636 | 6.912 | 14.427 | 78.872 |
| KotH State | Final | 1.826 | 1.562 | 3.282 | 4.995 | 8.882 | 20.106 |
| KotH timeline | Candidate | 3.153 | 1.591 | 5.337 | 9.003 | 37.875 | 58.624 |
| KotH timeline | Final | 1.734 | 0.902 | 1.915 | 2.727 | 28.779 | 36.025 |
| KotH token | Candidate | 4.944 | 3.938 | 6.736 | 8.794 | 29.610 | 119.557 |
| KotH token | Final | 3.703 | 3.367 | 5.434 | 6.696 | 9.510 | 19.694 |
| Main scoreboard | Candidate | 3.597 | 2.755 | 4.528 | 7.340 | 19.682 | 66.609 |
| Main scoreboard | Final | 2.755 | 2.352 | 3.650 | 4.538 | 10.401 | 50.468 |
| A&D submit | Candidate | 5.878 | 5.083 | 8.656 | 11.200 | 16.151 | 19.287 |
| A&D submit | Final | 4.511 | 3.954 | 6.492 | 7.328 | 9.417 | 21.449 |
| All HTTP | Candidate | 3.912 | 2.794 | 5.936 | 8.236 | 26.472 | 130.242 |
| All HTTP | Final | 2.886 | 2.421 | 4.576 | 5.768 | 10.581 | 89.765 |

Every reported p95 improved: overall HTTP **−29.97%**, combined board
**−36.73%**, A&D State **−42.98%**, Targets **−35.15%**, KotH board
**−36.93%**, timeline **−69.71%**, and submit **−34.57%**. The live Targets
security fence deliberately stopped caching mutable relay ports and checker
verdicts for five seconds. It added one bounded SQL overlay per poll; its p50
rose 2.854 → 3.438 ms, while average, p90, p95, p99, and max all improved. This
is the intended freshness tradeoff: a retired or reconnected BYOC endpoint is
never served from the five-second immutable roster cache. Submit p95 improved,
but its single worst observation rose 19.287 → 21.449 ms; neither tail is hidden
from the table.

Fifteen aligned four-second samples cover the final held-load window. CPU is a
percentage of one core; RAM is MiB.

| Component | Candidate CPU avg / median / p95 / max | Final CPU avg / median / p95 / max | Final RAM start / average / peak / end |
| --- | ---: | ---: | ---: |
| RSCTF | 13.53 / 11.17 / 25.62 / 29.86 | 14.02 / 10.83 / 25.25 / 40.38 | 46.50 / 60.66 / 65.23 / 62.55 |
| PostgreSQL | 12.98 / 11.52 / 29.21 / 33.45 | 12.00 / 8.58 / 28.80 / 35.25 | 117.40 / 144.89 / 155.50 / 155.50 |
| Redis | 5.31 / 4.76 / 9.99 / 13.52 | 3.09 / 2.48 / 5.91 / 8.35 | 7.14 / 7.65 / 7.77 / 7.73 |

The sum of sampled component CPU averages fell **31.82% → 29.10% of one
core**, an 8.55% reduction at the held request rate. RSCTF average CPU rose
3.61%, while PostgreSQL fell 7.57% and Redis fell 41.90%. The RSCTF maximum is
a single four-second scheduler/traffic overlap; its p95 was slightly lower than
the candidate. These are sampled operational bounds, not cumulative cgroup CPU.

| UTC | RSCTF CPU / MiB | PostgreSQL CPU / MiB | Redis CPU / MiB |
| --- | ---: | ---: | ---: |
| 13:12:17 | 8.07 / 46.50 | 0.54 / 117.40 | 0.43 / 7.14 |
| 13:12:21 | 13.73 / 53.12 | 35.25 / 130.30 | 4.85 / 7.55 |
| 13:12:25 | 18.76 / 58.50 | 25.80 / 139.20 | 2.76 / 7.74 |
| 13:12:29 | 9.30 / 59.12 | 8.03 / 140.40 | 2.48 / 7.77 |
| 13:12:33 | 8.30 / 61.29 | 5.70 / 143.90 | 2.75 / 7.77 |
| 13:12:37 | 16.70 / 62.02 | 10.02 / 145.50 | 2.40 / 7.61 |
| 13:12:41 | 16.30 / 62.89 | 13.09 / 145.80 | 4.86 / 7.66 |
| 13:12:45 | 10.83 / 62.66 | 6.97 / 146.50 | 2.06 / 7.66 |
| 13:12:49 | 16.56 / 62.77 | 8.58 / 147.80 | 2.51 / 7.64 |
| 13:12:53 | 8.80 / 62.27 | 6.92 / 149.10 | 2.01 / 7.69 |
| 13:12:57 | 18.15 / 63.18 | 12.11 / 151.30 | 4.62 / 7.66 |
| 13:13:01 | 40.38 / 63.35 | 26.04 / 152.60 | 8.35 / 7.66 |
| 13:13:05 | 8.74 / 64.43 | 5.73 / 153.70 | 2.13 / 7.71 |
| 13:13:09 | 7.57 / 65.23 | 4.36 / 154.40 | 1.99 / 7.72 |
| 13:13:13 | 8.08 / 62.55 | 10.83 / 155.50 | 2.08 / 7.73 |

### Comprehensive lifecycle acceptance

The final rerun served **15,900 requests at 86.340 requests/s**. It had zero
server 5xx responses, zero invalid successful A&D boards, and zero invalid
successful KotH lifecycle models. The script recorded 15 unexpected non-2xx
responses excluding 429 (0.239%) and 9,622 intentional 429 responses. The
direct driver is one source address and therefore exercises the configured
source ceilings; 429s are retained as failed HTTP responses rather than hidden.

| Operation | Average | p50 | p90 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| All HTTP | 5.453 | 0.880 | 14.037 | 25.007 | 61.755 | 402.985 |
| Combined board | 9.706 | 1.758 | 28.490 | 36.277 | 109.435 | 175.562 |
| A&D epoch board | 6.129 | 1.990 | 6.659 | 8.835 | 151.176 | 159.888 |
| A&D State | 9.447 | 7.288 | 14.271 | 26.430 | 44.216 | 61.537 |
| A&D Targets | 9.026 | 7.963 | 14.169 | 17.722 | 55.031 | 61.607 |
| Details / KotH State | 10.976 | 8.944 | 20.290 | 29.406 | 52.153 | 57.152 |
| KotH board | 7.170 | 1.824 | 26.335 | 28.707 | 30.196 | 30.510 |
| A&D submit | 24.944 | 12.599 | 55.575 | 62.479 | 79.888 | 93.075 |
| Jeopardy submit | 46.307 | 32.055 | 85.972 | 154.733 | 204.576 | 210.272 |
| Attachment download | 31.264 | 25.773 | 68.556 | 73.712 | 80.754 | 91.800 |
| Container operation | 388.831 | 388.831 | 400.154 | 401.570 | 402.702 | 402.985 |
| Onboarding | 58.400 | 45.000 | 82.600 | 82.800 | 82.960 | 83.000 |

The run established 4/4 real tunnels, independently delivered and verified
4/4 selected service flags, retained all 100 frozen roster services, created
two real Jeopardy containers, downloaded the seeded attachment, wrote 33 KotH
captures across three observed cycles, confirmed one stable acquisition, and
rejected the prior cycle's revoked capability 1/1. The lifecycle's one-second
sampler returned **319/319 liveness** and **319/319 readiness** probes. An
independent four-second observer returned 63/63 for public liveness, local
liveness, and readiness.

Every authoritative integrity query returned zero: duplicate or non-contiguous
rounds, duplicate attacks/tokens/cycles/control ticks/acquisitions/runtime
operations/participations, overlapping cycles, invalid reset receipts, stale
container evidence, cross-cycle token evidence, unbound controls, platform
voids, invalid cooldowns, holders outside the current cycle, late scorable
evidence, unfinished pipelines, delivery/publication failures, self-captures,
post-deadline attacks, probe failures, and fatal server logs. Publication lag
was p95 5.356 seconds and max 5.357 seconds, inside the 8/12-second gates.

The independent observer spans preparation, timed traffic, settlement, and
teardown. It retained 62 valid Docker samples per component; two fleet-inventory
samples raced the intentional relay teardown and did not affect health or
component telemetry.

| Component | CPU average / median / p95 / max | RAM average / p95 / max |
| --- | ---: | ---: |
| RSCTF | 8.75 / 3.56 / 21.99 / 68.82% | 76.48 / 78.83 / 92.27 MiB |
| PostgreSQL | 12.04 / 6.31 / 45.27 / 65.95% | 384.46 / 395.90 / 396.10 MiB |
| Redis | 1.83 / 1.10 / 5.01 / 5.97% | 7.73 / 8.03 / 9.03 MiB |

PostgreSQL averaged 32.70 connections and 1.03 active connections, with maxima
33/2. Waiting connections averaged 0.032 and peaked at one; waiting locks,
deadlocks, and temporary files remained zero. The longest sampled transaction
was 17.064 ms. Redis stayed at two keys with zero evictions, rejected
connections, or new error replies under its 256 MiB `allkeys-lru` bound.

### Saturation diagnostic and harness corrections

Before the representative run, the same image was deliberately driven with 40
VUs and no think time. It served 1,835,291 requests at **10,194.65 requests/s**
with zero 5xx responses and 61/61 public/local/readiness observer samples. The
limiter rejected 96.52% of requests, so the semantic metrics—which intentionally
classify any non-200 board response as invalid—crossed their zero thresholds.
This is rate-limit/backpressure evidence, not a successful-payload corruption
or a representative performance result.

That campaign exposed two load-harness defects, both now covered by regression
tests. `observe.mjs` used a hard-coded PostgreSQL role and now honors `PG_USER`
and `PG_DATABASE`. The lifecycle fatal-log gate scanned the unrelated
`rsctf-rsctf-1` container for its entire lifetime; it now fails closed while
scanning the configured `RSCTF_CONTAINER` only from the current run's start.
The unrelated container had nine historical panic/FATAL lines, while the
isolated accepted server had zero. The corrected full rerun produced the passing
result above.

This operational comparison does not add an optimization-ledger row. It bundles
several correctness and security closures and has one run per image, so it
cannot attribute the latency change to a single edit. It also uses one
single-binary server, local PostgreSQL/Redis, loopback traffic, one hill, and
four live tunnels from a 100-team roster. The held-rate player result is a valid
same-shape comparison; the no-think-time run is only an abuse diagnostic, and
the lifecycle result is a functional acceptance gate rather than a capacity
claim.

## Attack-Defense max-batch hardening and fixed-rate optimization — 19 July 2026

> Frozen-campaign scope: the measured after image predates the later A&D
> submit/deletion fence and the fresh-install repair to `m0071`. These numbers
> isolate the batch memoization and bounded eligibility-join changes; they are
> not current-tree end-to-end submit latency.

The measured after variant performs one adjudication for repeated flags in the
same request, uses one bounded authoritative lookup for a flag and its active
victim service, and charges the submit limiter for each distinct plausible
flag. The same frozen after image also passed a two-replica distributed-limiter
drill. At one maximum-size batch/s, repeated-known p95 fell from **694.49 ms to
38.74 ms** and whole-stack CPU time fell from **20.742 to 0.850 CPU-seconds**
(−95.90%). For 100 distinct known duplicates, p95 fell from **790.76 ms to
367.97 ms** and stack CPU fell from **21.644 to 10.811 CPU-seconds** (−50.05%).

The distinct-unknown control did regress: p95 was **68.92 → 73.35 ms** (+6.42%)
and stack CPU was **1.757 → 2.079 CPU-seconds** (+18.27%, an absolute increase
of 0.321 CPU-seconds over 30 seconds). The new joined eligibility lookup does
more work than the old index-only miss. This is the tradeoff for replacing the
old lookup plus later service/roster reads with one bounded authoritative
eligibility join. The join preserves the existing inactive-service rejection,
and the regression remains small in absolute terms.

### Controlled workload and provenance

Both variants used the same isolated PostgreSQL 18.4 and Redis fixture on an
8-vCPU Linux 6.8.0 host with Docker Engine 28.4.0/API 1.51. The fixture had game
65 in synthetic, unfinalized round 20, 500 authoritative active team services,
100 distinct current engine-shaped flags spread across 100 services, and an
attacker that had already captured every measured known flag. Its
accepted-attack count was 107 before and after every trial. No credential or
flag was written to an artifact or command output.

| Artifact | Exact identity |
| --- | --- |
| Before image | `rsctf-local:audit-before-6d38918` / `sha256:43c2ac05e510759395980a1cd60be5258497004f96b0afd1177cc9aa34886987` |
| Before binary | `sha256:2f9c627d0535177977ae4fa8f6b19fe73a6036f57f1cb4373ffd76e3d6d6d0e8` |
| After image | `rsctf-local:audit-after-final` / `sha256:869bd8cbf4274b1f2eae3b01489f6c1a4c113355732d1c23ef3fc344201a214b` |
| After binary | `sha256:b1ec3543b0ed928b6380d4adde0c76fc0245aa21be20223383782caa15fe2107` |
| Before source | commit `6d389181959b7c69c7502efb77bfcf90de8b8f86` |
| After source freeze | same HEAD plus tracked binary diff `fc4e7fa8334ff5575dbed3378df5a9836cc7e71a09768ef60885c03abef46a6c` and untracked manifest `87f10891425ac61cf7f12600df319ed0825057cde6d5502cf1efd891ff2f40a8` |

The after source fingerprint excludes this report and its README ledger entry,
which were written after the binary freeze and cannot affect the executable.
The frozen after image was built from the exact frozen tree in release mode; its
web build and Rust release build completed without warnings.

A post-measurement security review subsequently bound the admin import-password
cache to immutable account IDs, made delivery an atomic row-locked consume
across replicas, and added a per-account PostgreSQL session lease so a concurrent
admin update cannot unban an account during deletion teardown. Those admin-only
paths do not call the A&D submit handler or limiter and are therefore not
assigned a performance-ledger claim, but they are not part of the measured image
or source hashes above. A later deletion/submit fence does execute on the
measured handler and is also outside these hashes and timings. The final commit
is rebuilt and tested separately after those security fixes.

The measured `m0071` state also predates its later fresh-install default repair.
The baseline binary predates migration `m0071_team_deletion_fence` and refuses
to start when that migration is recorded as applied. The runner therefore
removed only that row from the isolated `seaql_migrations` table before each
baseline start and restored it before each after start. The schema column and
all fixture data remained unchanged. The migration row was restored after the
campaign. This compatibility shim changes startup bookkeeping, not either
measured query or its data.

Each trial restarted the selected web-only rsctf container cold, flushed the
dedicated Redis database after startup, and reused the same persistent database
and Redis containers. rsctf had 4 CPUs and 2 GiB RAM. The harness used k6
`constant-arrival-rate` with `RATE=1 VUS=4 MAX_VUS=4 DURATION=30s
REQUEST_TIMEOUT=10s`; every request carried the endpoint maximum of 100 flags.
CPU figures are bracketed deltas from cumulative container cgroup counters, not
instantaneous samples. The 30/31-iteration difference is k6's boundary timing;
both variants met the configured one-batch/s rate without a dropped iteration.
The trial order alternated before and after for each shape.

### Per-trial latency distributions

All values below are milliseconds. `results` counts individual flag results,
not HTTP requests.

| Variant | Shape | Trial | Iterations | Results | Average | p50 | p90 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Before | repeated known | 1 | 30 | 3,000 | 569.750 | 547.751 | 637.305 | 705.728 | 783.912 | 797.421 |
| After | repeated known | 1 | 30 | 3,000 | 26.452 | 17.718 | 30.773 | 40.949 | 212.422 | 280.232 |
| Before | repeated known | 2 | 30 | 3,000 | 611.268 | 604.551 | 658.621 | 683.255 | 788.820 | 827.885 |
| After | repeated known | 2 | 31 | 3,100 | 17.023 | 16.895 | 24.794 | 36.524 | 49.061 | 49.809 |
| Before | distinct known | 1 | 31 | 3,100 | 562.143 | 541.941 | 662.613 | 671.642 | 708.294 | 722.796 |
| After | distinct known | 1 | 30 | 3,000 | 300.334 | 285.025 | 328.880 | 378.813 | 531.612 | 581.829 |
| Before | distinct known | 2 | 31 | 3,100 | 632.289 | 597.825 | 746.671 | 909.875 | 928.168 | 929.316 |
| After | distinct known | 2 | 31 | 3,100 | 325.150 | 315.988 | 353.111 | 357.122 | 530.625 | 603.590 |
| Before | distinct unknown | 1 | 30 | 3,000 | 35.443 | 31.145 | 50.805 | 58.944 | 65.516 | 65.954 |
| After | distinct unknown | 1 | 30 | 3,000 | 48.369 | 42.434 | 56.991 | 62.749 | 124.299 | 147.551 |
| Before | distinct unknown | 2 | 31 | 3,100 | 47.193 | 42.325 | 61.684 | 78.903 | 110.390 | 116.692 |
| After | distinct unknown | 2 | 30 | 3,000 | 55.067 | 49.424 | 79.327 | 83.947 | 123.262 | 139.096 |

### Two-trial means

The following values are arithmetic means of the two trial-level statistics,
not a reconstructed percentile over combined samples.

| Shape | Variant | Average | p50 | p90 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Repeated known | Before | 590.509 | 576.151 | 647.963 | 694.492 | 786.366 | 812.653 |
| Repeated known | After | 21.737 | 17.306 | 27.784 | 38.736 | 130.742 | 165.020 |
| Repeated known | Change | **−96.32%** | **−97.00%** | −95.71% | **−94.42%** | −83.37% | −79.70% |
| Distinct known | Before | 597.216 | 569.883 | 704.642 | 790.758 | 818.231 | 826.056 |
| Distinct known | After | 312.742 | 300.507 | 340.996 | 367.968 | 531.119 | 592.710 |
| Distinct known | Change | **−47.63%** | **−47.27%** | −51.61% | **−53.47%** | −35.09% | −28.25% |
| Distinct unknown | Before | 41.318 | 36.735 | 56.245 | 68.923 | 87.953 | 91.323 |
| Distinct unknown | After | 51.718 | 45.929 | 68.159 | 73.348 | 123.780 | 143.323 |
| Distinct unknown | Change | **+25.17%** | **+25.03%** | +21.18% | **+6.42%** | +40.73% | +56.94% |

### CPU time at the held rate

Values are CPU-seconds consumed during each 30-second load window. `Stack` is
rsctf + PostgreSQL + Redis. The table reports the mean of two trials.

| Shape | Variant | rsctf | PostgreSQL | Redis | Stack |
| --- | --- | ---: | ---: | ---: | ---: |
| Repeated known | Before | 6.033 | 14.556 | 0.153 | 20.742 |
| Repeated known | After | 0.135 | 0.549 | 0.167 | 0.850 |
| Repeated known | Change | **−97.77%** | **−96.23%** | +9.00% | **−95.90%** |
| Distinct known | Before | 6.339 | 15.143 | 0.161 | 21.644 |
| Distinct known | After | 1.701 | 8.948 | 0.162 | 10.811 |
| Distinct known | Change | **−73.17%** | **−40.91%** | +0.57% | **−50.05%** |
| Distinct unknown | Before | 0.671 | 0.924 | 0.162 | 1.757 |
| Distinct unknown | After | 0.632 | 1.284 | 0.163 | 2.079 |
| Distinct unknown | Change | **−5.92%** | **+39.00%** | +0.33% | **+18.27%** |

The repeated path now performs one adjudication per request rather than 100
(−99% direct adjudication work). The distinct-known path cannot memoize across
different flags, but its authoritative joined lookup removes the separate
victim and two roster reads from each result. The control demonstrates the
tradeoff: the new path performs the bounded authoritative eligibility query
even for an unknown flag, while the old path stopped after its simple indexed
flag miss.

Normalizing for k6's 30/31 boundary iteration produces the same conclusion.
Repeated stack CPU was 0.6914 → 0.02788 CPU-seconds/batch (−95.97%), distinct
known was 0.6982 → 0.3544 (−49.23%), and distinct unknown was 0.05762 → 0.06928
(+20.24%). The joined lookup being responsible for the unknown-control increase
is an inference from the code path and measured PostgreSQL delta; individual
SQL statements were not instrumented in this campaign.

### Correctness and two-replica abuse drill

All 36,500 returned flag results matched their expected semantic status. The
repeated-known and distinct-known trials returned only `duplicate`; the
distinct-unknown trials returned only `wrong`. Across all 12 trials there were
zero semantic-invalid results, HTTP 429 responses, server 5xx responses,
unexpected HTTP statuses, failed HTTP requests, or dropped iterations. Accepted
attacks remained **107 → 107** in every trial.

The exact after image was then run as two web replicas with 2 CPUs each, sharing
the same Redis limiter. Thirty-two concurrent requests, each containing 100
distinct plausible unknown flags, were split across the replicas. All 32
requests returned HTTP 200; the immediate 33rd returned HTTP 429 with
`Retry-After: 10`; a request after 10.5 seconds returned HTTP 200. Attacks again
remained 107 → 107. This exercises the shared 3,200-token bucket and its
continuous 10-token/s refill across real replicas rather than assuming
in-process coordination. It proves atomic coordination with the deployed
standalone Redis; it does not prove Redis-outage fallback, Redis Cluster
behavior, long-duration fairness, or long-duration stability.

That 3,200-token result is historical evidence for the measured image above,
not the final production default. The reviewed configuration now permits 400
immediate distinct plausible flags (four maximum-size batches) and retains the
same 10 flags/second refill. Reproducing this 30-second distinct-batch campaign
therefore requires the documented 3,200-token override on an isolated test
process; production deployments should keep 400.

The campaign is deliberately a submit-path microbenchmark, not a whole-platform
replacement for `npm run player` or `node lifecycle.mjs`. It establishes the
cost and correctness of adversarial maximum-size batches. The final release
image must still pass the broader build, unit, integration, and lifecycle gates
before deployment.

## Trusted-worker functional-readiness and replica campaign — 18 July 2026

This campaign validates the new outbound trusted-worker plane with a real native
Linux agent and Docker runtime. Across two complete create, scale-up, scale-down,
and destroy cycles, all **2,405/2,405** fixed-rate proxy streams returned the
expected application marker. There were zero handshake failures, post-upgrade
stream failures, missing or invalid responses, HTTP 429 upgrades, server 5xx
responses, health failures, or invalid worker inventories.

This is the worker plane's **first valid fixed-rate performance baseline**, not
a same-shape before/after optimization comparison. The readiness diagnostic
below is a correctness result; it does not add an optimization-ledger row.

### Controlled workload and provenance

The accepted run used one outbound TLS 1.3 mTLS agent with 12 workload slots,
`FLEET=12 CYCLES=2 RATE=20 VUS=20 DURATION=20s`, and no post-Ready delay. Each
workload had two services: two `primary` replicas plus one `sidecar` replica at
the base shape, then three plus two during scale-up. The agent therefore ran
**36 → 60 → 36** worker-owned containers in each cycle. Every declared TCP port
on every replica had to accept a connection before the workload could report
Ready. Proxy responses also had to contain `Shared rsctf demo service`; the k6
client accepts text, binary, and marker-split WebSocket frames.

The isolated Compose project was `rsctf-worker-e2e-final5-0718`. Its retained
measurement window ran from `14:44:53.985` through `14:49:56.458 UTC`. The host
reported Linux 6.8.0-124-generic, Docker Engine 28.4.0/API 1.51, 8 logical CPUs,
and 31.34 GiB RAM. The agent was native Linux/amd64 and used Docker as its
workload runtime. Twelve distinct player identities held proxy admission to
1.667 requests/s/identity; the harness also ran liveness/readiness at one batch/s
and worker inventory at one poll/10 s. The resource artifact records
`outcome=passed`, valid metrics, and these exact artifacts:

| Artifact | Exact identity |
| --- | --- |
| rsctf image | `rsctf-worker-final-v4:current` / `sha256:26d64c832109f99bde8b61f69dcfc308edb21d86b0b9f3f4e1c655683234ef87` |
| rsctf binary in image | `sha256:3edc5abec53246e04bd0ea178d958957137bfb5b8ee60854e1a04e6344820484` |
| worker agent | `sha256:f7e9bf3837ebfce7bf62ba707024f162dbb4a8b2d84653a8078c518565f25213` |
| worker / fixture | `019f75af-e31d-7a72-9a4c-5cc6230e0a31` / `sha256:5d15dd0a8aa855265904a99133043a091202fb5a546ba49cdff633087df7abcf` |
| PostgreSQL 18.4 | `postgres:18.4-alpine3.24` / `sha256:bd1890816ae0b8ad4644f05728570d4be774e1f1490d7232f5084b52ea335183` |
| Redis 7 | `redis:7-alpine` / `sha256:487efc0616382465781b8fdc3d6d1db449e6fd80ae23bf48432a2da6b6929908` |
| measured source tree | tracked `391f6dd5fa857db75b97a64a2e369796961c7adf47b5edf834167843f9f6ed42`; untracked `8ea9eb3fb38270228f9d70200001a5cecca04a1e2974cdc1b8d6e35d2b27a80b` |

The source fingerprints were unchanged at the pre-build, post-build, and
pre-measurement gates. The tracked digest covers the exact `HEAD`, its binary
diff, and clean pinned recursive-submodule status; the untracked digest covers
the sorted file-hash manifest. The runner reused pinned image, fixture, and agent
artifacts, so these hashes identify exactly what ran but are not a cryptographic
build attestation from the source fingerprint. No compiler, image build, or
second load generator ran in the accepted measurement window.

Review after the frozen campaign added fail-closed proxy message bounds, bounded
agent connect/negotiation timeouts, a four-request definition-lock admission
gate, and late-rollout metadata recovery. Those changes affect admission,
failure, or admin rollout paths rather than the accepted steady proxy stream,
but they are not part of the fingerprints above. The tables therefore remain
acceptance evidence for the named measured snapshot, not a byte-for-byte
benchmark of the final commit; a future optimization claim must rerun the same
fixed-rate harness.

### Functional readiness: before and after

The readiness regression was isolated with the same harness shape and fixed
`FLEET=4 RATE=20 VUS=20 DURATION=10s` base phase. Both diagnostics used the same
rsctf image ID and fixture; the change was in the agent. Before the fix, a
workload became Ready when Docker said its containers were running, even if the
application listeners had not opened. After the fix, readiness probes every
declared TCP port on every replica and caches successful checks until runtime
state or a real proxy dial invalidates them.

| Metric | Before readiness gate | After readiness gate | Change |
| --- | ---: | ---: | ---: |
| WebSocket attempts | 201 | 201 | same fixed-rate load |
| Valid application replies | 137 (68.159%) | 201 (100%) | **+31.841 percentage points** |
| Missing replies | 64 (31.841%) | 0 | **−100%** |
| Upgrade/handshake failures | 64 (31.841%) | 0 | **−100%** |
| Post-upgrade stream / invalid-marker failures | not separately instrumented | 0 / 0 | explicit after-fix gates |
| Server 5xx / proxy 429 | 0 / not separately instrumented | 0 / 0 | clean |
| Proxy stream avg / p50 / p90 / p95 / p99 / max | 50.73 / 50 / 56 / 58 / 92 / 93 ms | 52.28 / 51 / 57 / 59 / 63 / 87 ms | see caveat below |
| Health avg / p50 / p90 / p95 / p99 / max | 1.556 / 1.344 / 2.211 / 2.771 / 3.218 / 3.330 ms | 1.471 / 1.206 / 2.019 / 3.071 / 3.913 / 4.123 ms | no health failures |
| Worker inventory avg / p50 / p90 / p95 / p99 / max | 3.946 / 3.955 / 4.553 / 4.637 / 4.705 / 4.722 ms | 3.257 at every percentile / max | no invalid payloads |

The before proxy distribution contains only the 137 successful exchanges; the
64 listener races are absent from its timing values. It is therefore
failure-censored and cannot support a latency improvement or regression claim.
The defensible result is delivery reliability, **68.159% → 100%**. The before
resource artifact also contains an incomplete Docker-stat sample, so there is
no valid before/after CPU comparison and no optimization-ledger row for this
correctness fix.

### Accepted fixed-rate phase distributions

The following tables retain all exported `avg / p50 / p90 / p95 / p99 / max`
values in milliseconds. Proxy stream time includes WebSocket upgrade, the
tunneled TCP request and valid response marker, and close initiation. Base and
restored phases have 36 worker containers; scaled phases have 60.

| Cycle / phase | Average | p50 | p90 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 / base | 51.461 | 51.000 | 55.000 | 57.000 | 64.000 | 99.000 |
| 1 / scaled | 51.461 | 51.000 | 55.000 | 57.000 | 61.000 | 100.000 |
| 1 / restored | 51.531 | 50.000 | 56.000 | 58.000 | 66.000 | 70.000 |
| 2 / base | 50.748 | 50.000 | 53.000 | 55.000 | 59.000 | 105.000 |
| 2 / scaled | 51.138 | 50.000 | 57.000 | 59.000 | 67.010 | 85.000 |
| 2 / restored | 49.494 | 49.000 | 51.000 | 52.000 | 56.000 | 62.000 |

Health time is the slower result from each concurrent `/livez` + `/healthz`
batch.

| Cycle / phase | Average | p50 | p90 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 / base | 1.310 | 0.962 | 2.442 | 2.637 | 2.662 | 2.668 |
| 1 / scaled | 1.306 | 1.123 | 1.839 | 1.987 | 3.042 | 3.305 |
| 1 / restored | 1.347 | 1.233 | 2.062 | 2.948 | 3.309 | 3.399 |
| 2 / base | 1.339 | 1.302 | 2.147 | 2.463 | 2.789 | 2.871 |
| 2 / scaled | 1.521 | 1.189 | 2.632 | 3.177 | 3.475 | 3.549 |
| 2 / restored | 0.957 | 0.932 | 1.327 | 1.685 | 2.602 | 2.831 |

Worker inventory deliberately has only 2–3 observations per phase because its
safe sustained cadence is one request/10 s.

| Cycle / phase | Average | p50 | p90 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 / base | 4.345 | 4.376 | 4.917 | 4.985 | 5.039 | 5.052 |
| 1 / scaled | 3.331 | 2.762 | 4.601 | 4.830 | 5.014 | 5.060 |
| 1 / restored | 4.402 | 4.402 | 4.808 | 4.859 | 4.900 | 4.910 |
| 2 / base | 3.798 | 4.556 | 4.580 | 4.583 | 4.585 | 4.586 |
| 2 / scaled | 3.851 | 4.482 | 4.538 | 4.545 | 4.551 | 4.552 |
| 2 / restored | 3.386 | 3.582 | 4.136 | 4.205 | 4.260 | 4.274 |

The scaled proxy p95 values, 57 and 59 ms, remained within 2–4 ms of their
same-cycle base values. This is clean functional scale evidence at the held
rate; the run is too short and the differences are too small to claim a
replica-count latency effect.

`HTTP requests` below are the non-WebSocket liveness, readiness, and inventory
requests. Each valid proxy response also had a successful HTTP 101 upgrade.

| Cycle / phase | Valid proxy responses | Health batches | Inventory polls | HTTP requests | Iterations | Passed / failed checks |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 / base | 401 | 21 | 3 | 45 | 425 | 428 / 0 |
| 1 / scaled | 401 | 20 | 3 | 43 | 424 | 427 / 0 |
| 1 / restored | 401 | 20 | 2 | 42 | 423 | 425 / 0 |
| 2 / base | 401 | 20 | 3 | 43 | 424 | 427 / 0 |
| 2 / scaled | 400 | 21 | 3 | 45 | 424 | 427 / 0 |
| 2 / restored | 401 | 20 | 3 | 43 | 424 | 427 / 0 |
| **Total** | **2,405** | **122** | **17** | **261** | **2,544** | **2,561 / 0** |

All six phases held the configured 20 proxy streams/s and had zero dropped
iterations. Across 2,666 HTTP/upgrade observations, `server_5xx` was zero. Each
of the 2,405 upgraded streams had zero handshake, post-upgrade stream,
missing-response, and invalid-marker failures. All 122 health batches and 17
inventories were valid. Both inventory and proxy HTTP-429 counters were zero.

### Lifecycle convergence and resources

The API mutation timings include the work necessary to reach their documented
result. Rollout timings separately cover the saved-definition rollout through
durable Ready state and an exact local Docker label/topology audit.

| Operation | Count | p50 | p95 | max |
| --- | ---: | ---: | ---: | ---: |
| Create workload | 24 | 22,088 ms | 34,622 ms | 35,150 ms |
| Destroy workload | 24 | 36 ms | 53 ms | 57 ms |
| Scale-up convergence, 36 → 60 containers | 2 | 20,604 ms | 21,613 ms | 21,613 ms |
| Scale-down convergence, 60 → 36 containers | 2 | 18,510 ms | 19,556 ms | 19,556 ms |
| Final liveness + readiness probe | 1 | 4 ms | 4 ms | 4 ms |

Both cycles reached 12/12 durable `Present/Ready`, preserved worker assignment
while advancing the workload generation, restored the saved two-service,
three-replica definition, and ended at 12/12 `Absent/Absent`. Worker lease and
session-epoch fences remained valid.

The resource artifact spans 302.473 seconds with 77 timestamped samples. CPU is
percent of one logical core and RAM is MiB. p95 below is the empirical
lower-rank observation at `floor((n - 1) * 0.95)`. The workload-fleet row sums
all sampled replica containers and includes lifecycle periods with no workload,
so it describes the whole run rather than only the six k6 windows.

| Component | CPU mean / p95 / max | RAM mean / p95 / max |
| --- | ---: | ---: |
| rsctf | 4.609 / 9.670 / 17.090% | 24.52 / 26.45 / 32.53 MiB |
| PostgreSQL | 6.364 / 13.200 / 33.770% | 144.58 / 161.40 / 167.90 MiB |
| Redis | 1.179 / 3.500 / 4.770% | 3.34 / 3.34 / 3.35 MiB |
| Native worker agent | 1.081 / 1.300 / 1.400% | 11.37 / 12.12 / 12.12 MiB |
| Workload fleet, whole window | 29.677 / 125.360 / 335.880% | 326.17 / 689.19 / 692.59 MiB |

For the fixed-rate 20 proxy-stream/s baseline, the narrower table below selects
only the **29 warning-free sampler completions whose completion timestamps fell
inside the six 20-second k6 load windows**. Unlike the full-run table above, its
p95 is the empirical nearest-rank observation (`ceil(0.95 * n)`). `Measured
total` is the per-sample sum of rsctf, PostgreSQL, Redis, agent, and
workload-fleet CPU or RAM; its p95 is therefore computed from those sums rather
than by adding the component p95 values.

| Component during fixed-rate windows (n=29) | CPU mean / p95 / max | Peak RAM |
| --- | ---: | ---: |
| rsctf | 7.46 / 16.69 / 17.09% | 26.46 MiB |
| PostgreSQL | 8.28 / 13.12 / 14.41% | 161.40 MiB |
| Redis | 1.45 / 3.89 / 4.32% | 3.35 MiB |
| Native worker agent | 1.13 / 1.30 / 1.40% | 12.12 MiB |
| Workload fleet | 3.47 / 4.42 / 5.12% | 689.20 MiB |
| **Measured total** | **21.80 / 35.60 / 38.78%** | **890.23 MiB** |

The load-window samples divide cleanly by the topology active at completion:

| Fixed-rate topology | Samples | Total CPU mean | Workload CPU mean | Peak total RAM |
| --- | ---: | ---: | ---: | ---: |
| Base/restored, 36 worker-owned replicas | 20 | 20.97% | 3.35% | 616.83 MiB |
| Scaled, 60 worker-owned replicas | 9 | 23.65% | 3.73% | 890.23 MiB |

This is the first valid held-rate resource baseline for the worker plane. The
before-readiness artifact is incomplete, so these values do **not** support a
before/after CPU comparison and do **not** add an optimization-ledger row.

The fleet was present in 59/77 samples. Across the full window its sampled
container count averaged 28.78 with p95/max 60/60; among active-only samples,
count averaged 37.56, CPU 38.73%, and RAM 425.68 MiB. Exact-topology subsets
show the steady shapes separately:

| Exact workload topology | Samples | CPU mean / p95 / max | RAM mean / p95 / max |
| --- | ---: | ---: | ---: |
| 36 containers | 28 | 17.01 / 123.86 / 125.36% | 411.83 / 413.54 / 413.57 MiB |
| 60 containers | 14 | 22.96 / 110.30 / 172.59% | 688.62 / 689.20 / 692.59 MiB |

The workload CPU tails are short create/readiness bursts; median aggregate CPU
was 3.38% at 36 containers and 3.70% at 60. Workload RAM was about 11.5 MiB per
fixture container. The following 30-second buckets preserve the observed
resource trend. Values are arithmetic means of samples in each elapsed-time
bucket, not cumulative or time-weighted CPU.

| Elapsed window | Samples | RSCTF CPU / RAM | PG CPU / RAM | Redis CPU / RAM | Agent CPU / RAM | Fleet count / CPU / RAM |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 0–30 s | 8 | 4.08% / 24.03 MiB | 9.85% / 105.39 MiB | 1.30% / 3.32 MiB | 0.25% / 9.95 MiB | 13.0 / 43.24% / 137.23 MiB |
| 30–60 s | 7 | 5.15% / 22.86 MiB | 8.89% / 126.01 MiB | 1.46% / 3.33 MiB | 0.60% / 10.49 MiB | 36.0 / 20.26% / 408.62 MiB |
| 60–90 s | 7 | 3.98% / 23.05 MiB | 5.64% / 135.76 MiB | 0.92% / 3.33 MiB | 1.07% / 11.13 MiB | 41.3 / 46.14% / 470.28 MiB |
| 90–120 s | 7 | 5.09% / 23.39 MiB | 4.96% / 139.59 MiB | 1.50% / 3.33 MiB | 1.26% / 11.34 MiB | 42.3 / 55.31% / 479.64 MiB |
| 120–150 s | 8 | 6.21% / 23.67 MiB | 8.42% / 144.86 MiB | 1.04% / 3.34 MiB | 1.35% / 11.49 MiB | 22.5 / 2.06% / 258.42 MiB |
| 150–180 s | 10 | 2.46% / 24.38 MiB | 2.14% / 152.16 MiB | 0.93% / 3.34 MiB | 1.26% / 11.55 MiB | 8.4 / 24.51% / 91.10 MiB |
| 180–210 s | 7 | 5.09% / 25.11 MiB | 7.00% / 155.97 MiB | 1.62% / 3.34 MiB | 1.16% / 11.61 MiB | 35.7 / 22.31% / 404.46 MiB |
| 210–240 s | 6 | 3.11% / 25.78 MiB | 3.60% / 158.35 MiB | 0.56% / 3.34 MiB | 1.20% / 11.81 MiB | 44.8 / 85.73% / 509.25 MiB |
| 240–270 s | 7 | 6.18% / 26.09 MiB | 5.42% / 159.94 MiB | 1.36% / 3.34 MiB | 1.29% / 12.11 MiB | 39.4 / 19.74% / 451.65 MiB |
| 270–300 s | 8 | 6.32% / 26.49 MiB | 9.53% / 162.75 MiB | 1.32% / 3.34 MiB | 1.30% / 12.11 MiB | 27.0 / 2.12% / 310.12 MiB |
| 300–302.5 s | 2 | 0.18% / 26.45 MiB | 0.62% / 167.90 MiB | 0.41% / 3.34 MiB | 1.30% / 12.12 MiB | 0.0 / 0.00% / 0.00 MiB |

The sampler was requested every second, but serial
`docker stats --no-stream` across up to 63 containers stretched the observed
completion gap to average/p50/p95/p99/max
3.980/4.149/4.681/5.370/6.184 seconds. Six samples raced intentional replica
deletion and recorded workload-only EOF/no-such-container warnings. Stable
rsctf, PostgreSQL, Redis, and agent records remained complete with zero sample
errors; workload-fleet aggregates and buckets are approximate during churn.
The agent CPU value comes from `ps %cpu` and is a process-lifetime average, not
interval CPU.

### Correctness, cleanup, and limits

For every create and rollout, the direct database audit checked assignment ID,
generation, spec hash, worker binding, desired/observed state, reserved slot,
service count, and required replica count. Docker-label inspection independently
proved each exact three- or five-replica topology and the disappearance of
surplus replicas. No workload moved workers during rollout. The saved challenge
definition ended at its original two-service/three-replica shape.

- The isolated project, its worker-owned containers, network, volumes, PKI, and
  agent state were removed. The pre-existing replicated rsctf deployment kept
  the same container identities and remained healthy throughout. The final
  managed-workload count was zero.
- The shipped runtime is Linux Docker. A Windows PC can host the Linux agent in
  a dedicated VM; native Windows-container execution is not implemented. The
  Kubernetes chart can run the rsctf server, but this agent manages Docker
  workloads rather than Kubernetes pods.
- Replicas in one workload are deliberately co-located on its assigned worker.
  This provides scale-out throughput inside that host, not node-level high
  availability; HA requires multiple agents and independently scheduled
  workloads.
- The agent is outbound-only and mutually authenticates a separate worker
  listener. Enrollment is one-time, but issued worker certificates currently
  expire after 90 days and require operator rotation.
- TCP readiness is checked at startup and recovery, cached after success, and
  invalidated by a failed real proxy dial or runtime change. It is not a
  continuous application-level health check.
- A dedicated VM/host firewall remains required: the Docker host gateway is
  reachable from managed workloads, and the agent does not install host policy.
  Read-only roots, non-root workload identities, user namespaces, and a named
  seccomp profile remain hardening work.
- Remote worker execution is currently for per-team Jeopardy workloads. A&D and
  KotH stay on the local/hybrid path with constant scoring, and the A&D checker
  remains a process rather than a checker container.
- The accepted path is loopback on one host with a small stateless HTTP fixture.
  It does not measure WAN latency/loss, multiple physical workers, worker
  failover under load, CPU-heavy services, or persistent-volume behavior. The
  local fixture deliberately uses the documented host-network-boundary
  acknowledgement and unbounded-storage development override.
- Create/destroy percentiles have 24 observations, rollout percentiles only two,
  and inventory percentiles only 2–3 per phase. They are baseline observations,
  not estimates of production tail latency.

Two attempts are explicitly excluded from every accepted-run table and
performance claim:

| Excluded attempt | Diagnostic result | Why it is non-reportable |
| --- | --- | --- |
| Four-identity admission diagnostic | `FLEET=4 RATE=20` returned 401/401 valid responses in base, then 199 valid responses and 202 HTTP 429 upgrades in scaled. Each identity had spent about 100 of its authenticated 150-request/60-second budget in base, leaving only about 50 for scaled. | It measured the intentional per-user global admission policy, not rollout or tunnel capacity. The harness now fails before provisioning when rate/identity exceeds 2/s. |
| `final4` twelve-identity attempt | An unrelated Docker/Cargo build ran on the host during sampling, and the resource artifact ended with an invalid agent sample. The run was aborted and its project cleaned. | Concurrent compilation breaks host isolation and an incomplete resource series cannot support a fixed-rate CPU claim. No latency or resource value from this attempt is retained. |

Other mixed-source attempts were stopped by the mandatory fingerprint guard
before measurement. The accepted `final5` window was watched throughout: no
unrelated Cargo/Rust compiler, Docker build, or second load generator ran.

## Fixed-rate replicated hot-path optimization campaign — 16 July 2026

This campaign measured seven incremental optimizations on the deployed
two-`web`/one-`control` single-binary topology. The result at the same held
throughput was a **32.79% reduction in measured stack CPU** and a **24.61%
reduction in overall HTTP p95**, with zero failed requests, server 5xx responses,
invalid boards, or integrity violations in every timed pass.

### Controlled workload and provenance

Every pass used the public TLS target `https://tcp.1pc.tf`, PostgreSQL 18.4,
Redis 7, Traefik, and an 8-core host. `npm run player` ran with
`VUS=400 RATE=70 DURATION=300s THINK_MIN_SECONDS=3 THINK_MAX_SECONDS=5` after a
60-second warmup. Each image received a fresh same-shape fixture: 100 Jeopardy
teams plus 400 A&D/KotH teams, one shared exact-flag target service checked by
the process-executed exact checker, and the immutable
hill image
`sha256:bb87e100ef2fb25b19fee84860ed5314cbd0ac740641a6e8d44c359a5a1a69d0`
on port 8080. The workload produced about 429 HTTP requests/s and 21,001
iterations in every timed pass.

Cumulative cgroup `usage_usec` was bracketed around each run for PostgreSQL,
Redis, both web replicas, and control. Here, **app CPU** is
`web1 + web2 + control`; **stack CPU** is `app + PostgreSQL + Redis`. Traefik is
not included. PostgreSQL, `pg_stat_statements`, and Redis counters were bracketed
at the same boundaries. A five-second observer independently sampled the public
route, one web replica's liveness/readiness, container CPU/RAM, PostgreSQL, and
Redis. Fixtures were the same shape and age but not byte-identical database
snapshots.

| Build | Timed image and digest | Jeopardy / mixed games |
| --- | --- | ---: |
| Initial | `rsctf-local:roles-20260716t114721z` / `sha256:d0b83a05149c65f2f9edf6c3af68c18d77a4013223bb1d7a2222cbf6fe7b663e` | 25 / 26 |
| Limiter | `rsctf-local:perf-limiter-20260716t155354z` / `sha256:cbe960d51c16ef2c2c4a2808ce2f728f0a450714be6b46e67fbc1b685e90cffd` | 27 / 28 |
| Lifecycle | `rsctf-local:perf-koth-20260716t162313z` / `sha256:da37e060a5827ee65da4550c2162528dff01be57d0c619d5d82a114250878917` | 31 / 32 |
| Evidence | `rsctf-local:perf-evidence-20260716t165903z` / `sha256:09aeb37e9e64508178f4587597de0ad329a92145ae17e931a319e8a1712df5df` | 33 / 34 |
| State | `rsctf-local:perf-state-20260716t172144z` / `sha256:9b706b213e8deab3744552cc818eb5dc0fdfe6ab6a4c7720e4a865f57d730227` | 35 / 36 |
| Activity | `rsctf-local:perf-activity-20260716t174819z` / `sha256:573be1013e0035032b070288ab4ada9d8b973abf28e800b55c49aab93c7de8e7` | 39 / 40 |
| Eligibility | `rsctf-local:perf-eligibility-20260716t181222z` / `sha256:11662075d68434a7bbd7004e16771776e3329aab4e4a887131d65b6ee9266492` | 41 / 42 |
| Participation | `rsctf-local:perf-participation-20260716t183355z` / `sha256:515f7114d791223a390c85fab96d05ca1e8cfbfa84fe747374b8085720edcc41` | 43 / 44 |

Games 29/30 were an intentionally aborted attempt to provision 400 isolated
service containers; the default `/24` Docker network exhausted at service 249.
Games 37/38 were a failed provision caused by a stale, correctly owned harness
container from an earlier tag-specific teardown mistake. Both attempts failed
safely and cleaned their games and owned resources. Neither contributed any
timed data.

### Iteration results

CPU values are cumulative CPU-seconds over the 304.6–305.0-second k6
execution including graceful drain. `VU max` is observed active VUs; the
configured capacity was 400.

| Run | Requests / req/s | VU max | HTTP p95 / p99 ms | Board p95 ms | PG CPU-s | Redis CPU-s | App CPU-s | Stack CPU-s | Independent health |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Initial | 130,893 / 429.198 | 296 | 9.129 / 18.986 | 8.728 | 143.699 | 44.711 | 157.467 | 345.876 | 70/70 |
| Limiter | 130,905 / 429.724 | 302 | 9.169 / 23.359 | 8.806 | 143.319 | 40.957 | 155.201 | 339.476 | 70/70 |
| Lifecycle | 130,889 / 429.339 | 305 | 9.320 / 39.066 | 8.963 | 123.558 | 41.032 | 151.509 | 316.099 | 71/71 |
| Evidence | 130,862 / 429.113 | 299 | 8.972 / 20.173 | 8.721 | 117.888 | 40.997 | 150.917 | 309.802 | 69/69 |
| State | 130,881 / 429.460 | 311 | 8.802 / 24.486 | 8.729 | 112.266 | 40.835 | 148.245 | 301.347 | 69/69 |
| Activity | 130,889 / 429.437 | 299 | 9.017 / 21.350 | 9.112 | 98.686 | 44.012 | 152.780 | 295.478 | 70/70 |
| Eligibility | 130,902 / 429.410 | 307 | 7.296 / 17.549 | 7.554 | 78.237 | 39.424 | 126.423 | 244.085 | 72/72 |
| Participation | 130,897 / 429.703 | 293 | 6.883 / 12.698 | 7.438 | 71.223 | 40.118 | 121.135 | 232.476 | 74/74 |

Final versus initial at effectively identical throughput (+0.12% request rate):

- PostgreSQL CPU: 143.699 → 71.223 CPU-s (**−50.44%**).
- Redis CPU: 44.711 → 40.118 CPU-s (**−10.27%**).
- Application CPU: 157.467 → 121.135 CPU-s (**−23.07%**).
- Whole measured stack: 345.876 → 232.476 CPU-s (**−32.79%**), or
  2.642 → 1.776 stack-ms/request.
- Overall HTTP p95: 9.129 → 6.883 ms (**−24.61%**); combined-board p95:
  8.728 → 7.438 ms (**−14.78%**).
- A&D State, A&D Targets, KotH State, KotH token, KotH timeline, and A&D submit
  p95 improved by 41.66%, 32.47%, 43.58%, 30.39%, 26.85%, and 24.27%,
  respectively.

### Direct evidence by optimization

| Change | Direct work reduction | Same-rate outcome versus preceding image |
| --- | --- | --- |
| Distributed limiter batch | Redis commands 1,088,050 → 957,334 (**−12.01%**) by evaluating authenticated global and IP-backstop policies in one ordered Lua call. | Redis CPU −8.40%; app CPU −1.44%; stack CPU −1.85%; p95 flat. |
| KotH lifecycle cache | Lifecycle SQL 21,077 calls / 6,112.896 ms → 311 / 188.538 ms (**−98.52% calls; −96.92% SQL time**). Round tags reject cross-round values. | PG CPU −13.79%; app CPU −2.38%; stack CPU −6.89%. |
| A&D evidence set rewrite | Alternating same-snapshot median 112.198 → 47.904 ms (**−57.31%**); live mean 122.458 → 65.924 ms/call (**−46.17%**). | PG CPU −4.59%; stack CPU −1.99%. |
| One-query A&D State tail | Current round, services, flags, and latest status 28,667 → 7,160 statements (**−75.02%**); SQL time 2,143.3 → 1,603.2 ms (**−25.20%**). | State p95 −18.23%; PG CPU −4.77%; app CPU −1.77%; stack CPU −2.73%. |
| Activity writer batch | Legacy UPDATE 37,202 calls / 20,506.572 ms → zero; replacement 2,402 batches / 1,180.526 ms (**−93.54% statements; −94.24% SQL time**). | PG CPU −12.10%; stack CPU −1.95%. App CPU +3.06% and Redis CPU +7.78% in this single pass are retained as measured noise/cost, not hidden. |
| KotH eligibility cache | Full challenge entity 42,002 calls / 4,881.954 ms → zero; narrow set fills 377 / 19.005 ms (**−99.10% calls; −99.61% SQL time**). | KotH token p95 9.213 → 6.607 ms; KotH State 8.246 → 5.794 ms; PG CPU −20.72%; stack CPU −17.39%. |
| Participation join | Two generic lookups totaling 67,420 statements / 4,223.873 ms → zero; one `LEFT JOIN` ran 32,542 times / 3,176.343 ms (**−51.73% statements; −24.80% SQL time**). | PG CPU −8.97%; app CPU −4.18%; stack CPU −4.76%; overall p95 −5.67%. |

### Full endpoint distributions

Each cell is `avg / p50 / p90 / p95 / p99 / max` in milliseconds. The p99 and
max values are intentionally retained: individual maxima were burst-sensitive
and do not justify a blanket tail-latency claim.

| Endpoint | Initial | Limiter | Lifecycle | Evidence | State | Activity | Eligibility | Participation |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Overall | 5.07/4.35/7.14/9.13/18.99/252.51 | 5.19/4.29/7.09/9.17/23.36/297.94 | 5.54/4.18/6.93/9.32/39.07/555.92 | 4.85/4.12/6.85/8.97/20.17/282.51 | 5.21/4.09/6.79/8.80/24.49/713.73 | 5.16/4.27/7.12/9.02/21.35/439.59 | 4.36/3.69/5.77/7.30/17.55/598.02 | 4.02/3.62/5.63/6.88/12.70/228.82 |
| Combined board | 5.12/4.40/6.85/8.73/16.88/252.51 | 5.34/4.37/6.84/8.81/24.45/273.83 | 5.91/4.46/6.87/8.96/37.73/323.48 | 5.14/4.35/6.74/8.72/19.13/282.51 | 5.68/4.37/6.85/8.73/26.59/713.73 | 5.57/4.55/7.20/9.11/23.38/298.55 | 5.05/4.12/6.07/7.55/18.57/598.02 | 4.76/4.20/6.11/7.44/13.49/144.77 |
| Main board | 4.69/4.02/6.46/8.32/14.74/124.67 | 4.91/3.99/6.51/8.43/19.06/236.55 | 5.37/3.96/6.46/8.60/32.45/306.56 | 4.70/3.95/6.43/8.41/16.65/183.84 | 5.11/3.96/6.52/8.47/21.72/232.35 | 5.13/4.14/6.85/8.74/20.04/269.72 | 4.61/3.69/5.66/7.27/15.44/296.09 | 4.34/3.77/5.74/7.13/12.62/144.77 |
| A&D epoch board | 5.33/4.57/6.93/8.74/16.00/252.51 | 5.50/4.53/6.91/8.83/21.92/237.98 | 6.16/4.67/6.99/8.98/37.23/323.48 | 5.31/4.51/6.85/8.70/18.05/282.51 | 5.70/4.52/6.93/8.72/23.18/275.55 | 5.73/4.72/7.30/9.10/20.77/298.55 | 5.20/4.29/6.16/7.55/15.56/598.02 | 4.93/4.37/6.20/7.44/12.62/143.96 |
| KotH board | 5.35/4.56/7.05/9.08/20.88/163.92 | 5.61/4.53/7.03/9.19/30.31/273.83 | 6.20/4.63/7.09/9.30/41.29/316.93 | 5.41/4.50/6.88/9.04/24.90/159.19 | 6.23/4.53/7.05/9.01/33.39/713.73 | 5.86/4.72/7.40/9.50/29.22/269.60 | 5.34/4.29/6.27/7.89/26.84/277.90 | 5.03/4.38/6.28/7.73/17.05/143.65 |
| A&D State | 6.93/5.56/9.97/13.06/32.16/151.75 | 6.70/5.57/9.52/12.78/26.48/96.78 | 7.56/5.52/9.91/14.87/49.83/397.75 | 6.64/5.50/9.79/12.85/24.73/170.46 | 5.66/4.43/7.89/10.51/21.75/468.69 | 5.85/4.58/8.07/10.40/22.08/344.75 | 4.91/4.06/6.67/8.54/21.50/134.07 | 4.37/3.73/6.14/7.62/13.53/127.69 |
| A&D Targets | 5.52/4.30/7.94/11.17/26.91/159.00 | 5.25/4.21/7.71/10.69/23.58/89.93 | 6.11/4.24/7.97/12.88/47.61/212.87 | 5.20/4.19/7.66/10.56/20.84/192.87 | 5.68/4.23/7.74/10.83/26.66/540.80 | 5.66/4.39/7.90/10.46/22.82/322.01 | 4.82/3.87/6.59/8.78/21.66/126.01 | 4.22/3.50/5.85/7.54/15.27/228.82 |
| KotH timeline | 3.91/2.75/5.21/7.55/33.70/109.59 | 3.65/2.60/5.00/7.18/29.05/134.38 | 4.80/2.73/5.33/9.06/54.09/555.92 | 3.58/2.58/4.90/6.95/27.43/214.91 | 3.88/2.64/5.11/7.37/32.28/275.45 | 4.00/2.71/5.08/7.03/33.53/313.76 | 3.52/2.41/4.16/5.95/28.28/312.24 | 3.29/2.47/4.14/5.53/26.74/142.08 |
| KotH token | 4.66/4.06/6.94/8.65/15.72/147.60 | 4.88/4.02/6.95/8.80/19.59/292.33 | 5.04/3.89/6.75/8.88/30.09/442.70 | 4.61/3.94/6.75/8.76/18.94/258.85 | 4.83/3.96/6.79/8.64/18.67/321.01 | 5.02/4.18/7.39/9.21/19.03/439.59 | 3.61/3.10/5.24/6.61/13.99/154.60 | 3.29/2.88/4.85/6.02/10.40/153.54 |
| KotH State | 5.16/4.57/7.39/9.14/16.45/220.32 | 5.30/4.49/7.26/9.20/20.40/297.94 | 4.32/3.36/5.91/7.83/28.10/356.00 | 3.95/3.38/5.87/7.65/15.93/197.11 | 4.18/3.39/5.95/7.75/17.14/392.30 | 4.35/3.56/6.46/8.25/17.54/330.09 | 3.02/2.62/4.49/5.79/11.39/153.86 | 2.69/2.37/4.07/5.15/8.93/140.14 |
| A&D submit | 4.06/3.10/5.89/7.95/17.36/104.74 | 3.90/3.11/6.02/8.21/15.75/41.25 | 5.44/3.03/6.89/13.43/55.02/241.35 | 4.72/3.20/7.42/11.35/29.70/84.65 | 5.75/3.23/7.96/12.79/49.61/411.97 | 3.90/3.27/5.98/7.56/12.22/27.04 | 4.16/3.01/5.81/7.94/25.34/185.62 | 3.44/2.98/4.67/6.02/11.37/45.56 |

### Database, Redis, and observer evidence

| Run | DB commits | Rollbacks | Buffer hits | Redis commands | Public / local / ready |
| --- | ---: | ---: | ---: | ---: | ---: |
| Initial | 274,212 | 0 | 5,357,162 | 1,088,050 | 70/70 each |
| Limiter | 274,373 | 0 | 5,448,332 | 957,334 | 70/70 each |
| Lifecycle | 253,470 | 0 | 4,414,422 | 957,680 | 71/71 each |
| Evidence | 254,138 | 0 | 4,299,457 | 959,174 | 69/69 each |
| State | 232,300 | 1 | 2,712,648 | 958,529 | 69/69 each |
| Activity | 198,024 | 0 | 4,305,904 | 959,874 | 70/70 each |
| Eligibility | 156,872 | 0 | 2,406,938 | 961,503 | 72/72 each |
| Participation | 121,888 | 0 | 4,766,820 | 960,148 | 74/74 each |

Every run had zero PostgreSQL block reads, temp files/bytes, and deadlocks, plus
zero Redis evictions, rejected connections, or new error replies. The State
pass's single rollback had no associated HTTP failure, 5xx, deadlock, temp
spill, or health failure. Buffer-hit counts vary with query shape and control
work, so they are supporting evidence rather than a standalone CPU proxy.

The observer samples one web replica, not total application CPU. The following
five one-minute buckets cover the timed portion of the initial and final
observer windows. CPU is mean percent of one core; RAM is the bucket maximum in
MiB.

| Minute | Initial web CPU / RAM | Final web CPU / RAM | Initial PG CPU / RAM | Final PG CPU / RAM | Initial Redis CPU / RAM | Final Redis CPU / RAM |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 14.150% / 33.91 | 13.818% / 52.65 | 24.132% / 323.20 | 19.068% / 343.40 | 9.232% / 7.08 | 8.033% / 6.46 |
| 2 | 26.635% / 51.82 | 18.071% / 62.59 | 47.137% / 331.00 | 25.013% / 357.60 | 15.004% / 6.97 | 12.481% / 6.84 |
| 3 | 27.299% / 55.11 | 19.593% / 65.07 | 43.568% / 328.30 | 28.964% / 376.20 | 16.679% / 7.34 | 15.442% / 6.66 |
| 4 | 29.171% / 59.91 | 19.465% / 65.23 | 47.659% / 339.00 | 29.717% / 387.10 | 15.933% / 7.28 | 14.831% / 6.79 |
| 5 | 25.040% / 63.59 | 19.425% / 71.51 | 50.346% / 353.20 | 26.157% / 402.20 | 13.655% / 7.41 | 12.472% / 6.89 |

The final PostgreSQL RAM series starts from an older, warmer database than the
initial run and is not a causal memory comparison. It shows bounded cache growth
during the five-minute event, not evidence for or against a long-duration leak.

### Correctness, rollout behavior, and limits

- Every timed pass completed 21,001 iterations with zero failed requests,
  server 5xx, harness errors, or invalid A&D epoch boards. Duplicate rounds,
  attacks, KotH tokens/cycles/control ticks/acquisitions, and overlapping active
  cycles were zero after every pass.
- Stable-IP activity observations may be delayed by 250 ms; IP changes flush
  immediately. Queue or pool saturation remains best-effort and releases the
  local throttle reservation for retry. Replica batches sort by user ID to
  avoid opposite-order row-lock deadlocks.
- Live KotH eligibility/lifecycle caches use a one-second TTL. Existing remote
  L1 copies expire within one second; a pre-mutation fill racing invalidation
  can repopulate L2 and extend the rare stale window to about two TTLs. Official
  challenge toggles/review changes are locked once scoring begins.
- The batched limiter in this historical measured build was correct for the
  deployed standalone Redis. Redis Cluster would require both authenticated
  keys to share a hash slot; otherwise Lua returns `CROSSSLOT`. Current code
  falls back to a bounded per-replica limiter on Redis errors instead of
  becoming unlimited, but global cross-replica coordination is then lost.
- One run per image is insufficient for latency confidence intervals. Held-rate
  cgroup CPU plus direct SQL/Redis work reductions are the stronger causal
  evidence. p99/max values are reported rather than smoothed away.
- The final timed campaign image is the Participation build listed above. The
  subsequently deployed acceptance image has the same optimized code plus final
  cache-invalidation comments/tests and documentation, but is not presented as
  another measured performance step.
- Runtime fingerprint equality rejects mixed binaries, so each deployment used
  a coordinated recreate. Public rollout samplers recorded maintenance-window
  responses separately from the clean timed runs:

| Rollout | 200 | 500 | 503 | 429 | Note |
| --- | ---: | ---: | ---: | ---: | --- |
| Limiter | 127 | 17 | 5 | 0 | Normal coordinated recreate. |
| Lifecycle | 22 | 89 | 4 | 35 | An omitted existing DB environment variable caused an extended restart; corrected without data mutation. |
| Evidence | 97 | 17 | 6 | 0 | Normal coordinated recreate. |
| State | 99 | 17 | 4 | 0 | Normal coordinated recreate. |
| Activity | 94 | 16 | 5 | 0 | Normal coordinated recreate. |
| Eligibility | 95 | 16 | 5 | 0 | Normal coordinated recreate. |
| Participation | 95 | 17 | 4 | 0 | Normal coordinated recreate. |
| Final acceptance | 137 | 32 | 11 | 0 | Coordinated recreate to the final audited image; post-rollout public/local/ready probes each passed 20/20. |

These rollout responses are deployment downtime, not load-test failures. All
fixtures were torn down after their integrity audit.

### Final exact-image lifecycle acceptance

The complete audited tree was built and deployed as
`rsctf-local:perf-final-20260716t185504z`, image
`sha256:0e35a5937a0f19db9d086e8834990defc94e5c1965b9e281f17c1f3cf03255c0`.
Both web replicas and control run that exact digest with zero restarts or OOM
kills. Its binary contains the measured optimizations plus the final eligibility
invalidation race fix; the repository snapshot adds the regression tests and
documentation. The image is not silently substituted for the Participation
image in the causal ledger above.

The passing comprehensive run used 20 Jeopardy teams, 20 A&D/KotH teams, 20
real BYOC tunnels, 60 k6 VUs, a five-second player think time, and a 300-second
window through public TLS. All A&D teams shared one exact-flag target service;
the platform's exact checker remained a process, not a checker container. The
KotH hill used immutable image
`sha256:bb87e100ef2fb25b19fee84860ed5314cbd0ac740641a6e8d44c359a5a1a69d0`
on port 8080.

| Metric | Passing result |
| --- | ---: |
| Requests / rate | 26,307 / 86.348 req/s |
| Iterations / rate | 15,448 / 50.705 per second |
| HTTP avg / p50 / p90 / p95 / p99 / max | 4.909 / 1.987 / 10.592 / 16.856 / 43.135 / 334.352 ms |
| Combined board p95 | 26.692 ms |
| A&D epoch board p95 | 7.760 ms |
| A&D State / Targets p95 | 16.262 / 18.013 ms |
| Blended Jeopardy details/challenge + KotH State p95 | 17.308 ms |
| KotH board p95 | 25.775 ms |
| A&D submit p95 | 123.974 ms |
| Attachment / container-operation p95 | 119.209 / 334.280 ms |
| Server 5xx / invalid A&D boards / invalid KotH lifecycle payloads | 0 / 0 / 0 |
| Unexpected non-2xx excluding 429 | 32 (0.310%) |
| Lifecycle liveness / readiness | 513/513 / 513/513 |

The 15,969 HTTP 429 responses are retained in the JSON evidence rather than
counted as successes. The public test driver is one source address, so anonymous
and other source-partitioned traffic eventually exercises the intentional
ceiling even though the script varies an untrusted forwarding header. The
authenticated A&D and KotH semantic probes all returned valid models. This run
is therefore a whole-platform correctness gate, not a replacement for the
fixed-rate CPU comparison above.

All printed integrity gates were zero: duplicate/non-contiguous rounds,
attacks, tokens, cycles, control ticks, acquisitions, participations, and
runtime operations; overlapping cycles; cadence/duration drift; late evidence;
delivery/publication failures; invalid reset receipts, cooldowns, holders, or
container/cross-cycle attribution; scorable voids; liveness/readiness failures;
and panics. The run additionally completed the heavy paths:

- 20/20 A&D flags were delivered and independently checker-verified; 10 attacks
  were accepted after the baseline.
- Three real Jeopardy container creates ran; normal deletion plus final teardown
  reaped every instance, and the attachment upload/download path was exercised.
- The driver wrote 37 KotH captures across five observed cycles, completed two
  crown cycles, confirmed one acquisition and one stable hold, and rejected the
  revoked prior-cycle capability 1/1.
- A&D flag publication lag was p95 5.085 seconds and max 5.090 seconds, inside
  the 8/12-second gates.

An independent five-second observer sampled health 73 times. Public health,
local liveness, and replica readiness each passed 73/73; the resource table has
72 successful Docker samples per component. The two collector errors were
fleet-stat EOFs at `19:36:40 UTC`, after the lifecycle had intentionally reaped
all 20 tunnel containers; health sampling remained clean. These resource values
include preparation and settlement and sample one web replica, so they are
operational bounds rather than a before/after ledger comparison. Resource p95
uses the empirical lower-rank observation at `floor((n - 1) * 0.95)`.

| Component | Mean / lower-rank p95 CPU (% of one core) | Peak RAM |
| --- | ---: | ---: |
| One web replica | 2.469 / 9.800 | 39.16 MiB |
| PostgreSQL | 6.163 / 12.990 | 550.10 MiB |
| Redis | 2.343 / 4.670 | 5.40 MiB |
| Traefik | 10.178 / 13.870 | 137.40 MiB |

Two preceding attempts are intentionally retained as diagnostics rather than
being relabeled as passes:

| Attempt | Shape | Traffic / p95 | Result and interpretation |
| --- | --- | --- | --- |
| Saturation diagnostic | 20 teams, 120 VUs, no think time, 300 s | 1,787,294 requests at 5,956.85 req/s; p95 39.329 ms | Failed: 24 server 5xx and 94.90% HTTP failures, dominated by rate-limit responses; semantic metrics rejected 97.57% of A&D and 97.77% of KotH samples. Valid payloads before load and the clean same-image reruns show this shape primarily measured policy rejection, not useful player capacity. |
| Strict-contract diagnostic | 20 teams, 60 VUs, five-second think time, 300 s | 26,307 requests at 86.386 req/s; p95 16.092 ms | Zero 5xx and zero semantic-invalid models. It stopped before settlement only because the temporary `STRICT_ZERO_ERRORS=1` also rejected 32 non-429 non-2xx responses (0.309%) allowed by the repository's normal sub-1% gate. |
| Accepted lifecycle | Same representative shape | 26,307 requests at 86.348 req/s; p95 16.856 ms | Normal contract passed, every integrity gate passed, and the namespace was then torn down successfully. |

## Split-role replica deployment and scale rehearsal — 16 July 2026

RSCTF was initially deployed from `rsctf-local:roles-20260716t103411z`
(`sha256:b535c702812f03aa8c3fb34ca30bd9d3f8c861a9d8ecb25062c8d09bd6690384`)
as two public `web` replicas and one singleton `control` replica. Ordinary HTTP
traffic is balanced across the web replicas; state-owning BYOC and
container-exec routes remain pinned to control. All three application
containers finished healthy on one runtime build fingerprint, with zero
restarts and zero OOM kills.

The rehearsal exposed and corrected two deployment defects. Checker scratch
permissions are now set before ownership is transferred under the minimal
`CAP_CHOWN`, so control starts without adding `CAP_FOWNER`. A draining web
replica now becomes unready immediately but continues serving ordinary requests
during the existing five-second load-balancer deregistration window; stateful
roles still reject fresh work while draining. Traefik actively checks
`/healthz` every second. The first diagnostic run had 1,453 scale-in 5xx, all
from the two retiring replicas, which is why its throughput and latency are not
used as a performance baseline.

A corrected 400-player fixed-rate run started with two web replicas, scaled to
four, and returned to two during a 180-second window. It completed 14,400
iterations and 89,802 requests at 485.95 req/s with zero server 5xx and 225/225
successful independent readiness probes. HTTP p95 was 30.78 ms, board p95
23.47 ms, A&D epoch-board p95 23.52 ms, and submit p95 68.48 ms. All four web
replicas received traffic, and the scale-in window served 255 successful
requests from the retiring replicas before Traefik removed them. The remaining
238 HTTP failures were 429s at the configured 30,000 requests/minute
shared-source credential-admission ceiling; one scheduler iteration was
dropped. This is clean scale-transition safety evidence, not a fully passing
player-semantic gate.

### Performance comparison note

The replica rehearsal does not have a causal monolith-versus-replica latency
comparison: its fixture, identities, request rate, and topology differ from the
older runs. Its after-deployment measurements are therefore recorded as 30.78
ms overall HTTP p95, 23.47 ms board p95, 23.52 ms A&D epoch-board p95, and
68.48 ms submit p95 at 485.95 req/s, without claiming that replicas alone caused
those values.

The repository does have one valid same-shape latency comparison. Both sides
used the same public target, 100 team clients, 100 relays, 100 isolated
services, five-minute duration, and five-second client think time. It measures
the A&D scoreboard stale-while-revalidate, single-flight, and revision-fenced
invalidation work described in the detailed gate later in this report.

| Latency metric | Before | After | Change |
| --- | ---: | ---: | ---: |
| Overall HTTP mean of team averages | 305.9 ms | 111.2 ms | -63.6% |
| Overall HTTP median team p95 | 1,194.6 ms | 531.7 ms | -55.5% |
| A&D scoreboard mean of team averages | 1,129.6 ms | 84.7 ms | -92.5% |
| A&D scoreboard median team p50 | 421.4 ms | 15.3 ms | -96.4% |
| A&D scoreboard median team p95 | 4,846.2 ms | 334.1 ms | -93.1% |
| A&D scoreboard p95 of team p95 | 4,948.1 ms | 579.7 ms | -88.3% |

| Lifecycle run | Shape | Traffic and health | Domain result |
| --- | --- | --- | --- |
| `lifecycle2` | 400 teams, 20 BYOC tunnels, 120 VUs, 120 s | 14,481 requests at 116.04 req/s; zero 5xx; 194/194 liveness and readiness waves | Integrity and race checks passed, but the phase-dependent window ended with only one observation in each candidate cycle, so no acquisition could confirm. |
| `lifecycle3` | Same shape, 300 s | 35,776 requests at 117.29 req/s; zero 5xx; 474/475 public liveness and readiness waves | All 35 pre-deadline non-probe domain and integrity gates passed: one confirmed acquisition, one qualified stable confirmation, 1/1 stale-token rejection, and three completed crown cycles. The post-run deadline audit found the cleanup defect described below. |

The final lifecycle run also completed 20/20 BYOC delivery and checker
verification, spawned four real challenge containers, downloaded attachments,
and recorded 37 KotH capture writes. Unexpected non-2xx responses excluding
429 were 0.207%; A&D publication lag was p95 6.077 seconds and max 6.081
seconds, inside the 8/12-second gates. Overall HTTP p95/p99 was 26.56/132.93
ms; A&D submit p95 was 180.04 ms and container-operation p95 was 798.82 ms.
The single failed public probe occurred at `11:25:45 UTC`; Traefik continued
serving 573 other requests during the interval with zero 5xx and a 41.32 ms
maximum, and a post-run check returned 100/100 liveness plus 100/100 readiness
responses. It is recorded as an isolated client-to-public-endpoint timeout, not
an application outage, but the lifecycle wrapper correctly remained nonzero on
its strict zero-probe-failure gate.

The post-lifecycle audit found an uncovered deadline failure. An elected
champion without an active A&D VPN peer correctly left its next crown cycle in
`FirewallPending`, with a selected cooldown that had
`network_enforced = false` and no enforcement timestamp. Deadline cleanup then
tried to stamp that never-enforced row as released. This violated
`ck_koth_cycle_cooldowns_network` and left the pre-activation cycle, live
capabilities, target, and container reference behind. The 35 earlier gates were
therefore valid pre-deadline evidence, but not terminal-cleanup acceptance.

The production fix is isolated in
`services/ad/engine/koth_cycle/lifecycle/deadline/access.rs`. It revokes live
capabilities, deletes only never-enforced cooldown intent owned by a
pre-activation or failed cycle, and stamps a release only when durable
enforcement evidence exists. A missing receipt on an active, cooldown-release,
completed, or ended cycle now conflicts instead of inventing evidence or
weakening the database constraint. A PostgreSQL regression test covers the
failed `FirewallPending` case, an actually enforced historical cooldown, an
unrelated hill, idempotent retry, and protected scoring evidence.

The fix was released as `rsctf-local:roles-20260716t114721z`
(`sha256:d0b83a05149c65f2f9edf6c3af68c18d77a4013223bb1d7a2222cbf6fe7b663e`)
to the same two-web/one-control topology. Exact runtime-fingerprint matching
required a coordinated stop/recreate instead of a mixed-version rolling
upgrade. The 150-second public rollout sampler consequently recorded five 503s
and sixteen 500s during that 21-second maintenance window, followed by 114
consecutive HTTP 200 responses. Traefik produced the 503s while no RSCTF web
backend was eligible; the 500s came from the unrelated fallback homepage route
while the RSCTF router was absent. This is public deployment downtime, not
load-test traffic, and should not be hidden by the healthy final state.

The patched image then ran a fresh 20-team fixture with 20 real BYOC tunnels,
30 player VUs, and a 300-second player window. k6 completed 13,949 iterations
and 21,613 requests at 71.33 req/s with zero server 5xx and 0.24% unexpected
non-2xx responses after excluding 429s. Board, A&D epoch-board, details, A&D
submit, onboarding, container-operation, and attachment-download p95 were
34.42, 7.31, 16.65, 79.73, 142.80, 386.74, and 81.24 ms respectively. The run
verified 20/20 selected-service deliveries and checkers, made 32 KotH writes,
accepted five capture requests, produced one confirmed acquisition and one
qualified stable confirmation, rejected the stale capability 1/1, and
completed two crown cycles. A&D publication lag was p95/max 5.090 seconds.

k6 itself exited zero, but the lifecycle wrapper correctly exited nonzero: its
strict liveness and readiness gates each recorded 468/469 successes. The
independent public sampler likewise returned HTTP 200 for 419/420 requests; at
`12:03:12 UTC` its host-side client timed out after 3.001 seconds without an
HTTP response (`000`). Its remaining samples had p95 52.8 ms and p99 147.1 ms,
and the last 243 were consecutive HTTP 200 responses. This is the same isolated
public-client caveat, so the run is not reported as a fully green wrapper exit.

The wrapper's failure cleanup ended the fixture at `12:06:03.894 UTC`, taking
the exact deadline path that had failed before. By `12:06:07.903 UTC` it had
converged, and four successive audits through `12:07:12 UTC` stayed clean:
zero nonterminal cycles, unreleased cooldowns, live tokens, routable/live
targets, claim states, or shared-container references. The first three crown
cycles were `Completed`; cycles 4 and 5 were `Ended`. Control emitted a bounded
122-warning retry fan-out while the four-second cleanup converged (60 BYOC
revocation retries, 61 reconcile failures, and one safety-audit warning), but
there was no cooldown-constraint signature or persistent error storm, and no
matching warning, error, or problem line after recovery.

These runs are operational and functional acceptance evidence, not an
optimization comparison. No optimization-ledger row is added: the 2→4→2 run
changes topology, the diagnostic and corrected player identities differ, and
the lifecycle windows and fixture sizes differ in duration and crown-cycle
alignment. There is no same-harness, same-shape fixed-load before/after pair.

## PostgreSQL 18.4 upgrade verification — 16 July 2026

The managed database was recreated from empty on `postgres:18.4-alpine3.24`; the
discarded PostgreSQL 16 volume was not reused. PostgreSQL initialized the new
cluster with data checksums, worker-based asynchronous I/O (`io_workers=3`,
`effective_io_concurrency=16`), and `pg_stat_statements` 1.12. RSCTF applied all
53 migrations, including `m0053_roster_indexes`, and local plus public liveness
and readiness checks passed after deployment.

A five-minute competitive smoke run exercised the public TLS endpoint with 100
independent team clients, 100 WireGuard peers, 100 BYOC relays, 100 isolated A&D
services, one shared KotH hill, and the integrated anti-cheat drill. It produced
28,767 HTTPS/VPN requests, 736 accepted A&D attacks, 49 KotH writes, four crown
cycles, 21 accepted Jeopardy solves, and 491/491 successful liveness and
readiness probes. All 100 team processes exited successfully, with zero 5xx,
timeouts, retries, panics, malformed summaries, or per-team threshold failures.

Every platform-integrity query returned zero: duplicate rounds, attacks, tokens,
cycles, control ticks, acquisitions, participations, stale-container evidence,
late evidence, unfinished pipelines, lock/race artifacts, and invalid reset
receipt chains. Publication lag was p95 2.479 seconds and max 2.509 seconds,
below the 8/12-second gates.

Seven fixed-interval resource samples observed PostgreSQL RAM rise from 413.1 to
470.7 MiB as the event populated data and cache; connections remained exactly 41,
active queries were 1–3, and lock waiters remained zero. RSCTF RAM stayed within
91.6–98.2 MiB. After event teardown both services were idle and healthy; the
database retained cache pages but no event rows. The 14.5 million
relation-buffer hits versus about 14 MiB of client relation reads show that cache
residency contributed materially to RSS. Seven samples over five minutes do not,
by themselves, exclude a long-duration leak. The one-hour results below used the
previous PostgreSQL cluster, so a one-hour PostgreSQL 18 observation remains pending.

| UTC sample | App CPU | App RAM | PG CPU | PG RAM | Redis CPU | Redis RAM | DB active / lock waiters |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 01:32:35 | 25.80% | 95.36 MiB | 45.45% | 413.1 MiB | 2.57% | 7.512 MiB | 1 / 0 |
| 01:33:22 | 24.85% | 98.21 MiB | 37.36% | 435.0 MiB | 2.46% | 7.457 MiB | 1 / 0 |
| 01:34:10 | 33.25% | 94.56 MiB | 46.86% | 452.1 MiB | 2.12% | 7.566 MiB | 2 / 0 |
| 01:34:57 | 25.06% | 93.71 MiB | 18.09% | 453.0 MiB | 2.49% | 7.559 MiB | 1 / 0 |
| 01:35:45 | 15.69% | 93.30 MiB | 13.30% | 457.7 MiB | 2.75% | 7.648 MiB | 1 / 0 |
| 01:36:33 | 367.73% | 96.67 MiB | 46.15% | 462.9 MiB | 2.17% | 7.648 MiB | 3 / 0 |
| 01:37:22 | 65.70% | 91.61 MiB | 62.58% | 470.7 MiB | 3.00% | 7.371 MiB | 1 / 0 |

The ephemeral run used lifecycle tag `pg18-competitive`, games 9/10, model-v2
seed `rsctf-competitive-v2`, and a 300-second player window. Teardown removed its
team artifacts and manifest, so this is a diagnostic smoke record rather than a
retained/replayable acceptance artifact. A final clean Compose recreation kept
the PostgreSQL 18 volume and returned idle RSS to 39.0 MiB for PostgreSQL and
20.6 MiB for RSCTF.

This short run exited nonzero on nine competition-depth gates. Five minutes did
not produce enough completed repair journeys, repeated Jeopardy solves/container
teardowns, or two-round stable KotH confirmations. Those are model-coverage
failures, not server/integrity failures, and the gates were not weakened. The
passing one-hour event below remains historical application-lifecycle evidence,
not PostgreSQL 18 acceptance evidence. No optimization-ledger row is added for the
PostgreSQL upgrade because there is no same-shape PostgreSQL 16/18 fixed-load pair;
targeted query plans are reported below instead.

Before deleting the old volume, two SQL rewrites were compared with
`EXPLAIN (ANALYZE, BUFFERS)` against the same retained 100-team event snapshot.
This isolates query shape from the major-version change and is not presented as
a PostgreSQL 16-versus-18 benchmark.

| Query rewrite | Before | After |
| --- | ---: | ---: |
| Previous checker status: field-wide `DISTINCT ON` scan to one lateral backward index seek per service | 154.6 ms; 12,498 shared-buffer hits | 48.2 ms; 411 hits |
| Player A&D State services: load the full event roster and filter in Rust to a participant-scoped SQL projection | about 2.0 ms; 543 hits | 0.6 ms; 13 hits |

## Official11 passing historical event — 15 July 2026

RSCTF completed and settled a full one-hour competition through the public TLS
endpoint with 100 independently authenticated teams, 100 WireGuard relays, 100
isolated A&D services, one shared KotH hill, real challenge-container journeys, and
an integrated anti-cheat drill. The lifecycle exited zero, all 100 schema-v9 team
artifacts passed, every scoring/integrity gate returned zero, and the retained
manifest is `completed`.

### Identity and provenance

| Item | Value |
| --- | --- |
| Public path | `https://tcp.1pc.tf` through Traefik and TLS |
| Player window | `2026-07-15 14:53:29–15:53:29 UTC` (half-open, exactly 3,600 seconds) |
| Settlement deadline | `15:53:44 UTC` after 15 seconds of grace |
| Historical game IDs | Jeopardy `115`; mixed A&D/KotH `116` (`hidden=false` at capture time) |
| Challenges | A&D `563`; KotH `564`; anti-cheat `565` |
| Competition run | `eef8d03b-93c7-4bac-a55f-2b40715c09ee`; model v2; seed `rsctf-competitive-v2` |
| Workload | 100 clients, 100 authenticated peers, 100 relays, 100 isolated services, one shared hill |
| Player mix | 10 always-on, 25 committed, 45 part-time, 20 casual; five balanced 20-team specialties |
| Deployed image | `sha256:7dc26284572213c28ad957c076d83a9a63465fe8525ffc3670296812ff5d7f08` |
| Application source | `4ec30d67858264634f2e1a359bd9758dbd815769` |
| Harness source | `db880a80ff04935dc39880275d2f134d20bbb753` |
| Lifecycle result | Exit 0; manifest was `completed`; run-owned fleet reaped; games and namespace were retained at capture time |

The observer started from a clean `db880a8` worktree and bound its metadata to the
exact competition UUID. The application image is the clean production image built
from `4ec30d6`; the later commits in the observer provenance affect repository legal
files and the load harness, not the deployed application binary.

### Workload and competitive outcome

| Signal | Result |
| --- | ---: |
| Team summaries | 100/100; 0 malformed; 0 threshold failures |
| HTTPS/VPN requests | 346,636 |
| Work conservation | 47,866/47,866 completed; 38,252 active + 9,614 idle; 0 runtime errors; 0 hard-stop tails |
| Platform first failures | 19 timeouts; 19/19 retried and recovered; 0 exhausted |
| Final platform 5xx / 429 / unexpected / timeout | 0 / 0 / 0 / 0 |
| A&D exploit outcomes | 13,228: 9,372 captured, 3,662 patched, 194 unavailable |
| A&D submissions | 9,372/9,372 accepted; 0 duplicate, replay, terminal, or unresolved |
| Defense activity | 100/100 teams advanced; 55 incidents; 53 repairs |
| Action budget | 16,037 credits spent; 68 decisions denied |
| Jeopardy activity | 341 accepted solves by 96 teams; 163 wrong guesses; 39 attachments; 32/32 container journeys |
| KotH writes | 592/602 successful by 98 teams; 542 opening and 50 takeover writes |
| KotH target failures | 8 HTTP 4xx + 2 HTTP 5xx; 0 network/unclassified failures; these are hill-target results, not RSCTF API 5xx |
| KotH patches | 26/28 applied; 5/5 repairs; 16/16 healthy holds; 5 blocked and 13 bypassed takeovers |
| KotH reset evidence | 25/25 replacement-observed patch losses; 0 retained old instances; 0 status-proof failures |

The A&D field contained all 100 attackers, 97 victims, and 1,855 unique
attacker-victim pairs. Per-attacker captures ranged from 4 to 401 across 80 distinct
totals (coefficient of variation 0.859). Nine A&D leaders produced nine changes;
eight Jeopardy leaders produced 16 changes; KotH had 12 confirmed leaders. Specialty
lift was 1.537 offense, 1.007 defense, 2.520 KotH, and 1.524 Jeopardy.

### Scoring, lifecycle, and anti-cheat

KotH used the fixed formula with an immutable official configuration snapshot: 12-tick epochs, 3-tick crown cycles,
one champion-cooldown tick, two confirmation ticks, 100 roster members, and one
hill. A&D finalized 121/121 rounds and 16 epochs. Its total epoch weight was 15.125,
including a final 1/8 partial epoch. KotH finalized 41/41 cycles and 11 epochs with
total weight 10.083333, including a final 1/12 partial epoch.

| Board | Settled result |
| --- | --- |
| A&D | `fullySettled=true`; 100 distinct nonzero scores; 38.228543–43.192819; ranks valid |
| KotH | `fullySettled=true`; 36 nonzero teams; 11 distinct positive scores; 0–4.523441; comparator/ranks valid |
| Jeopardy | 100 rows; 341 solves by 96 teams; 35 distinct scores; comparator/ranks valid |

KotH recorded 77 controlled ticks, 77 responsible ticks, 72 healthy responsible
ticks, 36 provisional claimants, 12 confirmed controllers, 19 interrupted claims,
and 14 stable confirmations/acquisitions. All stale capabilities were rejected,
cooldowns were released, and deadline cleanup left zero live tokens, claims, target
runtime state, container rows, or shared-container references.

All 41 cycles completed and retained filesystem snapshots; 39 diffs were nonempty
and two were empty. Fourteen champion cooldowns across 12 teams were network-enforced
and released for exactly one round. Personal eligible-tick denominators matched those
cooldowns for every team. All 4,100 cycle capabilities were revoked after use.

The run-scoped reset audit found zero invalid chains. Every activated cycle has a
matching destroy receipt for its old container, a create receipt binding the exact
replacement to the snapshotted image, and a functional-readiness `Ok` receipt. The
player-side check separately proves that an old patch disappeared by observing a
different instance. A replacement that another team already patched is therefore no
longer misclassified as a reset failure.

The integrated anti-cheat artifact is bound to the same run and challenge `565`. It
contains six intended offenders and 94 clean controls, 18 suspicion rows, 12 clean
context rows, and all 14 semantic integrity booleans true. Normal scoring and health
continued during the drill. At capture time, the database retained 22 immutable
events across 18 participants, including four stolen-flag actors, 40 distinct
brute-force answers, and three honeypot baits; evidence keys were unique and
persisted scores reconciled.

### Independent observer

The exact player window contains 695 synchronized waves. Every public liveness,
local liveness, and local readiness probe returned HTTP 200. Percentiles use R-7
linear interpolation.

| Endpoint | Success / failure | Average | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Public liveness | 695 / 0 | 61.7 ms | 51.6 ms | 124.8 ms | 195.2 ms | 304.1 ms |
| Local liveness | 695 / 0 | 6.0 ms | 1.8 ms | 25.3 ms | 97.1 ms | 236.5 ms |
| Local readiness | 695 / 0 | 8.4 ms | 2.2 ms | 28.9 ms | 130.0 ms | 367.8 ms |

| Component | CPU average / p95 / max | RAM average / p95 / max |
| --- | ---: | ---: |
| RSCTF | 59.9 / 316.8 / 516.9% | 170.5 / 435.2 / 583.3 MiB |
| PostgreSQL | 50.6 / 89.1 / 155.7% | 568.1 / 646.8 / 652.3 MiB |
| Redis | 3.2 / 5.8 / 9.3% | 7.4 / 9.5 / 11.2 MiB |

All 695 fixed-cohort samples contained exactly 100 clients, 100 relays, and 100
isolated services. Combined fleet CPU averaged 102.7%, with p95 128.9% and max
393.3%; RAM averaged 3,447.4 MiB, with p95 3,663.0 MiB and max 3,693.1 MiB.
RSCTF's lifecycle bursts are visible in the CPU/RAM maxima, but they were short and
did not produce a failed health probe.

PostgreSQL averaged 32.87 connections, 1.56 active and 0.32 waiting, with maxima
34/8/4. Waiting locks remained zero, and the window added no deadlocks, rollbacks,
or temporary files. Database size grew 10.930 MiB. Redis logical memory averaged
1.714 MiB and peaked at 1.974 MiB (RSS max 6.090 MiB), with at most 310 keys, zero
evictions, zero rejected connections, and zero new error replies under its 256 MiB
`allkeys-lru` cap.

| UTC interval | n | App CPU | App RAM | PG CPU | PG RAM | Redis CPU | Public avg / p95 | DB active / waiting |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 14:53:29–14:58:29 | 59 | 57.2% | 150.5 MiB | 41.8% | 559.3 MiB | 2.8% | 55.0 / 94.8 ms | 1.39 / 0.31 |
| 14:58:29–15:03:29 | 58 | 57.3% | 166.8 MiB | 56.3% | 607.2 MiB | 3.4% | 62.4 / 106.0 ms | 1.69 / 0.31 |
| 15:03:29–15:08:29 | 59 | 63.5% | 182.3 MiB | 51.8% | 627.3 MiB | 3.6% | 60.3 / 112.5 ms | 1.51 / 0.37 |
| 15:08:29–15:13:29 | 58 | 60.0% | 175.4 MiB | 51.5% | 642.9 MiB | 3.2% | 59.7 / 131.9 ms | 1.57 / 0.29 |
| 15:13:29–15:18:29 | 57 | 53.8% | 175.6 MiB | 56.6% | 622.4 MiB | 3.3% | 65.4 / 114.4 ms | 1.60 / 0.35 |
| 15:18:29–15:23:29 | 58 | 56.4% | 155.6 MiB | 59.9% | 551.6 MiB | 2.8% | 59.2 / 103.9 ms | 1.43 / 0.31 |
| 15:23:29–15:28:29 | 55 | 67.9% | 189.8 MiB | 53.9% | 492.1 MiB | 3.1% | 79.9 / 173.0 ms | 2.00 / 0.44 |
| 15:28:29–15:33:29 | 57 | 55.2% | 166.3 MiB | 52.3% | 519.6 MiB | 3.5% | 62.7 / 124.4 ms | 1.70 / 0.49 |
| 15:33:29–15:38:29 | 58 | 58.7% | 174.6 MiB | 48.8% | 533.9 MiB | 3.4% | 67.4 / 158.8 ms | 1.36 / 0.21 |
| 15:38:29–15:43:29 | 57 | 66.3% | 181.7 MiB | 52.0% | 547.1 MiB | 3.5% | 58.7 / 97.6 ms | 1.68 / 0.33 |
| 15:43:29–15:48:29 | 60 | 71.5% | 167.0 MiB | 47.2% | 552.1 MiB | 3.4% | 51.0 / 92.6 ms | 1.48 / 0.18 |
| 15:48:29–15:53:29 | 59 | 51.6% | 161.7 MiB | 36.3% | 556.8 MiB | 2.7% | 60.1 / 114.5 ms | 1.32 / 0.22 |

Sampling gaps averaged 5.181 seconds, with p50/p95/p99/max
5.001/6.219/7.991/10.639 seconds. Every included row remained available. The
observer's three `fleet-stats` diagnostics all occurred at `15:53:56.424 UTC`,
27.424 seconds after the player window, when its Docker inventory raced the
intentional fleet teardown. They are outside every metric above and are not event
health failures.

### Player-facing latency

These values summarize 100 independent schema-v9 clients. “Median p95” is the
median of per-team p95 values; “p95 of p95” shows the slower-team tail. KotH
capture and Jeopardy submit have 98 contributing clients.

| Operation | Mean average | Median p50 | Median p90 | Median p95 | P95 of p95 | Median p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| All HTTP | 33.2 ms | 6.3 ms | 49.6 ms | 134.7 ms | 189.2 ms | 528.3 ms | 5,033.9 ms |
| A&D state | 64.5 ms | 12.3 ms | 91.9 ms | 320.9 ms | 457.6 ms | 984.5 ms | 4,956.9 ms |
| A&D targets | 26.7 ms | 4.7 ms | 22.2 ms | 68.1 ms | 146.5 ms | 410.6 ms | 4,713.8 ms |
| A&D scoreboard | 31.8 ms | 5.0 ms | 35.0 ms | 139.9 ms | 237.9 ms | 514.6 ms | 3,847.2 ms |
| A&D submit | 75.2 ms | 24.0 ms | 168.7 ms | 278.0 ms | 456.6 ms | 539.3 ms | 3,544.5 ms |
| KotH scoreboard | 18.3 ms | 2.8 ms | 19.9 ms | 60.7 ms | 97.5 ms | 336.6 ms | 4,569.7 ms |
| KotH token | 25.1 ms | 4.5 ms | 35.1 ms | 115.5 ms | 162.8 ms | 406.5 ms | 4,438.7 ms |
| KotH state | 22.0 ms | 5.3 ms | 27.9 ms | 82.7 ms | 125.4 ms | 345.6 ms | 4,812.7 ms |
| KotH timeline | 17.3 ms | 2.3 ms | 27.0 ms | 48.6 ms | 106.1 ms | 266.0 ms | 2,552.1 ms |
| KotH capture | 2.9 ms | 2.5 ms | 3.3 ms | 3.4 ms | 8.3 ms | 3.5 ms | 51.5 ms |
| Jeopardy game | 34.3 ms | 8.7 ms | 60.6 ms | 168.4 ms | 234.4 ms | 454.3 ms | 3,980.7 ms |
| Jeopardy details | 27.9 ms | 5.9 ms | 51.7 ms | 130.6 ms | 197.6 ms | 382.1 ms | 4,228.6 ms |
| Jeopardy submit | 158.9 ms | 63.6 ms | 295.5 ms | 344.2 ms | 890.0 ms | 391.7 ms | 2,557.2 ms |
| VPN attack | 40.2 ms | 35.2 ms | 60.8 ms | 84.7 ms | 125.3 ms | 154.4 ms | 740.2 ms |

### Integrity, correction, and limitations

Every lifecycle gate passed: zero duplicate rounds, attacks, tokens, cycles,
controls, acquisitions, participations, runtime operations, or rollups; zero
overlapping cycles, stale-container/cross-cycle-token attribution, late evidence,
platform void penalties, leaked cooldowns, unfinished pipelines, unfinalized rounds,
or nonterminal cycles. The exact reset receipt query returned zero invalid chains.
The public authenticated profile smoke test returned 200 as a raw Admin object,
anonymous profile returned the expected 401, all three historical scoreboards returned
200, and A&D/KotH were fully settled. PostgreSQL had all 52/52 migrations.

Official10 was retained with an `aborted` manifest because its older player-side
check counted one already-repatched replacement as a pristine-reset failure. That run
observed 19 replacements before another patch and one afterward; its 41 backend
receipt chains were nevertheless valid. Commit `db880a8` separates replacement
identity from pristine state and adds the authoritative run-scoped receipt gate.
Official11 is the passing rerun of that exact correction. Historical retained
candidates are not rewritten.

This run does not add an optimization-ledger row: the deployed application image is
unchanged, and the measured change is a harness-evidence correction rather than an
application performance optimization. Raw observer CSVs and sanitized per-team JSON
remain outside Git under `/tmp/rsctf-competitive-v2-official11-100-observer` and
`/tmp/rsctf-team-event-evidence-competitive-v2-official11-100-eef8d03b-93c7-4bac-a55f-2b40715c09ee`.
The isolated services are deterministic fixtures rather than 100 distinct vulnerable
programs, each team has one automated player process, and the run uses one host, one
A&D service per team, and one shared hill. No host restart or network partition was
injected. The games, scoring evidence, and anti-cheat findings were visible when
this report was captured; the later authorized database reset removed them. The
100 relays, services, and clients were intentionally reaped after settlement.

## Official8 schema-v8 historical candidate — 15 July 2026

The platform completed and settled a full one-hour, 100-team competitive event on the
public TLS path. Application health, scoring, deadline cleanup, anti-cheat attribution,
and every post-run database integrity query passed. The lifecycle process itself exited
with status 1 after the deadline because its client-evidence validator required equality
between independent k6 counters emitted at different points in each workload path. The
bounded one-way tail in 43 summaries is consistent with hard-duration cancellation. The
retained manifest therefore
correctly remains `aborted`; this run is operational evidence, not a passing lifecycle
acceptance result.

The interim schema-v8 validator reported and bounded that completion tail at one percent
per team. Replaying the unchanged artifacts under that rule validates all 100 summaries
and every other conservation gate. Future schema-v9 runs do not use that percentage
tolerance: each one-VU client records caught runtime errors in both its counters and a
retained `runner.log`, and may leave at most one otherwise unexplained hard-stop tail.
Historical artifacts remain under their original schema and are not reclassified. A new
one-hour run has not yet been performed under the schema-v9 gate.

### Event identity and provenance

| Item | Value |
| --- | --- |
| Public path | `https://tcp.1pc.tf` through Traefik and TLS |
| Attack window | `2026-07-15 09:32:41–10:32:41 UTC` (half-open, exactly 3,600 seconds) |
| Settlement deadline | `10:32:56 UTC` after 15 seconds of grace |
| Historical game IDs | Jeopardy `105`; mixed A&D/KotH `106` |
| Challenges | A&D `503`; KotH `504` |
| Competition run | `15aaac26-abce-45c3-b7c2-54737beef9fa`; model v2; seed `rsctf-competitive-v2` |
| Workload | 100 clients, 100 authenticated WireGuard peers, 100 relays, 100 isolated services, one shared KotH hill |
| Player mix | 10 always-on, 25 committed, 45 part-time, 20 casual; five balanced 20-team specialties |
| Deployed image | `sha256:9e042251dd824257993a80652d423689a9c1d7d73594a8c2216dc1fb8cad0544` |
| Implementation commit | `28bf95dce6c8431e2a6f79225715a1cca8251eac` |
| Lifecycle result | Exit 1; retained manifest `aborted`; all run-owned workload containers reaped |

The observer started from base commit `a7a0e3c` plus a content-addressed dirty snapshot.
The implementation was committed at `10:14:47 UTC` at the user's request. The commit's
24-file binary diff and all 36 formerly untracked blobs exactly match the observer hashes,
so this was a Git-provenance transition without a source-byte change. Observer metadata
did not record the competition UUID; evidence is instead bound by the unique state tag,
event IDs, exact time window, and worktree hash.

### Real-player model and result

The deterministic model made decisions from each player's private profile and public game
state. Players had finite per-round action credits, independent active/idle sessions,
different domain skills, rival memory, patch decisions, and discovery timing. It models a
competitive event reproducibly; it does not model human collaboration or novel exploit
development.

| Signal | Result |
| --- | ---: |
| Team summaries | 100/100 present; 0 malformed; 0 threshold failures |
| HTTPS/VPN requests | 347,992 |
| Server 5xx / unexpected response / timeout / rate limit | 0 / 0 / 0 / 0 |
| Platform retries / final VPN failures | 0 / 0 |
| Classified / work-completion samples | 48,058 / 47,978; completion tail 80 total, maximum 5/team |
| Active / idle classifications | 38,434 / 9,624 |
| A&D exploit attempts | 13,115: 9,257 flags, 3,660 patched, 198 unavailable |
| A&D submissions | 9,257 logical: 9,253 accepted, 4 terminal, 0 duplicate/replay/unresolved |
| Defense activity | 460 updates; 55 incidents; 54 repairs |
| Action budget | 16,061 credits spent; 79 denied decisions |
| Jeopardy activity | 317 solves, 179 wrong guesses, 40 attachment downloads, 33 complete container journeys |
| KotH writes | 758 attempts; 756 successes; 638 openings; 118 takeovers; 2 classified HTTP 4xx |
| KotH patches | 28 attempts; 15 applied; 2 repairs; 12 healthy holds; 15/15 replacement-observed patch losses |
| KotH patch contention | 1 blocked takeover; 7 bypassed takeovers; 4 interrupted holds; 0 status-proof failures |

The four terminal A&D verdicts reconcile the 9,257 discovered flags with 9,253 durable
accepted captures. There were no prior-round captures, delivery failures, semantic-invalid
responses, unresolved captures, stale-target writes, or final transport failures.
Inside the exact one-hour window, 9,249 captures came from all 100 attackers against 98
victims across 1,833 attacker-victim pairs; four more captures settled during grace.
Attacker totals ranged from 4 to 413 with a coefficient of variation of 0.866, preserving
the intended nonuniform competition.

### Official scoring and lifecycle evidence

KotH used the fixed formula with snapshotted 12-tick epochs, 3-tick crown cycles, one
cooldown tick, two confirmation ticks, a 100-team roster, and one hill. All scoreboards
return HTTP 200 after the deadline.

| Board | Settled result |
| --- | --- |
| A&D | `fullySettled=true`; 100 teams; 100 distinct scores; 37.999704–43.315034; ranks 1–100 valid |
| KotH | `fullySettled=true`; 100 teams; 46 nonzero; 10 primary values; 0–3.968248; secondary comparator and ranks 1–100 valid |
| Jeopardy | 92 solvers; 317 solves; 49 score values; 0–2,052; score/time/team comparator valid |

The corresponding specialty lifts were 1.521× offense, 1.007× defense, 1.311× KotH,
and 1.757× Jeopardy. A&D finalized 121 rounds and 16 epochs, including a final 1/8-weight
partial epoch. Round 1 was intentionally frozen during readiness; rounds 2–121 kept the
exact 30-second cadence. KotH finalized 11 score epochs with a final 1/12-weight epoch.

KotH produced 41/41 completed crown cycles, 121 control ticks, 22 interrupted provisional
claims, and 13 stable confirmations/acquisitions. Thirteen champion cooldowns lasted
exactly one authoritative tick and all were released. All 4,100 window tokens were
revoked. Deadline cleanup converged with one immutable `DeadlineSnapshot` and one
`DeadlineCleanup` receipt; target, claim, token, cooldown, container, and shared-runtime
leak counts are all zero. A&D publication lag was p95 4.931 seconds and max 4.961 seconds.

The final read-only audit ran 41 lifecycle/integrity checks. Every result was zero,
including duplicate rounds, attacks, cycles, control ticks, acquisitions, rollups,
participations, and suspicion evidence; stale-container and cross-cycle-token evidence;
self-captures; late/post-deadline evidence; overdue pipelines; unfinalized rounds; and
nonterminal cycles.

### Integrated anti-cheat drill

The drill ran during normal play at 45 percent progress. Its run-bound schema-v3 artifact
contains exactly six intended offenders and 94 clean controls, with all 14 semantic
integrity checks true. It exercised four stolen-flag actors, 40 distinct brute-force wrong
answers, and three honeypot baits. The checkpoint contained six suspicion rows and no
clean-control context finding. At capture time, the post-run database contained 12
immutable events on the same six offenders, a reconciled suspicion score total of 730,
zero duplicate evidence, and zero actionable clean-control finding.

### Independent health and resource observer

The exact attack window contains 685 synchronized waves. The first and last included
samples were `09:32:46.243` and `10:32:40.099`; sampling-gap p50/p95/max was
5.001/6.311/13.713 seconds. Percentiles use R-7 linear interpolation.

| Endpoint | Success | Mean | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Public liveness | 685/685 | 70.0 ms | 58.0 ms | 145.2 ms | 189.1 ms | 563.6 ms |
| Local liveness | 685/685 | 6.5 ms | 2.2 ms | 30.4 ms | 67.9 ms | 206.0 ms |
| Local readiness | 685/685 | 8.7 ms | 2.6 ms | 45.0 ms | 127.1 ms | 194.1 ms |

Docker CPU uses 100 percent for one logical CPU. One shared `docker stats` collection
failed at `09:59:59`; health, fleet, PostgreSQL, Redis, and container-count collection
continued. Each Docker role therefore has 684 available and one unavailable sample.

| Component | CPU mean / p50 / p95 / max | RAM mean / p50 / p95 / max |
| --- | ---: | ---: |
| RSCTF | 71.3 / 30.3 / 364.8 / 445.9% | 158.9 / 137.8 / 441.1 / 558.6 MiB |
| PostgreSQL | 57.1 / 56.9 / 96.9 / 135.3% | 554.8 / 564.6 / 621.5 / 625.1 MiB |
| Redis | 3.9 / 3.6 / 6.6 / 25.8% | 7.4 / 7.4 / 7.6 / 9.4 MiB |
| Traefik | 15.9 / 15.4 / 24.6 / 41.2% | 152.6 / 145.2 / 265.4 / 277.7 MiB |
| 100 relays | 4.1 / 3.0 / 10.9 / 16.3% | 404.6 / 420.5 / 437.8 / 446.4 MiB |
| 100 services | 2.6 / 2.3 / 6.0 / 8.2% | 941.1 / 941.0 / 943.4 / 943.7 MiB |
| 100 clients | 114.1 / 112.3 / 145.3 / 276.2% | 2,091.4 / 2,112.6 / 2,282.3 / 2,320.6 MiB |

All 685 fleet samples contained exactly 100 clients, 100 relays, and 100 isolated services.
Combined fleet CPU p50/p95/max was 118.7/152.4/285.3 percent; RAM was
3,471.8/3,662.8/3,691.5 MiB. Managed challenge-container churn changed the host's total
running-container count from 318 to 339 without changing the fixed cohorts.

| UTC interval | n | App CPU mean | App RAM mean | PG CPU mean | PG RAM mean | Public mean / p95 | DB active / waiting mean |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 09:32:41–09:37:41 | 59 | 65.2% | 121.5 MiB | 45.7% | 497.6 MiB | 58.5 / 109.2 ms | 1.37 / 0.19 |
| 09:37:41–09:42:41 | 57 | 75.4% | 131.1 MiB | 61.0% | 493.2 MiB | 71.1 / 150.9 ms | 1.51 / 0.33 |
| 09:42:41–09:47:41 | 58 | 79.3% | 170.2 MiB | 63.5% | 484.8 MiB | 76.8 / 159.6 ms | 1.69 / 0.45 |
| 09:47:41–09:52:41 | 56 | 66.7% | 142.5 MiB | 64.0% | 505.1 MiB | 82.8 / 159.7 ms | 1.66 / 0.43 |
| 09:52:41–09:57:41 | 57 | 69.0% | 166.3 MiB | 64.7% | 542.3 MiB | 77.7 / 131.9 ms | 1.72 / 0.47 |
| 09:57:41–10:02:41 | 54 | 80.1% | 182.6 MiB | 65.1% | 569.3 MiB | 78.1 / 154.8 ms | 1.65 / 0.46 |
| 10:02:41–10:07:41 | 58 | 75.7% | 178.6 MiB | 60.3% | 589.5 MiB | 67.3 / 119.1 ms | 1.52 / 0.36 |
| 10:07:41–10:12:41 | 55 | 74.3% | 162.5 MiB | 59.1% | 587.4 MiB | 70.9 / 133.2 ms | 1.45 / 0.35 |
| 10:12:41–10:17:41 | 57 | 78.4% | 171.1 MiB | 58.4% | 579.7 MiB | 68.1 / 125.4 ms | 1.86 / 0.37 |
| 10:17:41–10:22:41 | 57 | 65.3% | 160.6 MiB | 58.2% | 577.2 MiB | 71.6 / 139.0 ms | 1.47 / 0.37 |
| 10:22:41–10:27:41 | 59 | 72.5% | 168.2 MiB | 48.5% | 610.9 MiB | 63.5 / 111.3 ms | 1.51 / 0.31 |
| 10:27:41–10:32:41 | 58 | 54.3% | 154.0 MiB | 38.7% | 621.3 MiB | 54.9 / 100.5 ms | 1.28 / 0.33 |

### Player-facing latency

These are summaries across 100 independent clients. `Median p50/p90/p95/p99` is the
median of the per-team percentile; `p95 of p95` describes the slower-team tail. KotH
capture has 99 contributing clients.

| Operation | Mean average | Median p50 | Median p90 | Median p95 | P95 of p95 | Median p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| All HTTP | 27.2 ms | 8.0 ms | 52.9 ms | 111.9 ms | 137.5 ms | 356.4 ms | 3,180.5 ms |
| A&D state | 47.1 ms | 14.5 ms | 98.2 ms | 234.6 ms | 313.6 ms | 529.1 ms | 3,180.5 ms |
| A&D targets | 19.5 ms | 6.0 ms | 28.5 ms | 65.1 ms | 88.2 ms | 235.4 ms | 2,383.4 ms |
| A&D scoreboard | 22.8 ms | 6.3 ms | 44.8 ms | 112.3 ms | 138.9 ms | 285.8 ms | 2,831.8 ms |
| KotH scoreboard | 15.0 ms | 3.5 ms | 24.2 ms | 54.8 ms | 79.9 ms | 209.2 ms | 2,496.4 ms |
| KotH token | 20.1 ms | 5.7 ms | 41.7 ms | 87.6 ms | 121.9 ms | 247.4 ms | 2,266.3 ms |
| KotH state | 18.7 ms | 6.7 ms | 35.3 ms | 69.7 ms | 91.1 ms | 213.0 ms | 2,337.8 ms |
| KotH timeline | 15.1 ms | 2.9 ms | 31.4 ms | 52.3 ms | 86.9 ms | 163.7 ms | 2,468.6 ms |
| Jeopardy game | 32.5 ms | 11.2 ms | 66.9 ms | 143.7 ms | 185.0 ms | 347.4 ms | 2,203.9 ms |
| Jeopardy details | 26.1 ms | 7.6 ms | 54.6 ms | 114.2 ms | 161.3 ms | 322.3 ms | 1,967.9 ms |
| Jeopardy submit | 161.5 ms | 72.9 ms | 268.0 ms | 302.3 ms | 843.3 ms | 319.6 ms | 1,617.5 ms |
| VPN attack | 39.6 ms | 36.1 ms | 64.8 ms | 80.9 ms | 106.5 ms | 129.2 ms | 343.0 ms |
| A&D submit | 71.1 ms | 27.6 ms | 163.8 ms | 262.2 ms | 439.4 ms | 439.9 ms | 2,272.0 ms |
| KotH capture | 5.3 ms | 3.1 ms | 5.4 ms | 6.0 ms | 43.1 ms | 6.6 ms | 146.2 ms |

### PostgreSQL and Redis

PostgreSQL had 33 connections at p50/p95 and 34 maximum; active connections averaged
1.56 (p95 3, max 8), waiting connections averaged 0.37 (p95 2, max 5), and waiting
locks stayed zero. The longest transaction was p50/p95/max 0.018/1.737/3.710 seconds.
The sampled counter deltas were 1,208,060 commits, three rollbacks, zero new deadlocks or
temporary files, zero block reads, and 733,490,597 cache hits. Database size grew 9.906
MiB.

Redis used p50/p95/max 1.742/1.887/2.156 MiB, with at most 293 keys. It remained capped
at 256 MiB with `allkeys-lru`. The window added 57,942 hits and 420,305 misses; evictions,
rejected connections, and new error replies remained zero.

### Harness defect, correction, and limitations

The old equality compared three independently flushed counters. Active classification is
emitted near the start of active work, idle classification near the end of the idle path,
and the custom completion sample after workload evidence is recorded but before the final
think delay and return. Official8 had a one-way difference of 80 samples across 43 teams,
at most five for any team (below one percent). That shape is consistent with cancellation
at the hard duration, but the aggregate counters cannot prove the cause of each missing
sample. Applying the bounded tolerance made all remaining A&D, KotH, patch, retry,
action-budget, and workload conservation checks pass for all 100 retained summaries.

That interim schema-v8 validator rejected completion greater than classification and
bounded the tail at `max(1, ceil(classified_iterations / 100))`. Schema v9 supersedes that
percentage rule: caught runtime errors are explicit zero-tolerance evidence, and each
single-VU client may leave at most one otherwise unexplained hard-stop tail. Regression
tests cover exact conservation, one allowed tail, two rejected tails, caught runtime
errors, and the fleet sum without reclassifying historical artifacts.

The load-test tree was also simplified without moving runtime code: all 22 Node test files
live in `tests/load/test/`, and the test-only process worker lives in `test/fixtures/`.
At that checkpoint, the npm script discovered `test/*.test.mjs` and all 133 tests
passed.

This run does not add a performance-ledger row because there is no same-shape before/after
optimization comparison. The remaining limitations are the aborted manifest, lack of a
post-fix one-hour rerun, one in-window Docker collector failure, the observer's missing run
UUID field, and the deterministic model's inability to reproduce human strategy.

## Controlled anti-cheat drill — 14 July 2026

This drill used the historical 100-team mixed A&D/KotH event (`game_id=34`) and its
dedicated audit challenge (`challenge_id=114`); the associated 20-team Jeopardy event
was `game_id=33`. The namespace was kept for operator review at capture time and was
later removed by the authorized PostgreSQL reset.

### First drill and shared-NAT finding

The first HTTP phase completed all 329 expected checks. It produced four stolen-flag
submissions, 40 rapid wrong submissions from five accounts on one team, three
authenticated honeypot probes from another team, and 282 normal A&D/KotH polls from the
clean controls. There were zero unexpected responses and zero server 5xx responses;
request latency was p95 208.61 ms with a 285.05 ms maximum.

The subsequent report sweep exposed a detector-quality issue before the drill could be
marked complete. The public load generator's shared gateway address appeared on 48
teams and created 48 `CrossTeamIP` context events, including 43 clean controls. These
rows were non-actionable: `CrossTeamIP` is context-tier evidence, the affected clean
teams stayed in the `context` band, and no hard or strong signal accused them of
cheating. The rows were still noisy and added no useful attribution in a campus,
CGNAT, or event-gateway setting, so the acceptance gate correctly paused the run.

The correlation detector now emits shared-IP, shared-/28, and clustered-registration
context only when the address identifies two to four teams. Addresses spanning five or
more teams are suppressed as broad shared-network noise. A boundary unit test covers
zero, one, two, four, five, and 100 teams. The 48 synthetic `CrossTeamIP` rows and only
their corresponding score increments were removed from the retained test event before
the rerun; the four `StolenFlag` events and the `HighWrongRate`, `AutomatedPattern`,
`HoneypotHit`, and `HoneypotChain` evidence remained available for review until the
later database reset. The initial finding was retained in the lifecycle metadata rather
than being hidden by the cleanup.

### Corrected anti-cheat rerun

The final rerun passed all 329 expected checks with zero unexpected responses and zero
server 5xx responses. All 40/40 brute-force attempts were accepted for analysis—eight
from each of five accounts on one team—alongside four stolen-flag submissions, three
honeypot hits, and 282 clean-control polls. Request latency averaged 69.18 ms, with
p95 179.56 ms, p99 216.45 ms, and a 269.25 ms maximum.

The corrected-drill checkpoint contains exactly eight intended, unique evidence rows: four
`StolenFlag` rows and one each for `HighWrongRate`, `AutomatedPattern`, `HoneypotHit`,
and `HoneypotChain`. The four stolen-flag actors are in the `evidenced` band; the
brute-force and honeypot actors are in the `investigate` band. All 94 clean controls
have no actionable finding, duplicate evidence is zero, and every participation's
suspicion score reconciles with its persisted evidence.

An intermediate rerun returned 12 legitimate HTTP 429 responses and exposed a harness
actor-allocation bug, not a detector or application failure: several workers could use
the same account token and exceed its per-account burst limit. Token assignment now
uses the scenario iteration, and fixture-only bot security stamps rotate before a run,
keeping each account in a fresh authenticated limiter partition. The harness also
records brute-force attempts, accepted attempts, and rate-limited attempts separately.
The finalized harness also rotates reruns onto participants without prior actionable
evidence and validates exact actors, answers, rows, and detector evidence against
pre-run submission, honeypot, and suspicion-event baselines. Retained findings therefore
cannot satisfy a future allocation or detector-regression gate.

### Historical one-hour 100-team lifecycle

The retained-event lifecycle ran from `12:01:46` through `13:01:46 UTC` on 14 July
2026, followed by a 60-second settlement grace through `13:02:46`. It used deployed
image `sha256:d7e40101aec471fdf37938c917a0adf62b7613dbeccca54ca4e51425f749a2b3`.
Games 33 and 34, including audit challenge 114, were available for review before
the later authorized database reset.

All 100/100 clients completed and produced 100 valid sanitized summaries, with zero
threshold failures and zero malformed summaries. They completed 540,526 HTTPS/VPN
requests and 3,496 bounded flag-synchronization waits. The lifecycle driver recorded
5,592/5,592 successful liveness and readiness probes, with p95 16 ms and a 317 ms
maximum.

A&D accepted 12,000/12,000 captures across 120 complete rounds, covering all 100
attackers, all 100 victims, and 9,900 distinct attacker-victim pairs. Flag-publication
lag was p95 6.572 seconds and max 6.982 seconds. KotH produced 665 marker writes, 54
completed crown cycles, 39 confirmed acquisitions, 87 control rows with a king, and
rejected the sampled stale capability 1/1 times. Both official boards settled, and
every integrity check returned zero within the measured evidence window.

A post-lifecycle report sweep added two `CrossTeamIP` and two `SubnetOverlap` context
rows to two actors that already had controlled hard/strong evidence. It did not flag a
clean control, add an actionable band, or change the six detected actors. The database
therefore held 12 raw evidence rows across those same six participants at capture
time; the eight-row count above is the recorded corrected-drill checkpoint.

The first harness result reported one cadence drift. Inspection showed that it was
round 30's 107.825873-second gap, from `11:54:30` to `11:56:18`, instead of the
configured 30 seconds. That gap occurred during the intentional pre-run image
deployment, before lifecycle evidence began at round 40. Rounds 40–162 had zero cadence
drift. The harness query now scopes this check to `evidenceStartRound`, matching the
window already used by its other evidence gates. This is a diagnostic-scope correction,
not a claim that the earlier deployment pause did not occur.

The independent observer recorded 360 ten-second samples during the exact attack
window. Latencies below are `min / median / p95 / max`:

| Health path | Success | Latency (ms) |
| --- | ---: | ---: |
| Public liveness | 360/360 | 27.041 / 50.496 / 152.812 / 1,055.731 |
| Local liveness | 360/360 | 0.617 / 1.736 / 15.774 / 270.412 |
| Local readiness | 360/360 | 0.655 / 1.903 / 21.073 / 307.664 |

The same samples give these `min / median / p95 / max` resource distributions. Docker
CPU uses 100% for one logical CPU; memory is MiB.

| Component | CPU (%) | RAM (MiB) |
| --- | ---: | ---: |
| RSCTF | 5.730 / 31.485 / 283.524 / 414.320 | 74.400 / 92.635 / 456.465 / 530.300 |
| PostgreSQL | 1.790 / 35.375 / 83.392 / 113.410 | 166.900 / 410.500 / 426.915 / 441.200 |
| Redis | 0.480 / 3.055 / 5.325 / 9.260 | 4.273 / 4.633 / 4.786 / 6.102 |

Across the observer's full `11:58:21–13:12:48 UTC` span, public liveness, local
liveness, and readiness each passed 447/447 probes. Its only collector errors occurred
at `13:02:52`, after settlement, while the workload containers were being cleaned up:
one Docker-stats and three fleet-stats errors. Health sampling continued successfully.

## One-hour, 100-team remediation validation — 14 July 2026

The remediated build passed the complete one-hour lifecycle gate. All 100 distributed
team clients finished, both official scoreboards settled, and every hard client,
health, deadline, and database-integrity check passed. The 534,575 recorded HTTPS and
VPN requests contained no server 5xx response, platform API failure, request timeout,
rate-limit response, semantic validation failure, or final VPN failure. Sixty-two VPN
target operations needed one retry and then succeeded.

This run replaced the 13 July result as the 100-team operational baseline at the
time. The newer 16 July fixed-rate campaign above supersedes it for current hot-path
CPU and latency comparisons. The
older run remains below as historical evidence of the faults that motivated the work.

### Event shape and exact measurement window

| Item | Value |
| --- | --- |
| Public path | `https://tcp.1pc.tf` through Traefik and TLS |
| Attack window | `2026-07-14 07:57:08–08:57:08 UTC` (exactly 3,600 seconds) |
| Settlement grace | 60 seconds, ending at `08:58:08 UTC` |
| Observer span | `07:54:08–08:58:39 UTC`; 387 ten-second samples |
| Host | 8 logical CPUs, 31.3 GiB RAM, Docker Compose |
| A&D/KotH roster | 100 accepted teams; one A&D challenge and one shared KotH hill |
| Jeopardy roster | 20 teams, exercised by the first 20 distributed clients |
| Team clients | 100 independent containers, distinct trusted-proxy source addresses, and 100 authenticated WireGuard peers |
| BYOC path | 100 relay containers and 100 isolated flag-service containers |
| KotH path | One real `nginx:alpine` hill, replaced from the same image at crown-cycle boundaries |
| Client pacing | One closed-loop client per team with a five-second think time |
| Deployed image | `sha256:4f96e352bb97f8bedbf7a441bda5e9620b626e605754d52f8c87ea79062768fd` |
| Observer source revision | `f285b33be166b09c9c0d49bf926adf5dc1265aae` plus the tested remediation worktree |

All 300 workload containers—100 clients, 100 relays, and 100 services—were present in
every one of the 360 attack-window observations. Provisioning froze an official roster
of 100 teams and 100 services, then required a post-connect round in which all 100 flags
had durable delivery and successful checker evidence. The clients traversed the public
TLS proxy, used real WireGuard peers, attacked team-specific service containers,
submitted flags through the production API, and contended for a real managed KotH
container. The workload therefore exercised the normal schedulers and score engines,
not a synthetic score insertion path.

### Result summary

| Signal | Result |
| --- | ---: |
| Distributed clients | 100/100 completed; 100/100 sanitized summaries; 0 threshold failures |
| HTTPS/VPN requests | 534,575 |
| Server 5xx / platform failures / timeouts | 0 / 0 / 0 |
| Rate-limited / semantic-invalid responses | 0 / 0 |
| VPN target operations | 64,384; 62 first-attempt retries (0.0963%); 0 final failures |
| A&D captures | 12,100/12,100 accepted |
| A&D rounds | 121 complete / 121 observed; 0 cadence drift |
| A&D coverage | 100/100 attackers, 100/100 victims, 9,900 distinct attacker-victim pairs |
| Flag synchronization | 3,672 bounded waits; 0 selected-team delivery failures |
| A&D publication lag | p95 6.805 s; max 7.002 s, below the 8 s / 12 s gates |
| KotH exercise | 641 marker writes; 43 completed cycles; 40 confirmed acquisitions; 88 controlled results with a king |
| Stale KotH capability | 1/1 rejected after reset |
| Settlement | A&D `true`; KotH `true`; 0 unfinalized rounds; 0 nonterminal cycles |
| Lifecycle probes | 5,403/5,403 liveness and 5,403/5,403 readiness successful; liveness p95 20 ms, max 610 ms |
| Independent observer | 387/387 public, 387/387 local, and 387/387 readiness probes successful |

Across the exact attack window, the independent public probe was 360/360 successful
with p50 59.7 ms, p95 158.6 ms, p99 253.0 ms, and max 1,326.0 ms. Local liveness was
360/360 successful with p50 2.3 ms and p95 54.5 ms; readiness was 360/360 successful
with p50 2.8 ms and p95 48.9 ms. Three fleet-stat collector errors occurred together at
`08:58:31`, after the scoring deadline while 148 workload containers were being reaped;
they did not affect the attack-window series or any health probe.

### Player-facing latency

The evidence consists of 100 independent k6 summaries. `Median p50`, `median p90`,
`median p95`, and `median p99` are medians of those teams' respective percentiles;
`p95 of p95` describes the slow-team tail. These are not percentiles from one merged
request histogram. Jeopardy rows contain 20 team histograms; every other row contains
100.

| Operation | Mean average | Median p50 | Median p90 | Median p95 | P95 of p95 | Median p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| All HTTP | 53.4 ms | 12.6 ms | 103.7 ms | 225.4 ms | 255.8 ms | 717.6 ms | 5,002.1 ms |
| A&D state | 113.1 ms | 23.3 ms | 240.1 ms | 587.1 ms | 783.7 ms | 1,360.0 ms | 4,186.2 ms |
| A&D targets | 31.2 ms | 7.6 ms | 52.8 ms | 111.6 ms | 148.7 ms | 401.9 ms | 2,544.0 ms |
| A&D scoreboard | 45.9 ms | 9.2 ms | 97.8 ms | 225.0 ms | 291.3 ms | 566.1 ms | 1,974.2 ms |
| KotH scoreboard | 36.3 ms | 5.3 ms | 72.2 ms | 156.2 ms | 225.0 ms | 529.5 ms | 3,092.5 ms |
| KotH token | 41.0 ms | 9.0 ms | 97.6 ms | 201.5 ms | 244.9 ms | 475.8 ms | 2,383.3 ms |
| KotH state | 38.8 ms | 9.8 ms | 92.9 ms | 177.2 ms | 211.1 ms | 433.8 ms | 2,384.5 ms |
| KotH timeline | 33.1 ms | 4.7 ms | 69.3 ms | 162.0 ms | 250.5 ms | 510.8 ms | 1,927.4 ms |
| Jeopardy game | 53.5 ms | 27.2 ms | 125.0 ms | 195.4 ms | 210.1 ms | 421.1 ms | 1,479.4 ms |
| Jeopardy details | 48.4 ms | 21.1 ms | 114.3 ms | 188.7 ms | 225.0 ms | 445.0 ms | 1,552.5 ms |
| A&D submit | 226.8 ms | 152.2 ms | 586.4 ms | 788.4 ms | 1,091.1 ms | 1,308.8 ms | 2,479.0 ms |
| VPN attack | 40.2 ms | 38.6 ms | 62.0 ms | 79.5 ms | 90.7 ms | 143.6 ms | 4,157.3 ms |

### Fixed-shape five-minute gate: before and after

The failed diagnostic gate and the passing rerun used the same public target, 100 team
clients, 100 relays, 100 isolated services, five-minute duration, and five-second client
think time. This is the direct before/after evidence for the A&D scoreboard
stale-while-revalidate, single-flight, and revision-fenced invalidation work.

| Signal | Failed gate, 14 July 05:55 | Passing gate, 14 July 07:45 | Change |
| --- | ---: | ---: | ---: |
| Requests completed | 34,385 | 42,728 | +24.3% |
| All-HTTP mean of team averages | 305.9 ms | 111.2 ms | -63.6% |
| All-HTTP median team p95 | 1,194.6 ms | 531.7 ms | -55.5% |
| A&D scoreboard mean of team averages | 1,129.6 ms | 84.7 ms | -92.5% |
| A&D scoreboard median team p50 | 421.4 ms | 15.3 ms | -96.4% |
| A&D scoreboard median team p95 | 4,846.2 ms | 334.1 ms | -93.1% |
| A&D scoreboard p95 of team p95 | 4,948.1 ms | 579.7 ms | -88.3% |
| Platform timeouts / non-2xx | 372 | 0 | eliminated |
| Team threshold failures | 100/100 | 0/100 | eliminated |
| Server 5xx | 0 | 0 | unchanged |
| A&D / KotH settled | yes / yes | yes / yes | preserved |
| Duplicate/integrity failures | 0 | 0 | preserved |

The proxy's independent view of the failed window recorded 4,345 A&D scoreboard calls,
with median 482.1 ms, p95 5,001.7 ms, 321 calls at or above five seconds, and 279 client-
closed HTTP 499 responses. The passing gate did not archive an equivalent proxy
histogram, so that diagnostic is corroborating evidence only; the table compares the
same sanitized team-summary aggregation on both sides.

### Resource profile

Docker CPU uses 100% for one logical CPU. Values below use the 360 observations in the
exact attack window. Fleet rows are aggregate values for all 100 containers of that
kind.

| Component | CPU average | CPU p95 | CPU max | RAM average | RAM max |
| --- | ---: | ---: | ---: | ---: | ---: |
| RSCTF | 66.9% | 292.1% | 443.2% | 129.0 MiB | 527.0 MiB |
| PostgreSQL | 42.6% | 96.1% | 150.5% | 391.7 MiB | 431.1 MiB |
| Redis | 3.3% | 6.8% | 11.3% | 4.9 MiB | 6.6 MiB |
| Traefik | 17.1% | 39.8% | 125.0% | 124.9 MiB | 168.2 MiB |
| 100 relay containers | 8.1% | 15.4% | 28.0% | 557.3 MiB | 587.1 MiB |
| 100 isolated services | 4.6% | 8.7% | 22.7% | 931.9 MiB | 934.1 MiB |
| 100 attack clients | 110.2% | 155.9% | 603.2% | 1,672.3 MiB | 1,857.0 MiB |

PostgreSQL averaged 34 connections but only 1.31 active and 0.33 waiting; active p95/max
was 3/9 and waiting p95/max was 2/6. It recorded no new rollback, deadlock, or temporary
file and grew by 7.80 MiB. Redis data use averaged 1.53 MiB and peaked at 1.65 MiB, with
zero evictions, rejected connections, or new error replies.

Five-minute arithmetic means show bounded memory and no accumulating health loss:

| UTC interval | Samples | App CPU | App RAM | PostgreSQL CPU | PostgreSQL RAM | Public health | Avg / p95 | DB active / waiting |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 07:57:08–08:02:08 | 30 | 62.2% | 109.9 MiB | 40.7% | 387.8 MiB | 30/30 | 99.1 / 139.4 ms | 1.2 / 0.2 |
| 08:02:08–08:07:08 | 30 | 68.6% | 131.2 MiB | 43.0% | 401.7 MiB | 30/30 | 56.4 / 90.4 ms | 1.1 / 0.3 |
| 08:07:08–08:12:08 | 30 | 71.3% | 128.4 MiB | 39.4% | 400.3 MiB | 30/30 | 67.2 / 134.9 ms | 1.4 / 0.3 |
| 08:12:08–08:17:08 | 30 | 54.7% | 112.6 MiB | 42.2% | 357.5 MiB | 30/30 | 69.1 / 192.0 ms | 1.2 / 0.4 |
| 08:17:08–08:22:08 | 30 | 89.8% | 168.2 MiB | 41.6% | 374.1 MiB | 30/30 | 64.3 / 115.4 ms | 1.1 / 0.2 |
| 08:22:08–08:27:08 | 30 | 63.8% | 122.7 MiB | 46.2% | 402.8 MiB | 30/30 | 84.7 / 222.7 ms | 1.2 / 0.4 |
| 08:27:08–08:32:08 | 30 | 60.5% | 143.7 MiB | 45.2% | 413.5 MiB | 30/30 | 77.5 / 206.2 ms | 1.5 / 0.3 |
| 08:32:08–08:37:08 | 30 | 65.3% | 136.8 MiB | 41.9% | 417.3 MiB | 30/30 | 64.0 / 125.9 ms | 1.3 / 0.3 |
| 08:37:08–08:42:08 | 30 | 57.0% | 124.2 MiB | 40.3% | 404.8 MiB | 30/30 | 84.5 / 188.0 ms | 1.3 / 0.3 |
| 08:42:08–08:47:08 | 30 | 86.5% | 129.8 MiB | 45.7% | 361.5 MiB | 30/30 | 87.3 / 193.1 ms | 1.8 / 0.6 |
| 08:47:08–08:52:08 | 30 | 53.0% | 96.8 MiB | 47.8% | 377.3 MiB | 30/30 | 87.2 / 222.8 ms | 1.3 / 0.5 |
| 08:52:08–08:57:08 | 30 | 70.0% | 144.4 MiB | 36.8% | 401.5 MiB | 30/30 | 70.3 / 129.8 ms | 1.4 / 0.1 |

### Scoring, settlement, and integrity

The official A&D engine finalized all 121 observed rounds. Every round had one accepted
capture per team, both attacker and victim coverage reached 100/100, and no evidence was
accepted after the event deadline. KotH finalized 43 crown cycles and 40 confirmed
acquisitions, ended with no nonterminal reset state, and rejected the sampled pre-reset
capability after replacement. Both scoreboards reported `fullySettled=true`.

All duplicate checks returned zero: A&D rounds and attacks; KotH cycles, control ticks,
acquisitions, and tokens; runtime operations; participations; and A&D/KotH rollups. The
run also found zero overlapping active cycles, stale-container attribution, cross-cycle
token evidence, unbound scorable control, scorable platform voids, invalid cooldown
windows, retained former holders, late scoring evidence, self-captures, unpublished
flags, overdue pipelines, or post-deadline attacks. No process panic occurred.

### Evidence limitations

- The clients used a closed-loop five-second think time. Faster responses therefore
  increase completed work, so this run is an operational lifecycle validation, not a
  fixed-arrival-rate capacity benchmark.
- Endpoint percentiles are calculated from per-team histograms. Without raw merged
  samples, they cannot be interpreted as global request percentiles.
- The remediation build bundled scheduler, cache, transaction, cleanup, and KotH fixes.
  The matched five-minute gate isolates the scoreboard symptom closely enough to show
  the regression is gone, but the one-hour resource change cannot be assigned to one
  patch.
- Services were real isolated containers running a deterministic flag fixture, not 100
  distinct vulnerable applications or arbitrary exploit payloads.
- Each team had one automated client. The run did not model multiple simultaneous human
  browsers per team, a host restart, a network partition, or multiple RSCTF replicas.
- One A&D service and one shared KotH hill were used on one eight-core host. This does
  not establish a safe admission limit for a larger or multi-service event.
- Sanitized summaries and aggregate observer CSVs were retained outside Git; raw
  credentials, VPN configurations, capabilities, marker tokens, and unsanitized logs
  are deliberately excluded.

### Cleanup and reproduction

After evidence collection, teardown removed games 31 and 32 and all load clients,
relays, isolated services, and managed hill containers. A post-cleanup check found zero
remaining workload containers, games, orphan checker rows, or orphan automation tokens.
The normal RSCTF, PostgreSQL, Redis, and proxy services remained running, and public
root/config plus local liveness/readiness smoke endpoints all returned HTTP 200. The
sanitized archive contains 115 checksummed files and passed its secret scan.

Use a dedicated host or namespace. The lifecycle state contains live capabilities and
is intentionally gitignored.

```sh
cd tests/load
TEAMS_JEO=20 TEAMS_AD=100 CH_STATIC=4 \
  EVENT_DURATION_SECONDS=10800 npm run provision
```

Start the observer in a second terminal before the lifecycle run:

```sh
cd tests/load
OUT_DIR=/tmp/rsctf-final-observer TARGET=https://tcp.1pc.tf \
  INTERVAL_SECONDS=10 npm run observe
```

Run the distributed event in the first terminal:

```sh
TARGET=https://tcp.1pc.tf VUS=100 FLEET=100 DURATION=1h KEEP=1 \
  DISTRIBUTED_TEAM_CLIENTS=1 LIFECYCLE_ISOLATED_SERVICES=1 \
  REQUIRE_ISOLATED_SERVICES=1 TEAM_THINK_SECONDS=5 \
  TEAM_START_DELAY_SECONDS=90 EVENT_END_GRACE_SECONDS=60 \
  EVENT_SETTLEMENT_TIMEOUT_SECONDS=240 AD_MIN_ATTACK_ROUNDS=40 \
  CROWN_MIN_ACQUISITIONS=1 CROWN_MIN_COMPLETED=1 \
  CROWN_MIN_STALE_REJECTIONS=1 npm run lifecycle

npm run teardown
```

Stop the observer after settlement and cleanup. A valid run requires all distributed
team thresholds, both scoreboards, health probes, deadlines, coverage checks, and
duplicate/integrity SQL checks to pass.

## Historical diagnostic baseline — 13 July 2026

RSCTF remained reachable throughout this run, preserved its duplicate and token
integrity invariants, and completed the A&D scoreboard. It is not yet ready for a
high-stakes 100-team A&D/KotH event, however. Four event-critical problems need to be
fixed first: scoring-round drift, disruptive VPN reconciliation, intermittent scoreboard
500 responses, and a KotH transition failure that leaves the official board unsettled.

This is a diagnostic baseline, not an optimization comparison. It therefore does not add
a row to the before/after optimization ledger in `README.md`.

### Event shape

| Item | Value |
| --- | --- |
| Public path | `https://tcp.1pc.tf` through Traefik and TLS |
| Attack window | `2026-07-13 15:04:30–16:04:30 UTC` (exactly 3,600 seconds) |
| Settlement grace | 60 seconds after the attack window |
| Host | 8 logical CPUs, 31.3 GiB RAM, Docker Compose |
| A&D/KotH roster | 100 accepted teams; one A&D challenge and one shared KotH hill |
| Jeopardy roster | 20 teams, exercised by the first 20 distributed clients |
| Team clients | 100 independent containers, each with a distinct Traefik-network source IP and its own WireGuard peer |
| BYOC path | 100 relay containers and 100 isolated flag-service containers |
| KotH path | One real `nginx:alpine` hill, replaced from the same image at crown-cycle boundaries |
| Client pacing | One distributed client per team; five-second think time |
| Observer | 10-second public/local health, Docker, PostgreSQL, Redis, host, and fleet samples |
| Deployed application revision | `6a17ddb8d5398769fc50f9072c6bf487565f37e3` |

The roster was seeded to avoid spending the measurement hour on account creation, but
the event itself was real: public HTTPS requests crossed Traefik, every team received a
real VPN configuration, attacks reached real team-specific containers through WireGuard,
flags were submitted through the production API, checkers and score engines ran on the
normal scheduler, and KotH used a real managed container.

The 300 workload containers—100 clients, 100 relays, and 100 services—were present in
all 360 attack-window observations. The lifecycle worker also wrote 482 real KotH marker
captures and verified 25 of 25 stale-token rejections after resets.

### Result summary

| Signal | Result |
| --- | ---: |
| Public health | 360/360 successful; p50 76.7 ms, p95 180.8 ms, p99 479.6 ms, max 1,186.1 ms |
| Local health | 360/360 successful; p50 2.8 ms, p95 30.7 ms, max 401.0 ms |
| Team HTTP requests | 327,797 |
| Server 5xx | 9 (0.00275%) |
| VPN attack attempts | 40,789 |
| VPN attack failures | 4,530 (11.11%) |
| Database A&D captures | 12,896 |
| Current-round captures after the load baseline | 5,019; all 100 attackers and all 100 victims represented |
| Previous-round but still-valid captures | 7,877 (61.1% of all database captures) |
| A&D current-round coverage | 41 complete all-team rounds out of 64 observed rounds |
| A&D board | Fully settled; all 100 teams received a nonzero score |
| KotH cycles | 28 completed cycles; 83 control results; 5 confirmed acquisitions |
| KotH board | **Not fully settled** after the event deadline |
| Redis | 1.22–1.56 MiB used; 0 evictions, 0 rejected connections, no new error replies |
| PostgreSQL | 0 deadlocks; database grew by about 7.11 MiB |

All 100 client processes completed the full hour and wrote valid summaries. Their strict
zero-failure thresholds correctly failed because the run observed VPN failures, semantic
failures, and server errors; the processes did not crash early.

The A&D score distribution was intentionally narrow because every scripted team used the
same strategy. Settled scores ranged from 42.0584 to 42.9784. This validates symmetric
roster accounting, but it is not evidence that the formula differentiates human skill.
Only 21 teams received nonzero settled KotH points and five recorded a confirmed
acquisition, which is expected from contention for one shared hill.

### Player-facing latency

Each team ran in its own k6 process. The table reports the mean of the 100 per-team
averages, the median per-team p95, the p95 of those per-team p95 values, and the largest
observation. It is not a single merged histogram.

| Operation | Mean average | Median team p95 | p95 of team p95 | Max |
| --- | ---: | ---: | ---: | ---: |
| All HTTP | 521 ms | 3,876 ms | 4,064 ms | 5,036 ms |
| A&D state | 272 ms | 1,113 ms | 1,423 ms | 5,001 ms |
| A&D targets | 172 ms | 590 ms | 879 ms | 4,962 ms |
| A&D scoreboard | 1,125 ms | 3,975 ms | 4,099 ms | 5,032 ms |
| KotH scoreboard | 327 ms | 1,058 ms | 1,255 ms | 5,000 ms |
| KotH token | 333 ms | 1,196 ms | 1,405 ms | 4,999 ms |
| KotH state | 338 ms | 1,202 ms | 1,399 ms | 4,982 ms |
| KotH timeline | 275 ms | 847 ms | 1,116 ms | 3,765 ms |
| Jeopardy game | 195 ms | 613 ms | 672 ms | 4,991 ms |
| Jeopardy details | 175 ms | 590 ms | 704 ms | 4,508 ms |
| A&D submit | 360 ms | 1,429 ms | 1,920 ms | 4,962 ms |
| VPN attack | 25 ms | 60 ms | 69 ms | 3,351 ms |

### Resource profile

Docker CPU uses Docker's convention: 100% is one logical CPU. Memory stayed bounded;
database and connection contention, rather than host RAM exhaustion, was the immediate
limit.

| Component | CPU average | CPU p95 | CPU max | RAM average | RAM max |
| --- | ---: | ---: | ---: | ---: | ---: |
| RSCTF | 84.4% | 257.4% | 398.9% | 167.4 MiB | 418.4 MiB |
| PostgreSQL | 153.0% | 419.0% | 466.8% | 433.2 MiB | 658.9 MiB |
| Redis | 1.4% | — | 7.5% | about 8 MiB | 8.8 MiB |
| Traefik | 7.7% | — | 43.1% | — | 416.2 MiB |
| 100 relay containers | 4.6% aggregate | — | 18.5% | 542 MiB aggregate | 655 MiB |
| 100 flag services | 2.6% aggregate | — | 10.9% | 991 MiB aggregate | 1.03 GiB |
| 100 attack clients | 63.6% aggregate | — | 140.5% | 1.53 GiB aggregate | 1.74 GiB |

PostgreSQL held an average of 33 connections, with 7.6 active and 1.1 waiting. The p95
was 31 active and 7 waiting, nearly the full 32-connection application pool. Application
logs recorded 6,832 slow connection acquisitions and three activity-update pool
timeouts during the hour.

Five-minute arithmetic means show that application memory remained bounded during the
hour, while PostgreSQL's resident set rose as the event accumulated evidence and rollups:

| UTC interval | Samples | App CPU | App RAM | PostgreSQL CPU | PostgreSQL RAM | Public health | Avg / p95 | DB active / waiting |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 15:04:30–15:09:30 | 30 | 62.5% | 139.9 MiB | 232.3% | 350.2 MiB | 30/30 | 131.4 / 387.8 ms | 13.3 / 0.9 |
| 15:09:30–15:14:30 | 30 | 80.4% | 147.7 MiB | 277.6% | 355.8 MiB | 30/30 | 98.3 / 170.1 ms | 18.7 / 1.4 |
| 15:14:30–15:19:30 | 30 | 85.1% | 156.3 MiB | 159.0% | 350.5 MiB | 30/30 | 91.5 / 173.2 ms | 3.9 / 0.7 |
| 15:19:30–15:24:30 | 30 | 99.0% | 168.9 MiB | 204.4% | 345.3 MiB | 30/30 | 91.6 / 158.7 ms | 10.5 / 1.1 |
| 15:24:30–15:29:30 | 30 | 90.9% | 167.9 MiB | 101.6% | 352.9 MiB | 30/30 | 80.6 / 133.8 ms | 2.4 / 0.3 |
| 15:29:30–15:34:30 | 30 | 88.0% | 168.0 MiB | 73.5% | 348.9 MiB | 30/30 | 96.1 / 218.0 ms | 2.5 / 0.9 |
| 15:34:30–15:39:30 | 30 | 81.6% | 155.4 MiB | 40.5% | 346.1 MiB | 30/30 | 92.9 / 164.2 ms | 1.6 / 0.3 |
| 15:39:30–15:44:30 | 30 | 69.3% | 164.7 MiB | 313.9% | 376.8 MiB | 30/30 | 94.6 / 138.1 ms | 22.0 / 3.2 |
| 15:44:30–15:49:30 | 30 | 73.5% | 168.8 MiB | 65.1% | 546.2 MiB | 30/30 | 83.4 / 167.4 ms | 1.9 / 0.4 |
| 15:49:30–15:54:30 | 30 | 87.8% | 200.7 MiB | 152.7% | 582.7 MiB | 30/30 | 90.0 / 147.4 ms | 4.2 / 1.0 |
| 15:54:30–15:59:30 | 30 | 84.8% | 191.9 MiB | 141.4% | 602.4 MiB | 30/30 | 87.3 / 168.4 ms | 6.5 / 1.7 |
| 15:59:30–16:04:30 | 30 | 110.1% | 178.8 MiB | 73.7% | 640.1 MiB | 30/30 | 106.0 / 125.9 ms | 3.2 / 1.6 |

### Event-critical gaps

1. **The 30-second scoring cadence is not maintained.** Round gaps averaged 49.0
   seconds, with p95 67.6 seconds and a 117.6-second maximum. The checker pipeline alone
   averaged 31.1 seconds, with p95 64.5 seconds and a 113.8-second maximum. The
   supervisor samples due work every 30 seconds, while checker probes have fixed
   concurrency 32 and a default 30-second timeout. A near-boundary miss can add another
   complete scheduler interval. Round scheduling needs deadline-based catch-up,
   independently bounded checker work, and explicit lag/backlog metrics.

2. **VPN reconciliation interrupts the gameplay path.** The run observed 4,530 failed
   VPN attacks out of 40,789 and 55 full `wg0` synchronizations. Reconciliation installs
   a fail-closed DROP boundary, reloads the complete peer/firewall state, and then removes
   the boundary. Security must remain fail-closed, but peer mutations should be
   coalesced and applied incrementally or through an atomic shadow-state swap so one
   team's configuration request does not repeatedly pause all teams.

3. **KotH can finish with an unsettled official board.** During official round 11 the
   crown-cycle transition logged a foreign-key failure while writing
   `KothCycleAuditReceipts`. That round retained a non-scorable control result with no
   `cycle_id`. The rollup join then saw only 11 of the epoch's 12 official rounds, so a
   freshly generated board remained `fullySettled=false` after the deadline. Transition
   evidence, void-round accounting, and rollup completion need one transactional model
   plus a regression test for an event ending during reset/recovery.

4. **Scoreboard reads can return 500 under contention.** Nine requests failed with
   `SET TRANSACTION ISOLATION LEVEL must be called before any query`, each paired with a
   500 trace. Both A&D and KotH board builders begin a pooled transaction and issue a
   manual isolation statement. The hour also logged inconsistent transaction-state
   notices. Snapshot setup must use a transaction API that establishes isolation before
   any statement and guarantees a clean connection is returned to the pool.

### Important next gaps

1. **The connection pool and authentication side effects amplify polling.** The pool was
   at 31 active connections at p95. Every authenticated request starts a detached user
   activity update that repeats user work, and concurrent updates can defeat the
   five-second throttle. Coalesce activity updates, remove duplicate authentication
   reads, bound background work, and expose pool wait metrics.

2. **Current-round state precedes current flag availability.** Although previous flags
   are deliberately valid for a grace window, 61.1% of accepted captures used a previous
   round's flag. Publish the new player-visible round only after fresh flags have been
   planted, or return an explicit `flagsReady`/effective-round field so clients do not
   race propagation.

3. **The public rate limiter is unfair to shared NATs.** The general bucket is keyed by
   public IP. A separate concentrated-source canary reached 4,376 HTTP 429 responses in
   4,548 requests. The hour avoided that artifact by assigning distinct trusted-proxy
   source IPs, but real schools and teams commonly share NAT. Authenticated limits should
   key primarily on account/participation, with an IP abuse backstop.

4. **Core services have a single failure domain.** Compose lacks restart policies,
   RSCTF/Redis health checks, and explicit core-service resource limits. `/healthz`
   reports a static `ok` without proving PostgreSQL, Redis, Docker, WireGuard, or cron
   progress. The VPN advisory lease also prevents active-active VPN ownership. Add
   dependency-aware readiness, liveness, and operational failover before calling the
   platform highly available.

5. **Container disk and audit-diff growth are not bounded.** Challenge writable layers,
   Docker JSON logs, and KotH filesystem diffs have no hard size limits. The host started
   about 82% full. Add per-container storage/log caps, cap or stream diff receipts, and
   alert on bytes and inodes before a malicious challenge can starve PostgreSQL.

6. **Long Docker and cron work is too serial.** Round advancement, reaping, cache work,
   and lifecycle operations share a supervisor path, while several Docker operations
   have no explicit timeout. A slow daemon or checker can delay unrelated games. Give
   each domain durable ownership, a bounded deadline, and independent progress metrics.

7. **Admission control and telemetry are incomplete.** There is no aggregate CPU, RAM,
   subnet, or storage admission check for a proposed event. Operators also lack first-
   class metrics for round lag, checker backlog, VPN sync duration, pool wait, container
   disk growth, and cron ownership. `pg_stat_statements` was unavailable during this
   diagnosis.

### Integrity results

The load did not produce duplicate rounds, cycles, control observations, acquisitions,
tokens, runtime identities, rollups, or attacks. There were no overlapping active crown
cycles, stale-container attributions, cross-cycle tokens, scorable platform-void rows,
malformed cooldown spans, retained former holders, PostgreSQL deadlocks, Redis evictions,
or unexpected stale-token acceptances. These results are the strongest part of the
current implementation and should remain hard gates while the bottlenecks above are
fixed.

### Evidence limitations

- The services were real isolated containers, but their application was a deterministic
  flag fixture, not 100 distinct vulnerable programs or arbitrary exploit payloads.
- Each team had one automated player process. The run did not model several human team
  members browsing simultaneously.
- The run used one A&D service per team, one shared hill, one host, and no deliberate
  host restart or network partition during the measured hour.
- This run's original `unexpected_non_2xx` metric combined platform API and VPN-target
  outcomes, so it cannot reconstruct an exact public non-2xx count. Application logs and
  the `server_5xx` metric independently establish the nine 500 responses. The harness now
  records platform failures, timeouts, and 429 responses separately for future runs.
- Raw CSV and per-team JSON evidence stays outside Git under
  `/tmp/rsctf-100-team-1h-20260713`; only this sanitized, aggregated report is retained.
- Pre-teardown SQL and application-log findings were summarized into this report but not
  retained as a separate sanitized artifact. Future runs should export those aggregates
  so the database, KotH, and log findings remain independently reproducible after cleanup.

### Post-run cleanup

`npm run teardown` removed both load-test games, all `@load.test` users, all
`LT<game>_*` and `ltlive*` load teams, participations, rounds, crown cycles, targets,
checker directories, team clients,
relays, isolated services, and managed hill containers. A second teardown completed
successfully, confirming idempotency. Global duplicate checks for rounds, attacks,
cycles, control ticks, acquisitions, tokens, and A&D/KotH rollups all returned zero.
The Compose application, PostgreSQL, and Redis remained running; the public and local
health probes returned HTTP 200 after cleanup.

### Reproduction

Use a dedicated host or event namespace. The lifecycle state contains live capabilities
and is intentionally gitignored.

```sh
cd tests/load
TEAMS_JEO=20 TEAMS_AD=100 CH_STATIC=4 \
  EVENT_DURATION_SECONDS=10800 npm run provision
```

In a second terminal, start the observer and leave it running through settlement:

```sh
cd tests/load
OUT_DIR=/tmp/rsctf-100-team-1h TARGET=https://tcp.1pc.tf \
  INTERVAL_SECONDS=10 npm run observe
```

Back in the first terminal, start the event workload:

```sh
TARGET=https://tcp.1pc.tf VUS=100 FLEET=100 DURATION=1h KEEP=1 \
  DISTRIBUTED_TEAM_CLIENTS=1 LIFECYCLE_ISOLATED_SERVICES=1 \
  REQUIRE_ISOLATED_SERVICES=1 TEAM_THINK_SECONDS=5 \
  EVENT_END_GRACE_SECONDS=60 npm run lifecycle

npm run teardown
```

Stop the observer only after settlement and teardown have been recorded.
The lifecycle gate treats either A&D or KotH settlement failure, incomplete team
evidence, missing attackers/victims, or an integrity violation as a failed run.
