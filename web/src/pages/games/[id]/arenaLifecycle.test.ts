import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { arenaRoutes, initialArenaMatchTiming, observeArenaGameTiming, resolveArenaFinalState } from './arenaLifecycle'

test('live arena URLs match the registered case-sensitive route contract', () => {
  assert.deepEqual(arenaRoutes(17), {
    game: '/api/game/17',
    standardScoreboard: '/api/game/17/scoreboard',
    combinedScoreboard: '/api/game/17/scoreboard/combined',
    adScoreboard: '/api/Game/17/Ad/Scoreboard',
    kothScoreboard: '/api/game/17/ad/koth/scoreboard',
  })

  const gameRoutes = readFileSync('../src/controllers/game/routes.rs', 'utf8')
  const adRoutes = readFileSync('../src/controllers/game/ad/mod.rs', 'utf8')
  const kothRoutes = readFileSync('../src/controllers/game/koth/mod.rs', 'utf8')
  const arena = readFileSync('src/pages/games/[id]/Attack.tsx', 'utf8')
  assert.ok(gameRoutes.includes('"/api/game/{id}"'))
  assert.ok(gameRoutes.includes('"/api/game/{id}/scoreboard"'))
  assert.ok(gameRoutes.includes('"/api/game/{id}/scoreboard/combined"'))
  assert.ok(adRoutes.includes('"/api/Game/{id}/Ad/Scoreboard"'))
  assert.ok(kothRoutes.includes('"/api/game/{id}/ad/koth/scoreboard"'))
  assert.doesNotMatch(arena, /AttackFeed/)
  assert.match(arena, /new CompletionScheduledArenaCycle\(runLiveCycle/)
  assert.doesNotMatch(arena, /setInterval\(pollLive/)
  assert.match(arena, /arenaRetryDelay\(Math\.max\(1, wsRetry\)/)
  assert.doesNotMatch(arena, /setTimeout\(connectWS/)
  assert.match(arena, /window\.addEventListener\('offline', syncLiveTransport\)/)
  assert.match(arena, /if \(ws\) ws\.close\(\)/)
  assert.match(arena, /socket\.readyState !== WebSocket\.CONNECTING/)
  assert.match(arena, /clearTimeout\(wsConnectTimer\)/)
  assert.match(arena, /nextTopology !== liveTopologySignature/)
  assert.match(arena, /deferredTimers\.cancelAll\(\)/)
  assert.doesNotMatch(arena, /^\s*setTimeout\(/m)
})

const eventFormats = [
  {
    name: 'pure A&D',
    modes: { jeopardy: { active: false }, attackDefense: { active: true }, koth: { active: false } },
  },
  {
    name: 'pure KotH',
    modes: { jeopardy: { active: false }, attackDefense: { active: false }, koth: { active: true } },
  },
  {
    name: 'pure Jeopardy',
    modes: { jeopardy: { active: true }, attackDefense: { active: false }, koth: { active: false } },
  },
  {
    name: 'hybrid',
    modes: { jeopardy: { active: true }, attackDefense: { active: true }, koth: { active: true } },
  },
] as const

for (const format of eventFormats) {
  test(`${format.name} arena reaches its podium on the authoritative server clock`, (context) => {
    const localNow = 2_100_000_000_000
    const serverNow = localNow - 2 * 60 * 60_000
    context.mock.timers.enable({ apis: ['Date'], now: localNow })
    try {
      const timing = observeArenaGameTiming(
        initialArenaMatchTiming(),
        { end: serverNow + 2_000, serverTime: serverNow },
        localNow
      )
      const finalBoard = { fullySettled: true, modes: format.modes, items: [{ rank: 1, format: format.name }] }

      assert.equal(resolveArenaFinalState(timing, finalBoard), 'playing')
      context.mock.timers.tick(2_000)
      assert.equal(resolveArenaFinalState(timing, finalBoard), 'podium')
    } finally {
      context.mock.timers.reset()
    }
  })
}

test('epoch arenas wait for durable settlement after the event closes', () => {
  const timing = observeArenaGameTiming(initialArenaMatchTiming(), { end: 1_000, serverTime: 1_100 }, 50_000)
  const board = {
    modes: { jeopardy: { active: false }, attackDefense: { active: true }, koth: { active: false } },
    items: [{ rank: 1 }],
  }
  assert.equal(resolveArenaFinalState(timing, { ...board, fullySettled: false }, 50_000), 'settling')
  assert.equal(resolveArenaFinalState(timing, { ...board, fullySettled: true }, 50_000), 'podium')
})

test('an organizer extension moves the live deadline without a reload', (context) => {
  const localNow = 2_200_000_000_000
  const serverNow = localNow - 60 * 60_000
  context.mock.timers.enable({ apis: ['Date'], now: localNow })
  try {
    let timing = observeArenaGameTiming(
      initialArenaMatchTiming(),
      { end: serverNow + 2_000, serverTime: serverNow },
      localNow
    )
    const finalBoard = {
      fullySettled: true,
      modes: { jeopardy: { active: true }, attackDefense: { active: true }, koth: { active: false } },
      items: [{ rank: 1 }],
    }

    context.mock.timers.tick(1_000)
    timing = observeArenaGameTiming(timing, { end: serverNow + 8_000, serverTime: serverNow + 1_000 }, Date.now())
    context.mock.timers.tick(1_000)
    assert.equal(resolveArenaFinalState(timing, finalBoard), 'playing', 'the obsolete deadline must not win the race')

    context.mock.timers.tick(6_000)
    assert.equal(resolveArenaFinalState(timing, finalBoard), 'podium')
  } finally {
    context.mock.timers.reset()
  }
})
