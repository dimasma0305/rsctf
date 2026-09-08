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
