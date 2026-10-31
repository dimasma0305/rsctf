# Runtime, agents, state and deployment map

Read [architecture-data.md](architecture-data.md) for ownership/security rules and
[verification-release.md](verification-release.md) before lifecycle/release work.

## Roles and startup

The same binary supports these roles. The exact source of truth is
[src/models/internal/configs.rs](../../../../src/models/internal/configs.rs) for capabilities and validation,
[src/main.rs](../../../../src/main.rs) for actual worker registration, and
[src/startup_policy.rs](../../../../src/startup_policy.rs) for reconciliation ownership. A capability bit does
not prove every queue has a consumer: inspect startup and test the topology.

| Role | HTTP surface | Background/runtime ownership |
| --- | --- | --- |
| `all` | Public API, SPA and stateful routes | Combined maintenance, rounds and network; default monolith |
| `development` | API/SPA preview | API-focused development; no production network/round engine |
| `web` | Public API and SPA | Scalable requests/feeds; no process-local tunnel owner |
| `control` | Narrow stateful API | Singleton combined maintenance, rounds, network |
| `engine` | Health only | Scalable maintenance and managed rounds |
| `network` | Narrow stateful API | Singleton VPN/SSH/tunnels/capture and network-bound BYOC rounds |
| `migrate` | None; exits after migration | One-shot schema/bootstrap work |

Supported shapes are `all x1`, `web xN + control x1`, and
`web xN + engine xN + network x1`. Split roles require Redis and shared POSIX
working storage, including when blobs use S3. Check
[docs/deploy/scaling.md](../../../../docs/deploy/scaling.md) for constraints, pool budgeting, proxy routes, and
the centralized [worker registration](../../../../src/runtime_workers.rs).

Public web and stateful routing are separated in [src/server.rs](../../../../src/server.rs).
`/api/stateful/...` and `/hub/containerExec...` must reach the singleton owner;
read the actual compatibility routes before changing proxy rules.
[deploy/Caddyfile](../../../../deploy/Caddyfile) and [charts/rsctf/templates/](../../../../charts/rsctf/templates/) implement the deployed
routing/probes. `/livez` is process liveness; `/healthz` is readiness, including
dependencies/topology/drain. Inspect role/capability headers and
[src/services/runtime_topology.rs](../../../../src/services/runtime_topology.rs) when a healthy process is not ready.

## Background work

| Work | Registration / implementation |
| --- | --- |
| Maintenance, ended backends and image cleanup | [src/services/cron/mod.rs](../../../../src/services/cron/mod.rs), [src/services/cron/maintenance.rs](../../../../src/services/cron/maintenance.rs), [src/services/cron/backend_reaper.rs](../../../../src/services/cron/backend_reaper.rs), [src/services/cron/image_cleanup.rs](../../../../src/services/cron/image_cleanup.rs) |
| Round scheduler and end-of-round completion | [src/services/cron/scheduler.rs](../../../../src/services/cron/scheduler.rs), [src/services/cron/round_finish.rs](../../../../src/services/cron/round_finish.rs), [src/services/ad/engine/](../../../../src/services/ad/engine/) |
| Imports, challenge revision effects, pushback | [src/controllers/edit/test_container/import_jobs.rs](../../../../src/controllers/edit/test_container/import_jobs.rs), [src/controllers/edit/challenges/](../../../../src/controllers/edit/challenges/), [src/services/repo_binding_scheduler.rs](../../../../src/services/repo_binding_scheduler.rs) |
| Durable control jobs and lease recovery | [src/services/control_jobs.rs](../../../../src/services/control_jobs.rs), [src/services/control_jobs/](../../../../src/services/control_jobs/) |
| Feed, mail, suspicion and retention workers | [src/startup_policy.rs](../../../../src/startup_policy.rs), [src/main.rs](../../../../src/main.rs), [src/services/mail_outbox.rs](../../../../src/services/mail_outbox.rs), [src/services/suspicion/](../../../../src/services/suspicion/), [src/services/mutation_retention.rs](../../../../src/services/mutation_retention.rs) |
| Capture, VPN and BYOC reconciliation | [src/services/traffic/capture/](../../../../src/services/traffic/capture/), [src/services/ad/vpn/](../../../../src/services/ad/vpn/), [src/services/byoc_tunnel/](../../../../src/services/byoc_tunnel/) |

For a stuck job, follow enqueue → durable row/operation ID → eligible role →
claim/expiry → execution → receipt. Search callers as well as the loop. Cancellation
of an HTTP request does not prove external Docker/Git/checker work stopped.
Control execution uses [supervised lanes](../../../../src/services/control_jobs/supervisor.rs).
The [runtime tests](../../../../src/runtime_worker_tests.rs) exercise registered
consumers. Runtime shutdown and sustained cross-game fairness still need the
end-to-end evidence tracked in TODO.md.

## Runtime adapters and trust boundaries

| Boundary | Owner / relevant evidence |
| --- | --- |
| Docker / no-runtime adapter and capacity admission | [src/services/container.rs](../../../../src/services/container.rs), [src/services/container/backend.rs](../../../../src/services/container/backend.rs), [src/services/container/docker/](../../../../src/services/container/docker/), [src/services/docker_admission.rs](../../../../src/services/docker_admission.rs) |
| Kubernetes workloads/policy/callbacks | [src/services/k8s.rs](../../../../src/services/k8s.rs), [src/services/k8s/](../../../../src/services/k8s/), [scripts/test-kubernetes-koth-callback.sh](../../../../scripts/test-kubernetes-koth-callback.sh) |
| Per-challenge Docker/Kubernetes/Kata selection and immutable instance ownership | [profile configuration/router](../../../../src/services/container/profiles/), [Kata RuntimeClass validation](../../../../src/services/k8s/profiles.rs), [deployment and retirement rules](../../../../docs/deploy/runtime-profiles.md), [real switch gate](../../../../scripts/test-runtime-profiles.sh) |
| Trusted worker server and durable desired state | [src/services/worker/](../../../../src/services/worker/), [src/services/worker_store/](../../../../src/services/worker_store/), [src/services/worker_pki.rs](../../../../src/services/worker_pki.rs) |
| Trusted worker native agent | [agents/worker-agent/src/main.rs](../../../../agents/worker-agent/src/main.rs), [agents/worker-agent/src/client/](../../../../agents/worker-agent/src/client/), [agents/worker-agent/src/runtime/](../../../../agents/worker-agent/src/runtime/), [agents/worker-agent/README.md](../../../../agents/worker-agent/README.md) |
| Shared worker framing/enrollment/workload protocol | [lib/worker-protocol/src/](../../../../lib/worker-protocol/src/), [lib/worker-protocol/README.md](../../../../lib/worker-protocol/README.md) |
| Participant BYOC agent and transport | [agents/byoc-agent/src/](../../../../agents/byoc-agent/src/), [src/services/byoc_tunnel/](../../../../src/services/byoc_tunnel/), [src/controllers/game/ad/byoc/](../../../../src/controllers/game/ad/byoc/) |
| Managed checker confinement | [src/services/ad/engine/sandbox.rs](../../../../src/services/ad/engine/sandbox.rs), [src/services/ad/engine/sandbox/](../../../../src/services/ad/engine/sandbox/), [src/services/ad/engine/checker/](../../../../src/services/ad/engine/checker/) |
| VPN owner session, firewall and route policy | [src/services/ad/vpn/](../../../../src/services/ad/vpn/), [src/services/ad/ssh.rs](../../../../src/services/ad/ssh.rs), [scripts/check-event-vpn.mjs](../../../../scripts/check-event-vpn.mjs) |
| Capture ownership, evidence and inventory | [src/services/traffic/capture/](../../../../src/services/traffic/capture/), [src/services/traffic/inventory/](../../../../src/services/traffic/inventory/), [src/services/capture_safety.rs](../../../../src/services/capture_safety.rs) |

Trusted workers use outbound mTLS and execute supported platform-owned workloads;
participant BYOC is a separate, untrusted boundary using outbound WSS/yamux.
Follow [docs/deploy/workers.md](../../../../docs/deploy/workers.md) for supported backend combinations.
Real confinement depends on Landlock/seccomp/kernel support; a local sandbox
startup failure is an environment limit to verify on a capable runner, never
a reason to silently disable production confinement.

## Persistent and reconstructible state

| State | Authority / ownership |
| --- | --- |
| Users, participation, scores, operations, evidence, worker intent | PostgreSQL; [src/models/data/](../../../../src/models/data/), [src/migrations/](../../../../src/migrations/) |
| Response acceleration and cross-replica notifications | [src/services/cache.rs](../../../../src/services/cache.rs), [src/services/event_bus.rs](../../../../src/services/event_bus.rs); Redis is not the business-data authority |
| One-time credentials, CAS, leases, local secrets | Trace the exact cache API and caller; ordinary response eviction semantics cannot replace credential/coordination semantics |
| Content-addressed local/S3 blobs and references | [src/storage/mod.rs](../../../../src/storage/mod.rs), [src/storage/local_blob_storage.rs](../../../../src/storage/local_blob_storage.rs), [src/storage/s3_blob_storage.rs](../../../../src/storage/s3_blob_storage.rs), [src/services/blob_refs.rs](../../../../src/services/blob_refs.rs) |
| Mutable repositories, checker environments, capture working files | Shared `RSCTF_STORAGE_ROOT`; [src/services/git_sync/](../../../../src/services/git_sync/), [src/services/traffic/](../../../../src/services/traffic/) |
| Docker images, writable layers, logs, final snapshots | Runtime adapter plus [src/services/image_storage.rs](../../../../src/services/image_storage.rs), [src/services/ad/snapshots.rs](../../../../src/services/ad/snapshots.rs) |

Trace which owner retains/deletes data before proposing storage savings.
S3 blobs alone do not eliminate filesystem rename/flock or shared mounts.
PostgreSQL session advisory-lock users require a direct or session-preserving
connection; inspect the actual lease and pooling setup rather than assuming
transaction pooling preserves ownership. Namespace lifetime and external-session
reconnection must be tested separately from process exit.

## Deployment entry points

- [Simple maintenance commands](../../../../scripts/manage.py), exposed through
  [the launcher](../../../../rsctf), support basic local-storage installs. Update
  verifies a new bundle, captures a consistent backup and invokes the established
  cutover. Restore uses a fresh Compose project and preserves file ownership.
  [Maintenance regressions](../../../../scripts/test-manage.py) include an opt-in
  real Docker/PostgreSQL restore drill (`RSCTF_MANAGE_DOCKER_TEST=1`).

- [First-run Docker setup](../../../../src/services/bootstrap_admin.rs) generates
  a persistent token for an empty database and shows it in startup logs until setup completes.
  Registration consumes its stored token in the first administrator's transaction.
  `RSCTF_BOOTSTRAP_AUTO=false` disables announcements; Helm supplies its Secret instead.

- [deploy/README.md](../../../../deploy/README.md), [deploy/.env.example](../../../../deploy/.env.example), [deploy/compose.yml](../../../../deploy/compose.yml):
  base services and configuration names. Never copy real secrets into the map.
- The A&D VPN overlays route managed callbacks through the private
  [reporter proxy](../../../../deploy/koth-reporter.Caddyfile). Its reserved service
  address stays separate from the VPN owner's blocked INPUT namespace; only
  capability authentication, context, observation and health reach the owner over
  a private uplink. Only the proxy's exact uplink /32 is trusted for target IPs.
  [The Docker regression](../../../../scripts/test-koth-reporter-proxy.py) tests
  both shipped topologies with a real INPUT fence and HTTP fixture upstream.
- [deploy/compose.roles.yml](../../../../deploy/compose.roles.yml), [deploy/compose.roles.docker.yml](../../../../deploy/compose.roles.docker.yml),
  [deploy/compose.roles.ad-vpn.yml](../../../../deploy/compose.roles.ad-vpn.yml), [deploy/compose.roles.capture.yml](../../../../deploy/compose.roles.capture.yml),
  [deploy/compose.roles.workers.yml](../../../../deploy/compose.roles.workers.yml): supported role overlays.
- [deploy/compose.development.yml](../../../../deploy/compose.development.yml), [scripts/dev.mjs](../../../../scripts/dev.mjs):
  development preview; inspect `--help` and defaults before starting it.
- [charts/rsctf/values.yaml](../../../../charts/rsctf/values.yaml), [docs/deploy/kubernetes.md](../../../../docs/deploy/kubernetes.md),
  [docs/deploy/reverse-proxy.md](../../../../docs/deploy/reverse-proxy.md): Helm, selectors, capabilities and routing.
- [scripts/compose-maintenance-cutover.sh](../../../../scripts/compose-maintenance-cutover.sh),
  [scripts/kubernetes-maintenance-cutover.sh](../../../../scripts/kubernetes-maintenance-cutover.sh): migration cutover; operational
  mutations require the authorized scope and exact release digest.
- [scripts/bootstrap-worker.sh](../../../../scripts/bootstrap-worker.sh), [scripts/bootstrap-worker.ps1](../../../../scripts/bootstrap-worker.ps1),
  [scripts/install-worker.sh](../../../../scripts/install-worker.sh), [scripts/install-worker.ps1](../../../../scripts/install-worker.ps1):
  public/native installer boundaries.

Use [checks-map.md](checks-map.md) for lifecycle evidence and publication gates.
