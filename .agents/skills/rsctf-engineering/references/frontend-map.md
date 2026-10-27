# Frontend ownership and discovery

Start with the feature row in [application-map.md](application-map.md). Read
[frontend-quality.md](frontend-quality.md) for required UI/accessibility rules.

## Routes and composition

[web/vite.config.mts](../../../../web/vite.config.mts) configures `vite-plugin-pages` for `web/src/pages/**/*.tsx`.
[web/src/App.tsx](../../../../web/src/App.tsx) imports `~react-pages`, renders `useRoutes`, and composes the
theme, notifications, dialogs, WSRX, guide, viewer identity and SWR providers.
An `Index.tsx` represents its directory, brackets represent parameters, and
`[...all].tsx` is the fallback. Sibling `.ts` helpers and CSS files are not pages.

| Browser surface | Source directory | Shared ownership to inspect |
| --- | --- | --- |
| `/`, `/about`, `/donations`, `/posts`, `/guide` | [web/src/pages/](../../../../web/src/pages/) | [web/src/components/WithNavbar.tsx](../../../../web/src/components/WithNavbar.tsx), [web/src/components/PageHeader.tsx](../../../../web/src/components/PageHeader.tsx) |
| `/account/*`, `/teams` | [web/src/pages/account/](../../../../web/src/pages/account/), [web/src/pages/Teams.tsx](../../../../web/src/pages/Teams.tsx) | [web/src/components/AccountView.tsx](../../../../web/src/components/AccountView.tsx), [web/src/hooks/useUser.tsx](../../../../web/src/hooks/useUser.tsx) |
| `/games`, `/challenges` | [web/src/pages/games/](../../../../web/src/pages/games/), [web/src/pages/challenges/](../../../../web/src/pages/challenges/) | [web/src/components/GameCard.tsx](../../../../web/src/components/GameCard.tsx), [web/src/components/ChallengeCard.tsx](../../../../web/src/components/ChallengeCard.tsx) |
| `/games/:id/*` | [web/src/pages/games/[id]/](../../../../web/src/pages/games/%5Bid%5D/) | [web/src/components/WithGameTab.tsx](../../../../web/src/components/WithGameTab.tsx), [web/src/components/WorkspaceBar.tsx](../../../../web/src/components/WorkspaceBar.tsx), [web/src/components/competition/](../../../../web/src/components/competition/) |
| `/games/:id/monitor/*` | [web/src/pages/games/[id]/monitor/](../../../../web/src/pages/games/%5Bid%5D/monitor/) | [web/src/components/WithGameMonitor.tsx](../../../../web/src/components/WithGameMonitor.tsx), [web/src/components/monitor/](../../../../web/src/components/monitor/) |
| `/admin/*` | [web/src/pages/admin/](../../../../web/src/pages/admin/) | [web/src/components/admin/WithAdminTab.tsx](../../../../web/src/components/admin/WithAdminTab.tsx), [web/src/components/admin/](../../../../web/src/components/admin/) |
| `/admin/games/:id/*` | [web/src/pages/admin/games/[id]/](../../../../web/src/pages/admin/games/%5Bid%5D/) | [web/src/hooks/useEdit.ts](../../../../web/src/hooks/useEdit.ts), [web/src/components/WithRole.tsx](../../../../web/src/components/WithRole.tsx) |

List every currently discovered page using the existing visual route catalog:

```sh
node --input-type=module -e 'import {discoverPageRoutes} from "./tests/visual/route-catalog.mjs"; for (const r of discoverPageRoutes({gameId:1,challengeId:1,postId:"example"})) console.log(r.path+"\tweb/src/pages/"+r.sourceFile)'
```

Those IDs are placeholders for discovery, not seeded records. The catalog's
fixture auth classification is not backend permission enforcement. For actual
route matching, follow Vite's configuration and React Router.

## Shared concerns

| Concern | Owning files/directories |
| --- | --- |
| API types, fetcher, generated/manual hooks | [web/src/Api.ts](../../../../web/src/Api.ts), [web/template/](../../../../web/template/), [web/package.json](../../../../web/package.json) |
| Effective deployment config and user | [web/src/hooks/useConfig.ts](../../../../web/src/hooks/useConfig.ts), [web/src/hooks/useUser.tsx](../../../../web/src/hooks/useUser.tsx) |
| SWR retention and weight estimation | [web/src/utils/Cache.ts](../../../../web/src/utils/Cache.ts), [web/src/utils/CacheWeight.ts](../../../../web/src/utils/CacheWeight.ts), [web/src/utils/BoundedCacheMiddleware.ts](../../../../web/src/utils/BoundedCacheMiddleware.ts) |
| Viewer retirement, late responses, route changes | [web/src/utils/ViewerIdentity.tsx](../../../../web/src/utils/ViewerIdentity.tsx), [web/src/utils/AuthState.ts](../../../../web/src/utils/AuthState.ts), [web/src/utils/ProfileRead.ts](../../../../web/src/utils/ProfileRead.ts) |
| Login completion, safe return URL, OAuth failure | [web/src/utils/CompleteSignIn.ts](../../../../web/src/utils/CompleteSignIn.ts), [web/src/utils/AccountNavigation.ts](../../../../web/src/utils/AccountNavigation.ts), [web/src/utils/OAuthFailure.ts](../../../../web/src/utils/OAuthFailure.ts) |
| Live-feed recovery and bounded polling | [web/src/hooks/useRecoveringHub.ts](../../../../web/src/hooks/useRecoveringHub.ts), [web/src/hooks/useChallengePolling.ts](../../../../web/src/hooks/useChallengePolling.ts), [web/src/hooks/useScoreboardLiveRefresh.ts](../../../../web/src/hooks/useScoreboardLiveRefresh.ts) |
| URL/tab/hash ownership and prefetch | [web/src/hooks/useUrlState.ts](../../../../web/src/hooks/useUrlState.ts), [web/src/hooks/useUrlTab.ts](../../../../web/src/hooks/useUrlTab.ts), [web/src/components/RoutePrefetcher.tsx](../../../../web/src/components/RoutePrefetcher.tsx) |
| Shell, desktop/mobile navigation | [web/src/components/AppHeader.tsx](../../../../web/src/components/AppHeader.tsx), [web/src/components/AppNavbar.tsx](../../../../web/src/components/AppNavbar.tsx), [web/src/components/navigation.ts](../../../../web/src/components/navigation.ts), [web/src/components/mobile/](../../../../web/src/components/mobile/) |
| Theme, CSS and localization | [web/src/utils/ThemeOverride.ts](../../../../web/src/utils/ThemeOverride.ts), [web/src/styles/](../../../../web/src/styles/), [web/src/locales/](../../../../web/src/locales/), [web/src/utils/I18n.tsx](../../../../web/src/utils/I18n.tsx) |
| Reusable failure and modal behavior | [web/src/components/RequestError.tsx](../../../../web/src/components/RequestError.tsx), [web/src/components/ErrorFallback.tsx](../../../../web/src/components/ErrorFallback.tsx), [web/src/components/AccessibleModal.tsx](../../../../web/src/components/AccessibleModal.tsx) |
| Guidance, Markdown, diagrams and PDF | [web/src/components/guide/](../../../../web/src/components/guide/), [web/src/components/MarkdownRenderer.tsx](../../../../web/src/components/MarkdownRenderer.tsx), [web/src/components/admin/](../../../../web/src/components/admin/) |

Before introducing a library or helper, inspect [web/package.json](../../../../web/package.json) and nearby
usage: Mantine for UI/dialogs, SWR for reads, React Router for navigation, i18next
for languages, SignalR for hubs, DOMPurify/marked for Markdown, react-pdf/pdfjs for
PDF, ECharts for charts, Pixi for the arena, xterm/WSRX for terminals/tunnels.
A dependency's presence alone does not prove it fits a new use case.

## Find the exact caller and check it

```sh
rg -n 'useScoreboardPage|api/game/.*/scoreboard' web/src
rg --files web/src | rg 'Cache|Identity|Scoreboard|Login'
rg -n 'mutate\(|useRecoveringHub|useSWR' web/src/hooks web/src/components
```

Quote bracketed filenames in shell commands. Keep profile/config reads on the
shared provider; verify viewer/game dimensions before reusing a key. Read the
[checks map](checks-map.md) for focused utility tests and the matching browser
suite. Fixture-only browser success does not replace backend permission tests.
