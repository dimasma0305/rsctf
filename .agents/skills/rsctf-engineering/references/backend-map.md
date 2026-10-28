# Backend ownership and discovery

Read [architecture-data.md](architecture-data.md) before backend changes and
[performance-load.md](performance-load.md) for hot reads or resource changes.

## Composition and route lookup

| Layer | Entry point | What it owns |
| --- | --- | --- |
| Process startup | [src/main.rs](../../../../src/main.rs), [src/startup_policy.rs](../../../../src/startup_policy.rs) | Configuration, migrations, adapters, role selection, worker supervision/drain |
| Dependency composition | [src/app_state.rs](../../../../src/app_state.rs) | Shared database, cache, token, storage, runtime and registry handles |
| HTTP composition | [src/server.rs](../../../../src/server.rs) | Public/web/stateful/health routers, middleware order, static SPA fallback |
| Controller roots | [src/controllers/mod.rs](../../../../src/controllers/mod.rs) | Account/team/info/OAuth/assets/exercise plus player/admin/editor roots |
| Player registration | [src/controllers/game/routes.rs](../../../../src/controllers/game/routes.rs), [src/controllers/game/ad/mod.rs](../../../../src/controllers/game/ad/mod.rs), [src/controllers/game/koth/routes.rs](../../../../src/controllers/game/koth/routes.rs) | Base player API and format-specific routes |
| Admin/editor registration | [src/controllers/admin/mod.rs](../../../../src/controllers/admin/mod.rs), [src/controllers/edit/mod.rs](../../../../src/controllers/edit/mod.rs) | Platform administration and organizer operations |
| Live hubs | [src/hubs/mod.rs](../../../../src/hubs/mod.rs), [src/hubs/signalr.rs](../../../../src/hubs/signalr.rs) | Admin, user, attack, monitor, container-exec transports |

Find the literal path first; inspect surrounding method registrations and router
merges before choosing a handler. String search includes aliases/tests and cannot
prove that a route is mounted on a particular role.

```sh
rg -n '"/(api|hub|healthz|livez)' src/server.rs src/controllers src/hubs
rg -n '\.route\(|\.nest\(|\.merge\(' src/server.rs src/controllers src/hubs
rg -n 'pub (async )?fn|pub\(crate\) (async )?fn' src/services/ad
```

Use [web/src/Api.ts](../../../../web/src/Api.ts) to find callers and expected wire shapes, then confirm
against Rust handlers/DTOs. On this baseline, `web genapi` still references a
specification endpoint not provided by main. The incremental Utoipa pipeline is
architecture-branch work; do not blindly regenerate the legacy client.

## Domain services

| Responsibility | Owning implementation |
| --- | --- |
| Competition engine, flags, checker sandbox | [src/services/ad/engine/](../../../../src/services/ad/engine/); compatibility aliases `services::ad_engine` resolve through [src/services/mod.rs](../../../../src/services/mod.rs) |
| A&D scoring, roster, resets, snapshots | [src/services/ad/scoring/](../../../../src/services/ad/scoring/), [src/services/ad/roster.rs](../../../../src/services/ad/roster.rs), [src/services/ad/reset.rs](../../../../src/services/ad/reset.rs), [src/services/ad/snapshots.rs](../../../../src/services/ad/snapshots.rs) |
| KotH policy, callbacks and capability authentication | [src/controllers/game/koth/](../../../../src/controllers/game/koth/), [src/services/ad/koth_api_capability.rs](../../../../src/services/ad/koth_api_capability.rs), [src/services/ad/koth_capability_cache.rs](../../../../src/services/ad/koth_capability_cache.rs), [src/services/ad/koth_reporter.rs](../../../../src/services/ad/koth_reporter.rs) |
| Challenge lifecycle and runtime policy | [src/services/container.rs](../../../../src/services/container.rs), [src/services/challenge_workloads.rs](../../../../src/services/challenge_workloads.rs), [src/services/container_policy.rs](../../../../src/services/container_policy.rs) |
| Durable jobs and mutation receipts | [src/services/control_jobs.rs](../../../../src/services/control_jobs.rs), [src/services/control_jobs/](../../../../src/services/control_jobs/), [src/services/mutation_operations.rs](../../../../src/services/mutation_operations.rs), [src/services/mutation_retention.rs](../../../../src/services/mutation_retention.rs), [src/services/mutation_retention/](../../../../src/services/mutation_retention/) |
| Repositories, checker publication, image preflight | [src/services/git_sync/](../../../../src/services/git_sync/), [src/services/repo_binding_scheduler.rs](../../../../src/services/repo_binding_scheduler.rs), [src/services/image_preflight/](../../../../src/services/image_preflight/) |
| Notifications and commit-to-live delivery | [src/services/event_bus.rs](../../../../src/services/event_bus.rs), [src/services/feed_publication.rs](../../../../src/services/feed_publication.rs), [src/services/game_event_feed.rs](../../../../src/services/game_event_feed.rs), [src/services/submission_feed.rs](../../../../src/services/submission_feed.rs), [src/services/notice_delivery.rs](../../../../src/services/notice_delivery.rs) |
| Mail, webhooks, donations | [src/services/mail_outbox.rs](../../../../src/services/mail_outbox.rs), [src/services/mail.rs](../../../../src/services/mail.rs), [src/services/discord_webhook.rs](../../../../src/services/discord_webhook.rs), [src/services/donations/](../../../../src/services/donations/) |
| Identity observations, suspicion, evidence | [src/services/anti_cheat.rs](../../../../src/services/anti_cheat.rs), [src/services/anti_cheat/](../../../../src/services/anti_cheat/), [src/services/suspicion/](../../../../src/services/suspicion/), [src/services/participation_evidence.rs](../../../../src/services/participation_evidence.rs) |
| Event security, honeypots, flag egress | [src/services/event_security/](../../../../src/services/event_security/), [src/services/honeypot_telemetry.rs](../../../../src/services/honeypot_telemetry.rs), [src/services/flag_egress_feed.rs](../../../../src/services/flag_egress_feed.rs), [src/bin/rsctf-event-sensor.rs](../../../../src/bin/rsctf-event-sensor.rs) |
| Health, metrics, audit | [src/services/health.rs](../../../../src/services/health.rs), [src/services/observability.rs](../../../../src/services/observability.rs), [src/services/audit.rs](../../../../src/services/audit.rs), [src/controllers/admin/realtime.rs](../../../../src/controllers/admin/realtime.rs) |

Main still contains business operations in controllers. Follow actual callers
rather than assuming the service extraction on the architecture branch is merged.

## Data, authorization and wire contracts

| Concern | Start here |
| --- | --- |
| Pool creation, model connection and raw SQL | [src/extensions/database.rs](../../../../src/extensions/database.rs), [src/utils/database.rs](../../../../src/utils/database.rs), [src/app_state.rs](../../../../src/app_state.rs) |
| Entities grouped by domain | [src/models/data/mod.rs](../../../../src/models/data/mod.rs), [src/models/data/](../../../../src/models/data/) |
| Forward migrations and schema compatibility | [src/migrations/mod.rs](../../../../src/migrations/mod.rs), [src/migrations/](../../../../src/migrations/) |
| Environment, roles, validation | [src/models/internal/configs.rs](../../../../src/models/internal/configs.rs) |
| User/admin/monitor extraction | [src/middlewares/privilege_authentication.rs](../../../../src/middlewares/privilege_authentication.rs) |
| Team/game eligibility and lifecycle leases | [src/controllers/game/membership.rs](../../../../src/controllers/game/membership.rs), [src/controllers/game/play.rs](../../../../src/controllers/game/play.rs), [src/services/authorization_lease.rs](../../../../src/services/authorization_lease.rs) |
| Cookies, CSRF, proxy trust and request admission | [src/controllers/oauth_response.rs](../../../../src/controllers/oauth_response.rs), [src/middlewares/request_security.rs](../../../../src/middlewares/request_security.rs), [src/middlewares/rate_limiter.rs](../../../../src/middlewares/rate_limiter.rs), [src/services/credential_admission.rs](../../../../src/services/credential_admission.rs) |
| DTO envelopes, errors, timestamps and enums | [src/utils/shared.rs](../../../../src/utils/shared.rs), [src/utils/error.rs](../../../../src/utils/error.rs), [src/utils/datetime.rs](../../../../src/utils/datetime.rs), [src/utils/enums.rs](../../../../src/utils/enums.rs) |
| Cache, coalescing, invalidation | [src/services/cache.rs](../../../../src/services/cache.rs), [src/services/cache/](../../../../src/services/cache/), [src/utils/single_flight.rs](../../../../src/utils/single_flight.rs), [src/controllers/game/scoreboard_page.rs](../../../../src/controllers/game/scoreboard_page.rs) |
| Bounded uploads, archives and blobs | [src/utils/upload.rs](../../../../src/utils/upload.rs), [src/utils/archive.rs](../../../../src/utils/archive.rs), [src/storage/](../../../../src/storage/), [src/services/blob_refs.rs](../../../../src/services/blob_refs.rs) |

Find test modules next to the owner: `mod tests`, `#[path]`, sibling
`*_tests.rs`, or subdirectory `tests.rs`. An ignored database test must be run
with a real disposable database and the correct filter; default unit success
alone does not cover it.

## Additional boundary owners

These entry points complete the public module-family index; use their callers and
adjacent tests when a feature crosses admission, identity, export or telemetry.

| Responsibility | Owning implementation |
| --- | --- |
| Remaining HTTP boundaries | [src/controllers/event_security.rs](../../../../src/controllers/event_security.rs), [src/controllers/honeypot.rs](../../../../src/controllers/honeypot.rs), [src/controllers/proxy/](../../../../src/controllers/proxy/) |
| JWT, managed tokens, captcha and live roster | [src/services/token.rs](../../../../src/services/token.rs), [src/services/managed_api_token.rs](../../../../src/services/managed_api_token.rs), [src/services/captcha.rs](../../../../src/services/captcha.rs), [src/services/live_roster.rs](../../../../src/services/live_roster.rs) |
| Download, proxy and export admission | [src/services/asset_admission.rs](../../../../src/services/asset_admission.rs), [src/services/proxy_admission.rs](../../../../src/services/proxy_admission.rs), [src/services/bulk_export.rs](../../../../src/services/bulk_export.rs), [src/services/monitor_export.rs](../../../../src/services/monitor_export.rs) |
| Image identity, event configuration and admin retention | [src/services/challenge_images.rs](../../../../src/services/challenge_images.rs), [src/services/game_config.rs](../../../../src/services/game_config.rs), [src/services/admin_mutation_retention.rs](../../../../src/services/admin_mutation_retention.rs) |
| Artifact scanning and raw observation writers | [src/services/agent_artifacts/](../../../../src/services/agent_artifacts/), [src/services/flag_egress_observations.rs](../../../../src/services/flag_egress_observations.rs), [src/services/honeypot_listener.rs](../../../../src/services/honeypot_listener.rs) |
| Hub streams and connection admission | [src/hubs/admin.rs](../../../../src/hubs/admin.rs), [src/hubs/user.rs](../../../../src/hubs/user.rs), [src/hubs/attack.rs](../../../../src/hubs/attack.rs), [src/hubs/monitor.rs](../../../../src/hubs/monitor.rs), [src/hubs/admission.rs](../../../../src/hubs/admission.rs) |
| Storage interface | [src/storage/blob_storage.rs](../../../../src/storage/blob_storage.rs) |

## Existing libraries and primitives

Inspect [Cargo.toml](../../../../Cargo.toml) and the owning module before implementing a primitive:
Axum/Tower for HTTP/admission; Tokio for concurrency; SQLx through the existing
SeaORM pool for new SQL; Redis/TieredCache/SingleFlight for distributed cache
contracts; Moka for existing snapshot caches; Bollard and kube for runtimes;
object_store for S3; reqwest/lettre for HTTP/mail; Argon2/JWT/rustls for identity
and transport; zip/flate2 for archives. Preserve bounds, atomicity and security
semantics when benchmarking a replacement. See [runtime-map.md](runtime-map.md)
for agents, filesystem state, networking and deployment constraints.
