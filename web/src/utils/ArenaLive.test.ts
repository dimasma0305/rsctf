import assert from 'node:assert/strict'
import test from 'node:test'
import { readFileSync } from 'node:fs'
import { arenaLiveRoutes, arenaPollDelay, arenaReconnectDelay, mergeArenaRoster, parseArenaRetryAfter } from './ArenaLive'

test('arena routes use only registered lowercase contracts', () => {
  assert.deepEqual(arenaLiveRoutes(17), {
    adScoreboard: '/api/game/17/ad/scoreboard',
    kothScoreboard: '/api/game/17/ad/koth/scoreboard',
    scoreboard: '/api/game/17/scoreboard',
    game: '/api/game/17',
  })
})

test('poll and reconnect recovery are bounded, jittered, and honor Retry-After', () => {
  assert.equal(parseArenaRetryAfter('4'), 4_000)
  assert.equal(parseArenaRetryAfter('999'), 60_000)
  assert.equal(arenaPollDelay(3, 7_000, () => 0), 7_000)
  assert.equal(arenaPollDelay(0, null, () => 0), 12_000)
  assert.equal(arenaPollDelay(20, null, () => 1), 60_000)
  assert.equal(arenaReconnectDelay(1, () => 0), 500)
  assert.equal(arenaReconnectDelay(20, () => 1), 60_000)
})

test('arena roster is the stable union of A&D, KotH, and Jeopardy boards', () => {
  const roster = mergeArenaRoster(
    [{ participationId: 2, teamName: 'shared', settledTotal: 10 }],
    [
      { participationId: 2, teamName: 'shared', settledTotal: 20 },
      { participationId: 3, teamName: 'koth-only', settledTotal: 30 },
    ],
    [
      { id: 4, name: 'shared', score: 40 },
      { id: 5, name: 'jeop-only', score: 50 },
    ]
  )
  assert.deepEqual(
    roster.map(({ key, teamName, ad, koth, jeopardy }) => [key, teamName, !!ad, !!koth, !!jeopardy]),
    [
      ['j5', 'jeop-only', false, false, true],
      ['p2', 'shared', true, true, true],
      ['p3', 'koth-only', false, true, false],
    ]
  )
})

test('arena owns one completion-scheduled recovery chain and every deferred cinematic callback', () => {
  const source = readFileSync('src/pages/games/[id]/Attack.tsx', 'utf8')
  assert.doesNotMatch(source, /setInterval\(pollLive|AttackFeed|\/api\/Game/)
  assert.doesNotMatch(source, /(?<![.A-Za-z])setTimeout\(/)
  assert.match(source, /livePollController.*AbortController/s)
  assert.match(source, /visibilitychange.*online.*offline/s)
  assert.match(source, /deferred\.forEach\(\(id\) => clearTimeout\(id\)\)/)
})
