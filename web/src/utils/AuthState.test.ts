import assert from 'node:assert/strict'
import { test } from 'node:test'
import { hasAuthSession, setAuthSession, shouldRedirectOnUnauthorized } from './AuthState'

// A PROTECTED page (challenges) is the base — a genuine session expiry here
// should still bounce to login. Public pages are exercised explicitly below.
const base = {
  status: 401 as number | undefined,
  requestPath: '/api/game/19/details',
  pathname: '/games/19/challenges',
  redirectInFlight: false,
  hasSession: true,
}

test('setAuthSession / hasAuthSession round-trips and defaults to false', () => {
  setAuthSession(false)
  assert.equal(hasAuthSession(), false)
  setAuthSession(true)
  assert.equal(hasAuthSession(), true)
  setAuthSession(false) // leave clean for other tests
})

test('REGRESSION: anonymous visitor on a public scoreboard does NOT redirect', () => {
  // The reported bug: GET /games/19/scoreboard#jeopardy bounced logged-out
  // users to login because the page fires an optional [RequireUser] /details
  // fetch that 401s. With no session, that 401 must be ignored.
  assert.equal(shouldRedirectOnUnauthorized({ ...base, pathname: '/games/19/scoreboard', hasSession: false }), false)
})

test('REGRESSION: EXPIRED session on a PUBLIC page (landing/scoreboard/list) does NOT redirect', () => {
  // The follow-up bug: an expired/stale session (hasSession=true) on the public
  // game landing or scoreboard still bounced to login. Public pages must render
  // the logged-out view, never redirect — even with a believed session.
  for (const pathname of ['/games', '/games/19', '/games/19/scoreboard', '/games/19/scoreboard/', '/', '/about', '/posts/3']) {
    assert.equal(shouldRedirectOnUnauthorized({ ...base, pathname, hasSession: true }), false, pathname)
  }
})

test('genuine session expiry on a PROTECTED page (challenges) DOES redirect', () => {
  assert.equal(shouldRedirectOnUnauthorized({ ...base, hasSession: true }), true)
})

test('falls back to the live flag when hasSession is omitted', () => {
  setAuthSession(false)
  assert.equal(
    shouldRedirectOnUnauthorized({ status: 401, requestPath: base.requestPath, pathname: base.pathname, redirectInFlight: false }),
    false,
  )
  setAuthSession(true)
  assert.equal(
    shouldRedirectOnUnauthorized({ status: 401, requestPath: base.requestPath, pathname: base.pathname, redirectInFlight: false }),
    true,
  )
  setAuthSession(false)
})

test('non-401 statuses never redirect', () => {
  for (const status of [200, 403, 404, 500, undefined]) {
    assert.equal(shouldRedirectOnUnauthorized({ ...base, status }), false)
  }
})

test('auth-probe endpoints (/account, /info) never trigger a redirect', () => {
  assert.equal(shouldRedirectOnUnauthorized({ ...base, requestPath: '/api/account/profile' }), false)
  assert.equal(shouldRedirectOnUnauthorized({ ...base, requestPath: '/api/info' }), false)
})

test('already on an account page → no redirect loop', () => {
  assert.equal(shouldRedirectOnUnauthorized({ ...base, pathname: '/account/login' }), false)
})

test('redirect already in flight → suppressed (fires at most once)', () => {
  assert.equal(shouldRedirectOnUnauthorized({ ...base, redirectInFlight: true }), false)
})
