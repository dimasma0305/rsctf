# Application navigation

Use this index to locate ownership before reading a large module or running a full
suite. All source paths are relative to the current checkout. Start at
`git rev-parse --show-toplevel`; inspect `git status --short` and the branch first.
This map includes the architecture integration work based on main `deb89691`.
Source on the active branch is authoritative; maintain the map as owners move.

See [navigation-effectiveness.md](navigation-effectiveness.md) for coverage,
lookup measurements, reproduction and limitations.

## Choose a feature

Browser URLs are navigation examples, not a backend authorization policy.
API paths and parameter names must be read
from the router: browser `/games/:id` usually calls singular `/api/game/{id}`.

| Feature / user intent | Frontend entry | API / backend entry |
| --- | --- | --- |
| Home, event discovery, recent events | [Home](../../../../web/src/pages/Index.tsx), [event catalog](../../../../web/src/pages/games/Index.tsx) | [src/controllers/info.rs](../../../../src/controllers/info.rs), [src/controllers/game/catalog.rs](../../../../src/controllers/game/catalog.rs) |
| Login, registration, confirmation, password reset, profile | [account pages](../../../../web/src/pages/account/) | [src/controllers/account/](../../../../src/controllers/account/), [src/models/request/account.rs](../../../../src/models/request/account.rs) |
| Discord / OAuth callback and session cookies | [web/src/components/OAuthButtons.tsx](../../../../web/src/components/OAuthButtons.tsx), [web/src/utils/CompleteSignIn.ts](../../../../web/src/utils/CompleteSignIn.ts) | [src/controllers/oauth.rs](../../../../src/controllers/oauth.rs), [src/controllers/oauth_response.rs](../../../../src/controllers/oauth_response.rs) |
| Teams, invitations, joining/leaving an event | [web/src/pages/Teams.tsx](../../../../web/src/pages/Teams.tsx), [web/src/components/TeamJoinModal.tsx](../../../../web/src/components/TeamJoinModal.tsx), [web/src/components/GameJoinModal.tsx](../../../../web/src/components/GameJoinModal.tsx) | [src/controllers/team/](../../../../src/controllers/team/), [src/controllers/game/membership.rs](../../../../src/controllers/game/membership.rs), [src/controllers/game/participation_review.rs](../../../../src/controllers/game/participation_review.rs) |
| Challenge catalog, details, hints, solvers | [web/src/pages/challenges/Index.tsx](../../../../web/src/pages/challenges/Index.tsx), [web/src/pages/games/[id]/Challenges.tsx](../../../../web/src/pages/games/%5Bid%5D/Challenges.tsx), [web/src/components/GameChallengeModal.tsx](../../../../web/src/components/GameChallengeModal.tsx) | [src/controllers/game/play.rs](../../../../src/controllers/game/play.rs), [src/controllers/game/catalog.rs](../../../../src/controllers/game/catalog.rs), [src/controllers/game/routes.rs](../../../../src/controllers/game/routes.rs) |
| Jeopardy flag submission and verdict | [web/src/pages/games/[id]/submit.tsx](../../../../web/src/pages/games/%5Bid%5D/submit.tsx), [web/src/components/FlagVerdictOverlay.tsx](../../../../web/src/components/FlagVerdictOverlay.tsx) | [src/controllers/game/submit.rs](../../../../src/controllers/game/submit.rs), [src/controllers/game/submit/](../../../../src/controllers/game/submit/) |
| Scoreboards, standings pages, freeze, CTFtime, Overall | [web/src/pages/games/[id]/Scoreboard.tsx](../../../../web/src/pages/games/%5Bid%5D/Scoreboard.tsx), [web/src/hooks/useScoreboardPage.ts](../../../../web/src/hooks/useScoreboardPage.ts) | [src/controllers/game/scoreboard_page.rs](../../../../src/controllers/game/scoreboard_page.rs), [src/controllers/game/scoreboard/](../../../../src/controllers/game/scoreboard/), [src/controllers/game/combined_scoreboard.rs](../../../../src/controllers/game/combined_scoreboard.rs) |
| Arena / globe / visual competition state | [web/src/pages/games/[id]/Index.tsx](../../../../web/src/pages/games/%5Bid%5D/Index.tsx), [web/src/pages/games/[id]/arenaSceneModel.ts](../../../../web/src/pages/games/%5Bid%5D/arenaSceneModel.ts) | [src/controllers/game/ad/](../../../../src/controllers/game/ad/), [src/controllers/game/koth/](../../../../src/controllers/game/koth/), [src/controllers/game/play.rs](../../../../src/controllers/game/play.rs) |
| A&D state, targets, resets, flag delivery, scoring | [web/src/pages/games/[id]/Attack.tsx](../../../../web/src/pages/games/%5Bid%5D/Attack.tsx), [web/src/components/AdChallengePanel.tsx](../../../../web/src/components/AdChallengePanel.tsx) | [src/controllers/game/ad/](../../../../src/controllers/game/ad/), [src/services/ad/engine/](../../../../src/services/ad/engine/), [src/services/ad/scoring/](../../../../src/services/ad/scoring/) |
| KotH capture, API observers, cycles, scoring | [web/src/components/KothChallengePanel.tsx](../../../../web/src/components/KothChallengePanel.tsx), [web/src/pages/games/[id]/kothCapture.ts](../../../../web/src/pages/games/%5Bid%5D/kothCapture.ts) | [src/controllers/game/koth/](../../../../src/controllers/game/koth/), [src/services/ad/engine/koth_cycle/](../../../../src/services/ad/engine/koth_cycle/) |
| Live A&D/KotH operational edits | [CompetitionMaintenancePanel.tsx](../../../../web/src/components/admin/CompetitionMaintenancePanel.tsx), [live maintenance guide](../../../../docs/organizers/live-maintenance.md) | [maintenance controller](../../../../src/controllers/edit/ad/maintenance.rs), [maintenance service](../../../../src/services/ad/maintenance.rs); PostgreSQL fences/history tests, [runtime profile browser journey](../../../../tests/visual/runtime-profiles.mjs), [fixed-rate status polling](../../../../tests/load/competition-maintenance.mjs) |
| Challenge containers, terminal, exercise | [web/src/components/InstanceEntry.tsx](../../../../web/src/components/InstanceEntry.tsx), [web/src/components/WsrxManager.tsx](../../../../web/src/components/WsrxManager.tsx) | [src/controllers/game/containers/](../../../../src/controllers/game/containers/), [src/controllers/exercise/](../../../../src/controllers/exercise/), [src/hubs/container.rs](../../../../src/hubs/container.rs) |
| BYOC, VPN, SSH and player connections | [web/src/components/AdToolkitSections.tsx](../../../../web/src/components/AdToolkitSections.tsx) | [src/controllers/game/ad/byoc.rs](../../../../src/controllers/game/ad/byoc.rs), [src/services/byoc_tunnel/](../../../../src/services/byoc_tunnel/), [src/services/ad/vpn/](../../../../src/services/ad/vpn/), [src/services/ad/ssh.rs](../../../../src/services/ad/ssh.rs) |
| Traffic captures, inspection, downloads | [web/src/pages/games/[id]/monitor/Traffic.tsx](../../../../web/src/pages/games/%5Bid%5D/monitor/Traffic.tsx), [web/src/components/traffic/](../../../../web/src/components/traffic/) | [src/controllers/game/traffic.rs](../../../../src/controllers/game/traffic.rs), [src/services/traffic/](../../../../src/services/traffic/) |
| Anti-cheat, evidence, security sensors, flag egress | [web/src/pages/admin/anti-cheat.tsx](../../../../web/src/pages/admin/anti-cheat.tsx), [web/src/pages/games/[id]/monitor/CheatCheck.tsx](../../../../web/src/pages/games/%5Bid%5D/monitor/CheatCheck.tsx) | [src/services/anti_cheat/](../../../../src/services/anti_cheat/), [src/services/suspicion/](../../../../src/services/suspicion/), [src/services/event_security/](../../../../src/services/event_security/), [src/controllers/game/cheat.rs](../../../../src/controllers/game/cheat.rs) |
| AI chat links and solver uploads | [web/src/pages/games/[id]/monitor/ai-chats.tsx](../../../../web/src/pages/games/%5Bid%5D/monitor/ai-chats.tsx), [web/src/components/SolverUploadSection.tsx](../../../../web/src/components/SolverUploadSection.tsx) | [src/controllers/game/ai_chats.rs](../../../../src/controllers/game/ai_chats.rs), [src/controllers/game/solver_uploads.rs](../../../../src/controllers/game/solver_uploads.rs), [src/services/ai_chat_links.rs](../../../../src/services/ai_chat_links.rs) |
| Writeup upload, PDF viewing, grading | [web/src/components/WriteupSubmitModal.tsx](../../../../web/src/components/WriteupSubmitModal.tsx), [web/src/pages/admin/games/[id]/Writeups.tsx](../../../../web/src/pages/admin/games/%5Bid%5D/Writeups.tsx) | [src/controllers/game/writeup.rs](../../../../src/controllers/game/writeup.rs), [src/controllers/admin/writeup_grading.rs](../../../../src/controllers/admin/writeup_grading.rs) |
| Event configuration, divisions, managers, notices, reviews | [event administration pages](../../../../web/src/pages/admin/games/%5Bid%5D/) | [src/controllers/edit/games/](../../../../src/controllers/edit/games/), [src/controllers/edit/divisions.rs](../../../../src/controllers/edit/divisions.rs), [src/controllers/edit/notices.rs](../../../../src/controllers/edit/notices.rs), [src/controllers/edit/reviews.rs](../../../../src/controllers/edit/reviews.rs) |
| Challenge editing, flags, import/export, builds, preflight | [web/src/pages/admin/games/[id]/challenges/](../../../../web/src/pages/admin/games/%5Bid%5D/challenges/), [web/src/pages/admin/builds.tsx](../../../../web/src/pages/admin/builds.tsx) | [src/controllers/edit/challenges/](../../../../src/controllers/edit/challenges/), [src/controllers/edit/test_container/](../../../../src/controllers/edit/test_container/), [src/services/challenge_builds/](../../../../src/services/challenge_builds/), [src/controllers/edit/preflight.rs](../../../../src/controllers/edit/preflight.rs) |
| Repository bindings, scans, pushback, checker preparation | [web/src/pages/admin/repo-bindings.tsx](../../../../web/src/pages/admin/repo-bindings.tsx) | [src/controllers/admin/repo_bindings.rs](../../../../src/controllers/admin/repo_bindings.rs), [src/services/repo_binding_scheduler.rs](../../../../src/services/repo_binding_scheduler.rs), [src/services/git_sync/](../../../../src/services/git_sync/) |
| Platform users, teams, imports, credential actions | [web/src/pages/admin/Users.tsx](../../../../web/src/pages/admin/Users.tsx), [web/src/pages/admin/Teams.tsx](../../../../web/src/pages/admin/Teams.tsx) | [src/controllers/admin/users.rs](../../../../src/controllers/admin/users.rs), [src/controllers/admin/teams.rs](../../../../src/controllers/admin/teams.rs), [src/controllers/admin/users_import_history.rs](../../../../src/controllers/admin/users_import_history.rs) |
| Platform settings, login policy, branding, integrations | [web/src/pages/admin/Settings.tsx](../../../../web/src/pages/admin/Settings.tsx), [web/src/hooks/useConfig.ts](../../../../web/src/hooks/useConfig.ts) | [src/controllers/admin/settings.rs](../../../../src/controllers/admin/settings.rs), [src/services/oauth_config.rs](../../../../src/services/oauth_config.rs), [src/services/settings_branding.rs](../../../../src/services/settings_branding.rs) |
| Runtime instances, worker enrollment and diagnostics | [web/src/pages/admin/Instances.tsx](../../../../web/src/pages/admin/Instances.tsx), [web/src/pages/admin/workers.tsx](../../../../web/src/pages/admin/workers.tsx) | [src/controllers/admin/instances.rs](../../../../src/controllers/admin/instances.rs), [src/controllers/workers.rs](../../../../src/controllers/workers.rs), [src/services/worker/](../../../../src/services/worker/) |
| Admin dashboard, logs, metrics | [web/src/pages/admin/Dashboard.tsx](../../../../web/src/pages/admin/Dashboard.tsx), [web/src/pages/admin/Logs.tsx](../../../../web/src/pages/admin/Logs.tsx) | [src/controllers/admin/dashboard.rs](../../../../src/controllers/admin/dashboard.rs), [src/controllers/admin/logs.rs](../../../../src/controllers/admin/logs.rs), [src/controllers/admin/realtime.rs](../../../../src/controllers/admin/realtime.rs), [src/services/observability.rs](../../../../src/services/observability.rs) |
| Posts, About, guide, donations | [web/src/pages/posts/](../../../../web/src/pages/posts/), [web/src/pages/About.tsx](../../../../web/src/pages/About.tsx), [web/src/pages/guide/](../../../../web/src/pages/guide/), [web/src/pages/Donations.tsx](../../../../web/src/pages/Donations.tsx) | [src/controllers/info.rs](../../../../src/controllers/info.rs), [src/controllers/edit/posts.rs](../../../../src/controllers/edit/posts.rs), [src/controllers/donations.rs](../../../../src/controllers/donations.rs) |
| Assets, attachments, API tokens, image retention | [API client](../../../../web/src/Api.ts) | [src/controllers/assets.rs](../../../../src/controllers/assets.rs), [src/controllers/api_token.rs](../../../../src/controllers/api_token.rs), [src/services/image_storage.rs](../../../../src/services/image_storage.rs), [src/services/blob_refs.rs](../../../../src/services/blob_refs.rs) |

For every row, continue to [frontend-map.md](frontend-map.md),
[backend-map.md](backend-map.md), [runtime-map.md](runtime-map.md), or
[checks-map.md](checks-map.md) for shared dependencies and verification.

## Start from a symptom

For a local A&D source ZIP comparison, [patch-preview](../../../../src/cli/patch_preview.rs)
uses the [patch manifest](../../../../src/services/ad/patches.rs) and the same validated
archive reader as Docker builds. The [archive tests](../../../../src/services/challenge_builds/archive_tests.rs)
cover Git tree compatibility, mode changes, and equivalent repackaging.
The [private SSH comparison](../../../../src/controllers/game/ad/patch_preview/ssh_comparison.rs)
compares one saved ZIP with the registered paths in the team's latest SSH observation.
The same CLI module exports a saved tree through `patch-context`. Its
[CLI regression](../../../../tests/rsctf_cli.rs) checks the generated tar and
empty output for stale or damaged input.
The [BuildKit regression](../../../../scripts/test-ad-patch-context.py) builds a
fixture from a Git-generated tree ID and verifies the executable and its output.
Its `--guest` mode exercises the [offline VM worker](../../../../scripts/ad-patch-builder/build.py)
and [guest init](../../../../scripts/ad-patch-builder/init), including a failed
build, a pinned starter image, and preservation of the previous candidate.
The [Toolkit build control](../../../../web/src/components/competition/AdPatchBuildAction.tsx)
submits a saved commit through the [team-scoped build endpoint](../../../../src/controllers/game/ad/patch_preview/builds.rs).
The [challenge worker](../../../../scripts/ad-patch-builder/worker.py) claims the request and replaces
the private candidate after verification. The [PostgreSQL and VM regression](../../../../src/controllers/game/ad/patch_preview/tests/history/builds.rs)
checks ownership, replay, failure and candidate retention.
The [private image export](../../../../scripts/ad-patch-builder/export.py) selects one service,
revision and build attempt. Its [PostgreSQL regressions](../../../../scripts/ad-patch-builder/test_export.py)
cover corruption, replacement during transfer and competing output files.
The [registry staging command](../../../../scripts/ad-patch-builder/stage.py)
transfers the same build to its private service repository and records the verified digest.
Its challenge watcher resumes pending transfers using the same eligibility and receipt checks.
The [TLS registry test](../../../../scripts/ad-patch-builder/test_stage_registry.py) uses a real VM output and PostgreSQL.
The [retention command](../../../../scripts/ad-patch-builder/retain.py) pins database-referenced
manifests before offline collection. The [collection regression](../../../../scripts/ad-patch-builder/test_retention.py)
pulls the retained images from the restarted registry and checks that an obsolete image was reclaimed.
It exercises the [Docker digest pull](../../../../src/services/container/docker/image.rs) with the platform's
registry-specific pull account on an empty runtime cache.
The [team activation endpoint](../../../../src/controllers/game/ad/patch_preview/activation.rs)
pins a staged candidate to the current service. The [real Docker regression](../../../../src/controllers/game/ad/player_activation_tests.rs)
checks withdrawal before retirement, request replay, flag delivery and registry identity.
The [failed-build regression](../../../../src/services/challenge_builds/archive_tests/failed_candidate.rs)
checks the Docker tag and the running service's data across an interrupted build,
candidate cleanup, and retry. The [Docker transport](../../../../src/services/challenge_builds/docker_io.rs)
builds an untagged image and publishes the tag after verifying the build receipt.
The [starter ZIP editor](../../../../web/src/components/admin/AdPatchStarterInput.tsx)
sends an explicitly selected source archive to the [hosted challenge endpoint](../../../../src/controllers/edit/ad/patch_starter.rs).
The [SSH source registration](../../../../src/services/ad/patches/ssh_sources.rs) maps selected starter files to their running container paths before kickoff.
The [hosted SSH observer](../../../../src/services/ad/patches/ssh_observer.rs) records scoped file identities while the team shell is active. Its [Docker regression](../../../../src/services/ad/patches/ssh_observer/tests.rs) checks repeated edits, service replacement, and separation between teams.
The [SSH recovery download](../../../../src/controllers/game/ad/patch_preview/ssh_recovery.rs) reconstructs the owning team's starter ZIP with the registered files retained from a shell session.
The [starter image migration](../../../../src/migrations/m0371_ad_patch_starter_images.rs)
pairs the source with the organizer's pregame build and protects local images during cleanup.
Its [HTTP and PostgreSQL regression](../../../../src/controllers/edit/ad/patch_starter/tests.rs)
exercises replay, replacement, simultaneous registration, and game ownership.
The [per-service image history](../../../../src/migrations/m0372_ad_service_image_history.rs)
records the current and previous distinct image at endpoint publication. The
[runtime regression](../../../../src/services/ad/reset_runtime_tests.rs) checks reconciliation
across an interrupted reset and the [publication tests](../../../../src/services/ad/service_lifecycle_tests.rs)
cover retention and older servers.
The [organizer policy](../../../../src/controllers/edit/ad/patch_activity.rs) controls the [delayed deployment counts and closed-game archive](../../../../src/controllers/game/ad/patch_preview/activity.rs). The [archive panel](../../../../web/src/components/competition/AdPatchArchive.tsx) displays marked starter paths for revisions that reached publication. The [HTTP regression](../../../../src/controllers/game/ad/patch_preview/tests/activity.rs) checks round finalization, practice mode, division visibility, and pagination.
For files marked by the organizer, the [public diff](../../../../src/services/ad/patches/public_diff.rs) computes bounded line ranges when the team saves the ZIP. Source text stays in the team's private revision.

The [worker flag delivery receipt](../../../../src/services/worker/registry/flag_delivery.rs)
binds file readback to the request and worker session. Its
[regressions](../../../../src/services/worker/registry/flag_delivery/tests.rs)
cover partial replica failure, accepted commands, timeouts, and reconnects.
The [round publisher](../../../../src/services/cron/round_finish/worker_delivery.rs)
checks the official value, team assignment and worker session before and after readback.
Its [scoring regression](../../../../src/services/cron/round_finish/worker_delivery/tests.rs)
publishes two team receipts in PostgreSQL and checks replay and stale assignments.
The [placement regression](../../../../src/services/worker/postgres/health_tests/ad_placement.rs)
reconnects a worker with and without verified flag delivery and team storage, then checks assignment retention, routing, and cleanup.
The [Linux worker round test](../../../../src/services/cron/round_finish/worker_delivery/tests/live_agent.rs) reads each team's delivered file, replays the round and checks HTTP health over an actual worker stream. [Stream admission](../../../../src/services/worker/postgres/validation.rs) checks the current published team service before a new A&D connection opens.
The [team worker ownership check](../../../../src/services/ad/service_lifecycle/worker_owner.rs) locks the assignment while its service records an inactive backend or publishes an endpoint. It also checks the owner before reset or teardown can destroy a worker workload.
The [reset transaction](../../../../src/services/ad/engine/service_reset.rs) checks that ownership before retiring a service, recording its replacement or publishing the new endpoint.

The [private revision history](../../../../src/controllers/game/ad/patch_preview/revisions.rs)
stores [content-addressed source](../../../../src/services/ad/patches/revisions.rs) for one hosted team service.
The [revision download](../../../../src/controllers/game/ad/patch_preview/source.rs) checks team access
and [reconstructs the ZIP](../../../../src/services/ad/patches/checkout.rs) from the pinned starter and verified file contents.
The [player preview panel](../../../../web/src/components/competition/AdPatchPreviewPanel.tsx) and
the [A&D Toolkit](../../../../web/src/components/AdGuideModal.tsx) share the same challenge entry point. The
[player routes](../../../../src/controllers/game/ad/patch_preview.rs) handle the download
and comparison against the starter. The [PostgreSQL test](../../../../src/controllers/game/ad/patch_preview/tests.rs)
covers revocation during upload and division access. The [revision tests](../../../../src/controllers/game/ad/patch_preview/tests/history.rs)
also cover private comparison baselines and a teammate saving during an upload. The [browser test](../../../../web/e2e/ad-patch-starter.spec.mjs)
exercises the organizer and player flows.

For security-sensitive changes, use the initial
[route → policy → regression index](security-boundaries.md). Its scope and missing
end-to-end evidence are explicit.

| Symptom | First trace | First useful check |
| --- | --- | --- |
| Discord returns to login; cookie absent | OAuth callback → response cookie helpers → proxy/cookie config → browser profile read | OAuth integration tests and account-access browser suite |
| Wrong account's data, stale modal, canceled request shows error | App's viewer/route boundary → SWR key/provider → owning hook | Cache/identity utility tests; account/team browser suites |
| Scoreboard stale or slow | Page hook → API page/full route → visibility/cache key → canonical SQL/rebuild | Scoreboard PostgreSQL integrity tests, then fixed-rate page harness |
| Import/build stays pending | Enqueue path → durable row/lease → actual role startup → runtime/image adapter | Worker startup tests and real queue/restart regression |
| Container exists but UI says failed | Operation ID → stored backend ID → adapter lookup/retry → response/cache invalidation | Real Docker retry/adoption test, not a mock-only status test |
| BYOC connects intermittently | Reverse-proxy stateful route → singleton owner → tunnel registry → revocation lease | BYOC lifecycle harness and role/readiness checks |
| UI works but API returns 401/403 | Authentication extractor → game/team membership and division/freeze policy | Real PostgreSQL negative-authorization test |
| UI blank or controls unusable on mobile | Route → shared layout → error boundary → locale/theme/layout | Strict type/lint, focused browser fixture and Axe/overflow audit |
| Memory or disk grows | Active vs inactive client cache; L1/L2; retained blobs vs working captures/images/logs | Resource scenario in load README; measure before claiming savings |
| CI or release fails | Failing job + exact source SHA → local equivalent → CI planner/publication gate | Checks map; reproduce exact failing phase locally |

## Trace across layers

Normal read: generated browser route → page/component → hook → client/SWR →
Axum router → authentication/policy → controller/domain code → PostgreSQL/cache →
DTO/serialized body → viewer-scoped presentation.

Slow mutation: authorize request → durable operation/job where implemented →
eligible consumer/lease → runtime or repository work → durable receipt →
invalidation/live hint → status read. Not every lifecycle operation is delegated:
web still has direct runtime access in supported Docker profiles.

Live updates: commit authoritative state → feed/outbox or event bus → hub →
recovering frontend hook → API read/backfill. A browser fixture can prove rendering;
it cannot prove backend authorization, runtime connectivity, or durable recovery.

## Current source versus planned work

Read [TODO.md](../../../../TODO.md) and [IMPROVEMENTS.md](../../../../IMPROVEMENTS.md) for scope and evidence, then verify the
implementation. This checkout includes service extraction, response-cache sharding,
incremental OpenAPI generation, certificate renewal, and centralized worker wiring.
Privileged web delegation, capture offload, full runtime shutdown/fairness testing,
and kernel network recovery remain open. A component test does not complete those
parent tasks. A branch implementation is not evidence of a production deployment.

When changing structure, update this feature index, the relevant layer/check map,
and any role/proxy documentation affected. Validate all skill links:

```sh
node .agents/skills/rsctf-engineering/scripts/check-navigation.mjs
```
