/**
 * Best-effort, module-level belief about whether the visitor currently has an
 * authenticated session.
 *
 * The global 401 interceptor (see App.tsx `authAwareFetcher`) uses this to tell
 * two very different 401s apart:
 *
 *   1. A genuine session expiry — a logged-in user whose cookie lapsed
 *      mid-session. Here we DO want to bounce them to the login page.
 *   2. An anonymous visitor on an otherwise-PUBLIC page (e.g. a game
 *      scoreboard) whose page happens to fire an optional [RequireUser]
 *      enrichment fetch like GET /game/{id}/details. That 401 is expected and
 *      must NOT redirect — the public view should just render.
 *
 * Without this distinction, every public page that fetches team-scoped data
 * forced logged-out users to the login screen.
 *
 * The flag is seeded by `useUser` from the /account/profile probe (the single
 * source of truth for "am I logged in"): set true once a profile loads, false
 * once that probe 401s. It resets to false on every full page load, so a fresh
 * anonymous visit never redirects; an in-session SPA expiry still does, because
 * the earlier successful profile load already set it true.
 */
let authed = false

export const setAuthSession = (value: boolean): void => {
  authed = value
}

export const hasAuthSession = (): boolean => authed

export interface UnauthorizedRedirectContext {
  /** HTTP status of the failed request. */
  status?: number
  /** The request path that failed (first arg to the swagger fetcher). */
  requestPath: string
  /** Current window location pathname. */
  pathname: string
  /** Guard so the redirect fires at most once per navigation. */
  redirectInFlight: boolean
  /** Whether a session is believed to exist; defaults to the live flag. */
  hasSession?: boolean
}

/**
 * Pages that anyone (logged out) may view. A 401 from an optional [RequireUser]
 * enrichment fetch on one of these must NEVER bounce to login — not even when a
 * session was believed to exist but has since expired. The visitor just sees the
 * public (logged-out) view and can re-login from the navbar. Protected pages
 * (challenges, submit, monitor, admin, account management, team management) are
 * NOT listed, so a genuine session expiry there still redirects.
 */
const PUBLIC_PAGE_PATTERNS: RegExp[] = [
  /^\/$/, // home
  /^\/games\/?$/, // games list
  /^\/games\/\d+\/?$/, // game landing
  /^\/games\/\d+\/scoreboard\/?$/, // public scoreboard
  /^\/posts(\/|$)/, // posts list + detail
  /^\/about\/?$/, // about
]

export const isPublicPage = (pathname: string): boolean =>
  PUBLIC_PAGE_PATTERNS.some((re) => re.test(pathname))

/**
 * Pure decision for the global fetcher: should a failed request bounce the
 * visitor to the login page? Only a real session expiry (we believe a session
 * exists) on a non-auth endpoint of a NON-PUBLIC, non-account page qualifies.
 * Anonymous visitors — and expired-session visitors on a public page like the
 * game landing or scoreboard — render the public view instead of redirecting.
 */
export const shouldRedirectOnUnauthorized = (ctx: UnauthorizedRedirectContext): boolean => {
  const { status, requestPath, pathname, redirectInFlight } = ctx
  const hasSession = ctx.hasSession ?? authed
  const isAuthEndpoint = requestPath.includes('/account/') || requestPath.includes('/info')
  return (
    status === 401 &&
    hasSession &&
    !redirectInFlight &&
    !isAuthEndpoint &&
    !pathname.startsWith('/account/') &&
    !isPublicPage(pathname)
  )
}
