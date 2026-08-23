# Frontend quality and accessibility

## Product and code fit

- Use the existing Mantine, CSS-module, navigation, page-header, API-hook, locale, and
  theme patterns before creating a new abstraction.
- Derive behavior from the effective `/api/config` and game DTOs. Do not hard-code a
  login method, connection mode, donation provider, VPN behavior, event type, or
  registration policy that can differ by deployment or event.
- Keep server authorization authoritative. Protected routes should redirect or show a
  clear authenticated state, but must not rely on route hiding to protect data.
- Reuse SWR keys and mutate the correct cache after writes. Avoid a second profile or
  config cache with different semantics.

## WCAG 2.1 AA baseline

- Use semantic landmarks and one visible `h1`. Keep heading levels sequential.
- Every control needs an accessible name, keyboard operation, a visible focus state,
  and an adequate target. Use live regions for asynchronous result counts/statuses.
- Dialogs need a programmatic title, focus trap, Escape/close behavior, scroll lock,
  and focus restoration. Avoid nested banner/main landmarks inside dialogs.
- Error states must be textual, not color-only. Decorative icons/images use
  `aria-hidden`; informative screenshots have concise meaningful alt text.
- Preserve text contrast (4.5:1 for ordinary text) and UI-boundary contrast (3:1).
  Test configured accent colors through the existing contrast-safe theme tokens.
- Respect `prefers-reduced-motion`; never make animation necessary to understand or
  operate a feature.

## Responsive and visual fit

- Validate at 320x568, 390x844, tablet, desktop, and relevant wide/ultrawide sizes.
- Avoid horizontal page overflow, clipped labels, icon-induced layout shifts, and
  fixed widths that crowd localized text. Prefer compact, wrapping toolbars.
- The mobile bottom dock must not hide the final reachable content. Scroll regions
  need keyboard access and an accessible name.
- Match the platform’s dense competition-workspace theme; do not introduce a separate
  visual language for one page.

## Frontend evidence

Run at minimum:

```sh
cd web
pnpm check
pnpm lint:check
pnpm test
pnpm build
```

For changed pages/dialogs, run `tests/visual/audit.mjs` at desktop and mobile/compact,
inspect its screenshots, and require zero relevant Axe violations, overflow, unnamed
controls, heading skips, and runtime errors. Add a pure/source regression test under
`web/src/**/*.test.ts` when DOM infrastructure is unnecessary.
