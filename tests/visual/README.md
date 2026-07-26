# Full-page visual audit

The visual audit renders every React page component in both desktop and mobile
viewports. It saves screenshots, runs axe, checks responsive overflow, heading
structure, control names and target spacing, and records browser exceptions and
HTTP 5xx responses.

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
pnpm --dir web visual:audit --desktop-only
pnpm --dir web visual:audit --mobile-only
```

The audit exits non-zero for accessibility, responsive-layout, browser-runtime,
server-5xx, and route-rendering failures. Review `report.md`, `report.json`, and
`gallery.html` together: automated checks catch structural regressions while
the screenshot gallery is the operator's visual review surface.
