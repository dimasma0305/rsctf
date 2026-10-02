# Full-page visual audit

The visual audit renders every React page component at ultrawide (3440×1440),
wide desktop (1920×1080), desktop (1440×1100), notebook (1366×768), laptop
(1024×768), tablet (768×1024), mobile (390×844), and compact mobile (320×568)
sizes. For every render it saves the actual viewport plus an expanded
full-content screenshot that opens nested vertical scroll regions. It runs axe,
checks responsive overflow, viewport escapes, declared section spacing and row
limits, heading structure, control names and target spacing, and records browser
exceptions and HTTP 5xx responses.

Generated artifacts live in `/visual-audit-output` and are excluded from Git and
the Docker build context.

## Refined content pages

The About, Posts, Guide, and event Readiness pages share a compact, content-first
layout. Their existing browser harnesses include regressions against oversized
headings, decorative feed cards, and an oversized walkthrough banner.

For the full-content audit, start the preview on `127.0.0.1:63017` and run
`node tests/visual/refined-pages-fixtures.mjs --serve` in a separate terminal.
This loopback fixture server blocks writes and never forwards API or hub calls.

```sh
RSCTF_VISUAL_TARGET=http://127.0.0.1:63018 \
RSCTF_VISUAL_ADMIN_JWT=local-fixture-not-a-credential \
RSCTF_VISUAL_GAME_ID=19 \
scripts/bounded-frontend.sh exec node ../tests/visual/audit.mjs \
  --page =about --page =posts--index --page =guide--index \
  --page =admin--games--game--readiness \
  --viewport desktop --viewport tablet --viewport mobile --viewport compact
```

These are rendering/interaction fixtures, not evidence of backend permissions or
runtime health. Stop the fixture server and preview when finished.

## Home, event catalog, and admin dashboard

`overview-pages.mjs` checks the content-first Home feed, compact event filters,
and Dashboard totals. Start the loopback preview on port 63017, then run:

```sh
scripts/bounded-frontend.sh exec node ../tests/visual/overview-pages.mjs
```

It covers 320px through 1920px, Indonesian/light mode, reduced motion, keyboard
navigation, search/clear/membership filters, dashboard refresh and activity tabs,
empty/loading/error/retry states, charts, and guest controls. Layout regressions
require useful content near the top and mobile totals above the bottom dock.
Posterless event cards must retain distinct colors in both themes, preserve their
color after filtering, and keep opaque high-contrast ID labels.
All API/hub traffic is intercepted, and every mutation is blocked. This is not a
backend authorization test. The onboarding guide is disabled in these fixtures;
its dedicated guide harness tests onboarding separately.
Theme and guide setup run only in the selected origin's top-level document, never
inside child or opaque sandbox frames. Font loading and keyboard focus must settle
before inspection; application runtime exceptions still fail the audit.

`RSCTF_OVERVIEW_TARGET=https://intechfest.1pc.tf` or `https://tcp.1pc.tf` tests
released frontend assets with the same isolated data. `RSCTF_OVERVIEW_OUTPUT`
sets the evidence directory (default `visual-audit-output/overview-local/`).

For full-content screenshots and layout checks, run
`node tests/visual/overview-pages-fixtures.mjs --serve` on loopback port 63018:

```sh
RSCTF_VISUAL_TARGET=http://127.0.0.1:63018 \
RSCTF_VISUAL_ADMIN_JWT=local-fixture-not-a-credential \
scripts/bounded-frontend.sh exec node ../tests/visual/audit.mjs \
  --page =index --page =games--index --page =admin--dashboard \
  --viewport desktop --viewport tablet --viewport mobile --viewport compact
```

Stop the fixture server and preview afterward. Never supply a real credential
to the fixture server.

## Challenge card hash links

`node tests/visual/challenge-links.mjs` checks the built client at
`http://127.0.0.1:18080`: catalog and event card/list links, keyboard activation,
focus restoration, copy/reload, Back/Forward, filtered and off-page challenges,
unavailable/failed links, late responses after close, and login return URLs.
It runs Axe and overflow checks at 320, 390, 768, 1440, and 1920 pixels.
All API/hub traffic uses invented fixtures; writes are blocked. Backend access
boundaries are tested separately against PostgreSQL in `catalog_tests.rs`.

Set `RSCTF_CHALLENGE_LINK_TARGET=https://tcp.1pc.tf` to check released assets and
`RSCTF_CHALLENGE_LINK_OUTPUT` to select the evidence directory. For the general
full-content audit, run `node tests/visual/challenge-links-fixtures.mjs --serve`,
then:

```sh
RSCTF_VISUAL_TARGET=http://127.0.0.1:63019 \
RSCTF_VISUAL_PLAYER_JWT=local-fixture-not-a-credential \
RSCTF_VISUAL_GAME_ID=901 \
scripts/bounded-frontend.sh exec node ../tests/visual/audit.mjs \
  --page =challenges--index --page =games--game--challenges \
  --viewport desktop --viewport compact
```

Stop the fixture proxy after the audit. Never supply it with real credentials.

## Section and tab hash links

`node tests/visual/url-navigation.mjs` checks hash-backed sections, tabs, and
challenge views, with legacy query bookmarks retained for compatibility. Examples
include `#section=abnormal-solves`, `#tab=stats`, and
`#snapshot=2&snapshotTab=history`. Existing challenge title-slug fragments can
coexist with tabs, such as `#9001-Ret2win&category=Pwn&view=list`.

The read-only fixtures cover reload, Back/Forward, keyboard navigation, preserving
unsaved settings/profile/event drafts, nested snapshot/card selection, login
return links, and retaining the anti-cheat report's existing request owner.
Set `RSCTF_URL_NAV_TARGET` to the released origin and `RSCTF_URL_NAV_OUTPUT` to
select the evidence directory. No form drafts or credentials are put in URLs.

## Event readiness fixtures

Start a frontend preview on `127.0.0.1:63017`, then run from the repository root:

```sh
scripts/bounded-frontend.sh exec node ../tests/visual/event-readiness.mjs
```

The harness checks the saved-configuration checklist, attention-first ordering,
manual refresh, cached-data warnings, permission revocation, empty events, and
action links. It covers desktop, 320px, Indonesian/light mode, reduced motion,
keyboard activation, touch targets, Axe, and browser errors. Every API/hub request
is intercepted; all writes are rejected by the fixtures. This does not test real
checker health, team acceptance, backend authorization, or network connectivity.

Use `RSCTF_READINESS_TARGET=https://tcp.1pc.tf` or
`https://intechfest.1pc.tf` to check released frontend assets against the same
isolated fixtures. Reports and screenshots go to
`visual-audit-output/event-readiness/`; `RSCTF_READINESS_OUTPUT` overrides it.

## Required context

Dynamic routes need one existing game, challenge, and post. Protected routes
need short-lived admin and participant JWTs. Prefer token files so credentials
never appear in shell history or the process list:

```sh
export RSCTF_VISUAL_TARGET=https://ctf.example
export RSCTF_VISUAL_GAME_ID=67
export RSCTF_VISUAL_CHALLENGE_ID=326
export RSCTF_VISUAL_POST_ID=ffac23df
export RSCTF_VISUAL_ADMIN_JWT_FILE=/run/secrets/rsctf-visual-admin.jwt
export RSCTF_VISUAL_PLAYER_JWT_FILE=/run/secrets/rsctf-visual-player.jwt
pnpm --dir web visual:audit
```

Set `CHROME_BIN` when Chrome or Chromium is not on `PATH`.

Useful filters:

```sh
pnpm --dir web visual:audit --list
pnpm --dir web visual:audit --page admin--builds
pnpm --dir web visual:audit --shard 1/2
pnpm --dir web visual:audit --shard 2/2
pnpm --dir web visual:audit --viewport ultrawide --viewport wide
pnpm --dir web visual:audit --viewport notebook --viewport laptop --viewport tablet
pnpm --dir web visual:audit --desktop-only
pnpm --dir web visual:audit --mobile-only
```

Route shards are deterministic, contiguous slices of the filtered route
catalog. They are useful when a browser runner has a short process timeout;
running every shard covers each selected route exactly once.

The audit exits non-zero for accessibility, responsive-layout, browser-runtime,
server-5xx, screenshot truncation, and route-rendering failures. Review
`report.md`, `report.json`, and `gallery.html` together: automated checks catch
structural regressions while the paired viewport/full-content gallery is the
operator's visual review surface.

## Local workspace rework fixtures

`workspace-rework.mjs` exercises the shared player/admin shell without accounts
or event mutations. Start a separate frontend preview on `127.0.0.1:63017`, then
run from the repository root:

```sh
scripts/bounded-frontend.sh exec node ../tests/visual/workspace-rework.mjs
```

`RSCTF_WORKSPACE_PREVIEW` can select another loopback preview URL. The harness
intercepts API requests in Chromium, serves invented event/team data, and blocks
mutations. It is a layout and interaction test, **not** backend authorization,
scoring, upload delivery, or realtime integration evidence.

Coverage includes dashboard, event administration, users, settings, teams,
profile, challenge browsing, and scoreboard layouts; quick-navigation search
and focus restoration; settings keyboard navigation; challenge search/reset;
and the writeup dialog. It checks 320px through 1920px layouts, Indonesian copy,
light mode, and reduced motion. Screenshots and Axe/overflow/runtime findings
are saved under `visual-audit-output/rework-workspaces/`. The public-route audit
above remains necessary against the same candidate.

## Competition globe and list fixtures

`competition-workspace.mjs` uses the same isolated loopback preview and intercepts
all API calls. Its 100-challenge fixture covers category clustering, bounded globe
pages, keyboard selection, search/reset, persistent Globe/List/Cards preferences,
the desktop detail panel, and mobile dialogs. It also checks that an inaccessible
event and an unowned URL hash cannot issue a challenge-detail read. These are
client behavior checks, not a replacement for backend authorization tests.

```sh
scripts/bounded-frontend.sh exec node ../tests/visual/competition-workspace.mjs
```

Screenshots and the request/Axe/overflow report are written to
`visual-audit-output/competition/`. The globe draws only after interaction or a
theme change; it does not own a poll, idle animation loop, or score calculation.
Both desktop views reuse the existing challenge actions and their access gates.

Cards are the default unless a player has saved another view. The harness also
checks multi-word/category search, clear-search focus, shared card/list sorting,
rejected attempts under the unsolved filter, and compact practice-event headers.
`RSCTF_WORKSPACE_PREVIEW=https://tcp.1pc.tf` checks the published assets with the
same browser-intercepted fixtures; no real player data or writes are used.

For the standard full-page audit, start
`node tests/visual/competition-fixtures.mjs --serve` alongside the preview. It
serves a practice event on loopback port 63018 and blocks every API mutation:

```sh
RSCTF_VISUAL_TARGET=http://127.0.0.1:63018 \
RSCTF_VISUAL_PLAYER_JWT=local-fixture-not-a-credential \
RSCTF_VISUAL_GAME_ID=901 \
scripts/bounded-frontend.sh exec node ../tests/visual/audit.mjs \
  --page =/games/901/challenges --viewport desktop --viewport mobile --viewport compact \
  --output visual-audit-output/challenge-full
```

Stop both temporary servers afterward. Never supply real credentials to fixtures.

## Repository and build administration fixtures

With a frontend preview on `127.0.0.1:63017`, run:

```sh
scripts/bounded-frontend.sh exec node ../tests/visual/admin-operations.mjs
```

This checks repository cards, pagination, add/history dialogs, build search and
status filters, build details without a log, image inventory, and loading/error/
empty/active states. It covers 320px through 1920px, Indonesian copy, light mode,
reduced motion, keyboard controls, Axe, overflow, and browser exceptions. All API
requests are intercepted; repository scans, builds, upstream pushes, and deletes
are blocked. This is UI evidence, not backend or build-worker integration evidence.

Artifacts go to `visual-audit-output/admin-ops-local/`. `RSCTF_ADMIN_OPS_OUTPUT`
overrides that path. `RSCTF_ADMIN_OPS_TARGET=https://intechfest.1pc.tf` checks the
deployed preview's assets with the same isolated API fixtures.

For the standard full-content audit, run
`node tests/visual/admin-operations-fixtures.mjs --serve` in a separate terminal.
It binds only to `127.0.0.1:63018`, serves invented read-only admin data, and forwards
static asset reads to port 63017. Then run:

```sh
RSCTF_VISUAL_TARGET=http://127.0.0.1:63018 \
RSCTF_VISUAL_ADMIN_JWT=local-fixture-not-a-credential \
scripts/bounded-frontend.sh exec node ../tests/visual/audit.mjs \
  --page =admin--repo-bindings --page =admin--builds \
  --viewport desktop --viewport tablet --viewport mobile --viewport compact
```

Stop the fixture server and preview when finished. Never use a real credential
for the fixture server.

## A&D / KotH operator console

`ad-operations.mjs` checks both modes of `/admin/games/19/adops` against invented,
browser-intercepted data. It covers responsive/light/Indonesian views, local team
and challenge/status filters, BYOC action visibility, hidden flags, keyboard grid
scrolling, snapshot inspection, cancelled resets, and loading/error/event timing
states. No reset, scoring change, shell session, verdict override, or credential
mutation is allowed.

```sh
scripts/bounded-frontend.sh exec node ../tests/visual/ad-operations.mjs
```

Use the loopback preview on port 63017 as above. For the standard full-page audit,
`node tests/visual/ad-operations-fixtures.mjs --serve` supplies the same read-only
fixtures on port 63018; select `/admin/games/19/adops` with game ID 19. Stop the
temporary servers afterward. `RSCTF_AD_OPS_TARGET=https://intechfest.1pc.tf` and
`--screens-only` validate deployed assets without contacting the real admin API.
`RSCTF_AD_OPS_OUTPUT` overrides the default `visual-audit-output/ad-ops-local/`.
This does not replace backend authorization, checker, lifecycle, or load tests.

## Administration navigation

`admin-navigation.mjs` exercises the shared admin shell, grouped event sections,
challenge/flags switching, settings deep links, browser Back, retained settings
drafts, keyboard page search, manager navigation, and the admin mobile dock.
All API traffic is intercepted with invented read-only fixtures, including when
the target is the deployed Intechfest preview. No admin mutation is allowed.

```sh
scripts/bounded-frontend.sh exec node ../tests/visual/admin-navigation.mjs
```

Use the loopback preview on port 63017. For the standard full-page audit,
`node tests/visual/admin-navigation-fixtures.mjs --serve` provides a read-only
proxy on port 63018. Stop both temporary services after testing. Never supply a
real credential to the fixture proxy.

`--interactions-only` shortens the local feedback loop; the final run must include
all layouts. `RSCTF_ADMIN_NAV_TARGET=https://intechfest.1pc.tf` with `--screens-only`
checks deployed rendering. Set `RSCTF_ADMIN_NAV_OUTPUT` for a separate output
directory. These checks do not certify backend authorization or admin operations.

## AI chat links

`ai-chat-links.mjs` renders the solved-challenge AI chat section, the admin
provider registry (**Settings → AI links**), and the monitor list against
loopback fixtures at desktop, 320 px, and a light-theme phone layout. It
requires zero Axe violations, overflow, and browser exceptions, proves a King of
the Hill challenge never offers the section, and checks the save request body.
Built-in provider patterns come from `src/services/ai_chat_links.rs`, so the
browser matcher and the server cannot drift.

```sh
RSCTF_AI_CHAT_TARGET=http://127.0.0.1:63017 node tests/visual/ai-chat-links.mjs
# also refresh the README/docs screenshots
RSCTF_AI_CHAT_PUBLISH=1 node tests/visual/ai-chat-links.mjs
```
