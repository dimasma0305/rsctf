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
