import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import test from 'node:test'
import { connectionAllowsRoutePrefetch, createRouteModulePrefetcher, sameOriginRoutePath } from './RoutePrefetchPolicy'

test('the app wires bounded route prefetch for pointer, keyboard, touch, and settled-page navigation', () => {
  const app = readFileSync(resolve('src/App.tsx'), 'utf8')
  const component = readFileSync(resolve('src/components/RoutePrefetcher.tsx'), 'utf8')

  assert.match(app, /<RoutePrefetcher \/>/)
  assert.match(component, /import\.meta\.glob\('\.\.\/pages\/\*\*\/\*\.tsx'\)/)
  assert.match(component, /IDLE_PREFETCH_LIMIT = 2/)
  assert.match(component, /'pointerover'/)
  assert.match(component, /'focusin'/)
  assert.match(component, /'touchstart'/)
  assert.match(component, /setTimeout\(\(\) => void prefetchVisibleLinks\(\), IDLE_PREFETCH_DELAY_MS\)/)
})

test('route modules are matched case-insensitively across static, index, and parameter routes', async () => {
  const loaded: string[] = []
  const load = (name: string) => async () => {
    loaded.push(name)
  }
  const prefetch = createRouteModulePrefetcher({
    '../pages/Index.tsx': load('home'),
    '../pages/games/Index.tsx': load('games'),
    '../pages/games/[id]/Index.tsx': load('game'),
    '../pages/games/[id]/Challenges.tsx': load('challenges'),
    '../pages/admin/games/[id]/challenges/[chalId]/Flags.tsx': load('flags'),
    '../pages/[...all].tsx': load('not-found'),
  })

  assert.equal(await prefetch('/games'), true)
  assert.equal(await prefetch('/GAMES/20/'), true)
  assert.equal(await prefetch('/games/20/challenges'), true)
  assert.equal(await prefetch('/admin/games/20/challenges/74/flags'), true)
  assert.equal(await prefetch('/missing'), false, 'the catch-all page is not speculatively loaded')
  assert.deepEqual(loaded, ['games', 'game', 'challenges', 'flags'])
})

test('a route module is requested once, while a failed speculation remains retryable', async () => {
  let attempts = 0
  const prefetch = createRouteModulePrefetcher({
    '../pages/Teams.tsx': async () => {
      attempts += 1
      if (attempts === 1) throw new Error('temporary network failure')
    },
  })

  const failedAttempt = prefetch('/teams')
  assert.equal(await prefetch('/teams'), false)
  assert.equal(await failedAttempt, false)
  assert.equal(await prefetch('/teams'), true)
  assert.equal(await prefetch('/teams'), false)
  assert.equal(attempts, 2)
})

test('prefetch respects data-saving connections and rejects cross-origin links', () => {
  assert.equal(connectionAllowsRoutePrefetch(), true)
  assert.equal(connectionAllowsRoutePrefetch({ effectiveType: '4g' }), true)
  assert.equal(connectionAllowsRoutePrefetch({ effectiveType: '2g' }), false)
  assert.equal(connectionAllowsRoutePrefetch({ effectiveType: 'slow-2g' }), false)
  assert.equal(connectionAllowsRoutePrefetch({ saveData: true }), false)

  assert.equal(sameOriginRoutePath('/games/20?tab=ad#score', 'https://ctf.example'), '/games/20')
  assert.equal(sameOriginRoutePath('https://ctf.example/admin', 'https://ctf.example'), '/admin')
  assert.equal(sameOriginRoutePath('https://other.example/games', 'https://ctf.example'), null)
  assert.equal(sameOriginRoutePath('https://user@ctf.example/games', 'https://ctf.example'), null)
  assert.equal(sameOriginRoutePath('not a valid URL', 'not an origin'), null)
})
