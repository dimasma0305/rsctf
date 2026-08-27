import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { act, createElement, type FC } from 'react'
import { installTestDom } from '../test/installDom'
import {
  CompletionPollSWRConfig,
  MAX_POLL_ERROR_RETRIES,
  MAX_SCOREBOARD_SETTLEMENT_POLLS,
  MAX_SCOREBOARD_WARMUP_POLLS,
  eventScoreboardPollDelay,
  pollErrorIsTransient,
  pollErrorRetryDelay,
  useCompletionPolling,
} from './useCompletionPolling'

test('poll retry policy stops permanent responses and honors bounded Retry-After', () => {
  for (const status of [400, 401, 403, 404, 409, 422]) {
    const error = { response: { status } }
    assert.equal(pollErrorIsTransient(error), false)
    assert.equal(
      pollErrorRetryDelay(error, 1, () => 0.5),
      null
    )
  }

  assert.equal(pollErrorIsTransient({ response: { status: 429 } }), true)
  assert.equal(pollErrorIsTransient({ response: { status: 503 } }), true)
  assert.equal(pollErrorIsTransient(new TypeError('offline')), true)
  assert.equal(
    pollErrorRetryDelay({ response: { status: 429, headers: { 'retry-after': '12' } } }, 1, () => 0.5, 0),
    12_000
  )
  assert.equal(
    pollErrorRetryDelay({ response: { status: 429, headers: { 'retry-after': '3600' } } }, 1, () => 0.5, 0),
    null
  )
  assert.equal(pollErrorRetryDelay({ response: { status: 503 } }, MAX_POLL_ERROR_RETRIES + 1), null)
})

test('scoreboard warmup and final settlement polling are explicit and bounded', () => {
  assert.ok(eventScoreboardPollDelay('coming', false, MAX_SCOREBOARD_WARMUP_POLLS - 1, 10_000, () => 0.5))
  assert.equal(eventScoreboardPollDelay('coming', false, MAX_SCOREBOARD_WARMUP_POLLS, 10_000), null)
  assert.ok(eventScoreboardPollDelay('ongoing', false, 10_000, 10_000, () => 0.5))
  assert.equal(eventScoreboardPollDelay('ended', true, 1, 10_000), null)
  assert.ok(eventScoreboardPollDelay('ended', false, MAX_SCOREBOARD_SETTLEMENT_POLLS - 1, 10_000, () => 0.5))
  assert.equal(eventScoreboardPollDelay('ended', false, MAX_SCOREBOARD_SETTLEMENT_POLLS, 10_000), null)
})

test('fake-time warmup stops, lifecycle transitions refresh once, and settlement stops when final', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/7/scoreboard' })
  const restoreDom = installTestDom(browser)
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  const { default: useSWR, SWRConfig } = await import('swr')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const cache = new Map()
  let reads = 0
  let settleAfter = Number.POSITIVE_INFINITY

  const Probe: FC<{ lifecycle: 'coming' | 'ongoing' | 'ended' }> = ({ lifecycle }) => {
    const query = useSWR(
      '/api/game/7/ad/koth/scoreboard',
      async () => {
        reads += 1
        return { fullySettled: reads >= settleAfter }
      },
      CompletionPollSWRConfig
    )
    useCompletionPolling({
      key: '/api/game/7/ad/koth/scoreboard',
      phase: lifecycle,
      enabled: true,
      data: query.data,
      error: query.error,
      isValidating: query.isValidating,
      mutate: query.mutate,
      successDelay: (latest, completedSuccesses) =>
        eventScoreboardPollDelay(lifecycle, latest.fullySettled, completedSuccesses, 1_000, () => 0.5),
      random: () => 0.5,
    })
    return null
  }
  const Scope: FC<{ lifecycle: 'coming' | 'ongoing' | 'ended' }> = ({ lifecycle }) =>
    createElement(
      SWRConfig,
      { value: { provider: () => cache, dedupingInterval: 0 } },
      createElement(Probe, { lifecycle })
    )
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(Scope, { lifecycle: 'coming' })))
    assert.equal(reads, 1)
    for (let count = 1; count < MAX_SCOREBOARD_WARMUP_POLLS; count += 1) {
      await act(async () => context.mock.timers.tick(1_000))
    }
    assert.equal(reads, MAX_SCOREBOARD_WARMUP_POLLS)
    await act(async () => context.mock.timers.tick(60_000))
    assert.equal(reads, MAX_SCOREBOARD_WARMUP_POLLS)

    await act(async () => root.render(createElement(Scope, { lifecycle: 'ongoing' })))
    await act(async () => context.mock.timers.tick(0))
    assert.equal(reads, MAX_SCOREBOARD_WARMUP_POLLS + 1)
    await act(async () => context.mock.timers.tick(1_000))
    assert.equal(reads, MAX_SCOREBOARD_WARMUP_POLLS + 2)

    settleAfter = reads + 2
    await act(async () => root.render(createElement(Scope, { lifecycle: 'ended' })))
    await act(async () => context.mock.timers.tick(0))
    assert.equal(reads, MAX_SCOREBOARD_WARMUP_POLLS + 3)
    await act(async () => context.mock.timers.tick(1_000))
    assert.equal(reads, settleAfter)
    await act(async () => context.mock.timers.tick(60 * 60_000))
    assert.equal(reads, settleAfter)
  } finally {
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('cached pre-start 400 refreshes once when the same scoreboard becomes ongoing', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/7/scoreboard' })
  const restoreDom = installTestDom(browser)
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  let visibility: DocumentVisibilityState = 'visible'
  Object.defineProperty(browser.document, 'visibilityState', { configurable: true, get: () => visibility })
  const { default: useSWR, SWRConfig } = await import('swr')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const cache = new Map()
  let reads = 0

  const Probe: FC<{ lifecycle: 'coming' | 'ongoing' }> = ({ lifecycle }) => {
    const query = useSWR(
      '/api/game/7/scoreboard',
      async () => {
        reads += 1
        if (reads === 1) throw { response: { status: 400 } }
        return { version: reads }
      },
      CompletionPollSWRConfig
    )
    useCompletionPolling({
      key: '/api/game/7/scoreboard',
      phase: lifecycle,
      enabled: true,
      data: query.data,
      error: query.error,
      isValidating: query.isValidating,
      mutate: query.mutate,
      successDelay: () => 1_000,
      random: () => 0.5,
    })
    return null
  }
  const Scope: FC<{ lifecycle: 'coming' | 'ongoing' }> = ({ lifecycle }) =>
    createElement(
      SWRConfig,
      { value: { provider: () => cache, dedupingInterval: 0 } },
      createElement(Probe, { lifecycle })
    )
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(Scope, { lifecycle: 'coming' })))
    assert.equal(reads, 1)
    await act(async () => context.mock.timers.tick(60_000))
    assert.equal(reads, 1, 'a terminal pre-start response must not own an ordinary retry timer')

    visibility = 'hidden'
    await act(async () => browser.document.dispatchEvent(new browser.Event('visibilitychange')))
    await act(async () => root.render(createElement(Scope, { lifecycle: 'ongoing' })))
    await act(async () => context.mock.timers.tick(60_000))
    assert.equal(reads, 1, 'the phase refresh must remain deferred while the page is hidden')

    visibility = 'visible'
    await act(async () => browser.document.dispatchEvent(new browser.Event('visibilitychange')))
    await act(async () => context.mock.timers.tick(0))
    assert.equal(reads, 2, 'the ongoing phase must supersede and refresh the cached pre-start error once')
    await act(async () => context.mock.timers.tick(999))
    assert.equal(reads, 2)
    await act(async () => context.mock.timers.tick(1))
    assert.equal(reads, 3, 'a successful phase refresh must resume the bounded ongoing cadence')
  } finally {
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('one completion owner honors Retry-After, recovers, and stops after modal close', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/7/challenges' })
  const restoreDom = installTestDom(browser)
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  const { default: useSWR, SWRConfig } = await import('swr')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const cache = new Map()
  let reads = 0

  const Probe: FC<{ opened: boolean }> = ({ opened }) => {
    const key = opened ? '/api/game/7/ad/koth/9/state' : null
    const query = useSWR(
      key,
      async () => {
        reads += 1
        if (reads === 1) throw { response: { status: 429, headers: { 'retry-after': '12' } } }
        return { round: reads }
      },
      CompletionPollSWRConfig
    )
    useCompletionPolling({
      key: key ?? '',
      phase: 'open',
      enabled: opened,
      data: query.data,
      error: query.error,
      isValidating: query.isValidating,
      mutate: query.mutate,
      successDelay: () => 5_000,
      random: () => 0.5,
    })
    return null
  }
  const Scope: FC<{ opened: boolean }> = ({ opened }) =>
    createElement(
      SWRConfig,
      { value: { provider: () => cache, dedupingInterval: 0 } },
      createElement(Probe, { opened })
    )
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(Scope, { opened: true })))
    assert.equal(reads, 1)
    await act(async () => context.mock.timers.tick(11_999))
    assert.equal(reads, 1)
    await act(async () => context.mock.timers.tick(1))
    assert.equal(reads, 2)
    await act(async () => context.mock.timers.tick(5_000))
    assert.equal(reads, 3, 'a successful retry must return to the normal cadence')

    await act(async () => root.render(createElement(Scope, { opened: false })))
    await act(async () => context.mock.timers.tick(60_000))
    assert.equal(reads, 3)
  } finally {
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('a slow response must complete before its successor timer exists', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/7/scoreboard' })
  const restoreDom = installTestDom(browser)
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  const { default: useSWR, SWRConfig } = await import('swr')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const cache = new Map()
  const completions: Array<() => void> = []
  let reads = 0

  const Probe: FC = () => {
    const query = useSWR(
      '/api/game/7/scoreboard',
      () => {
        reads += 1
        return new Promise<{ version: number }>((resolve) => {
          const version = reads
          completions.push(() => resolve({ version }))
        })
      },
      CompletionPollSWRConfig
    )
    useCompletionPolling({
      key: '/api/game/7/scoreboard',
      phase: 'ongoing',
      enabled: true,
      data: query.data,
      error: query.error,
      isValidating: query.isValidating,
      mutate: query.mutate,
      successDelay: () => 1_000,
      random: () => 0.5,
    })
    return null
  }
  const Scope: FC = () =>
    createElement(SWRConfig, { value: { provider: () => cache, dedupingInterval: 0 } }, createElement(Probe))
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(Scope)))
    assert.equal(reads, 1)
    await act(async () => context.mock.timers.tick(60_000))
    assert.equal(reads, 1, 'an interval must not overlap an unfinished request')

    await act(async () => completions.shift()?.())
    await act(async () => context.mock.timers.tick(999))
    assert.equal(reads, 1)
    await act(async () => context.mock.timers.tick(1))
    assert.equal(reads, 2)
  } finally {
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('invalid or revoked reads stop while a transient outage has one bounded recovery chain', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/404/scoreboard' })
  const restoreDom = installTestDom(browser)
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  const { default: useSWR, SWRConfig } = await import('swr')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const cache = new Map()
  const reads = new Map<number, number>()

  const Probe: FC<{ status: number }> = ({ status }) => {
    const key = `/poll/failure/${status}`
    const query = useSWR(
      key,
      async () => {
        reads.set(status, (reads.get(status) ?? 0) + 1)
        throw { response: { status } }
      },
      CompletionPollSWRConfig
    )
    useCompletionPolling({
      key,
      phase: 'ongoing',
      enabled: true,
      data: query.data,
      error: query.error,
      isValidating: query.isValidating,
      mutate: query.mutate,
      successDelay: () => 1_000,
      random: () => 0.5,
    })
    return null
  }
  const Scope: FC<{ status: number }> = ({ status }) =>
    createElement(
      SWRConfig,
      { value: { provider: () => cache, dedupingInterval: 0 } },
      createElement(Probe, { status })
    )
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    for (const status of [401, 403, 404]) {
      await act(async () => root.render(createElement(Scope, { status })))
      assert.equal(reads.get(status), 1)
      await act(async () => context.mock.timers.tick(60_000))
      assert.equal(reads.get(status), 1)
    }

    await act(async () => root.render(createElement(Scope, { status: 503 })))
    assert.equal(reads.get(503), 1)
    for (const delay of [1_000, 2_000, 4_000, 8_000, 16_000]) {
      await act(async () => context.mock.timers.tick(delay))
    }
    assert.equal(reads.get(503), 1 + MAX_POLL_ERROR_RETRIES)
    await act(async () => context.mock.timers.tick(60 * 60_000))
    assert.equal(reads.get(503), 1 + MAX_POLL_ERROR_RETRIES)
  } finally {
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('completion polling suspends hidden and offline pages, then resumes once', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/7/scoreboard' })
  const restoreDom = installTestDom(browser)
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  let visibility: DocumentVisibilityState = 'visible'
  let online = true
  Object.defineProperty(browser.document, 'visibilityState', { configurable: true, get: () => visibility })
  Object.defineProperty(browser.navigator, 'onLine', { configurable: true, get: () => online })
  const { default: useSWR, SWRConfig } = await import('swr')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const cache = new Map()
  let reads = 0

  const Probe: FC = () => {
    const query = useSWR(
      '/api/game/7/scoreboard',
      async () => {
        reads += 1
        return { updateTimeUtc: reads }
      },
      CompletionPollSWRConfig
    )
    useCompletionPolling({
      key: '/api/game/7/scoreboard',
      phase: 'ongoing',
      enabled: true,
      data: query.data,
      error: query.error,
      isValidating: query.isValidating,
      mutate: query.mutate,
      successDelay: () => 1_000,
      random: () => 0.5,
    })
    return null
  }
  const Scope: FC = () =>
    createElement(SWRConfig, { value: { provider: () => cache, dedupingInterval: 0 } }, createElement(Probe))
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(Scope)))
    assert.equal(reads, 1)

    visibility = 'hidden'
    await act(async () => browser.document.dispatchEvent(new browser.Event('visibilitychange')))
    await act(async () => context.mock.timers.tick(5_000))
    assert.equal(reads, 1)

    visibility = 'visible'
    await act(async () => browser.document.dispatchEvent(new browser.Event('visibilitychange')))
    await act(async () => context.mock.timers.tick(1_000))
    assert.equal(reads, 2)

    online = false
    await act(async () => browser.dispatchEvent(new browser.Event('offline')))
    await act(async () => context.mock.timers.tick(5_000))
    assert.equal(reads, 2)

    online = true
    await act(async () => browser.dispatchEvent(new browser.Event('online')))
    await act(async () => context.mock.timers.tick(1_000))
    assert.equal(reads, 3)
  } finally {
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('scoreboard route and KotH modal consume one owned snapshot per key', () => {
  const route = readFileSync('src/pages/games/[id]/Scoreboard.tsx', 'utf8')
  const standardTable = readFileSync('src/components/ScoreboardTable.tsx', 'utf8')
  const mobileTable = readFileSync('src/components/mobile/ScoreboardTable.tsx', 'utf8')
  const timeline = readFileSync('src/components/charts/ScoreTimeLine.tsx', 'utf8')
  const adTable = readFileSync('src/components/AdScoreboardTable.tsx', 'utf8')
  const kothTable = readFileSync('src/components/KothScoreboardTable.tsx', 'utf8')
  const combinedTable = readFileSync('src/components/CombinedScoreboardTable.tsx', 'utf8')
  const kothPanel = readFileSync('src/components/KothChallengePanel.tsx', 'utf8')

  assert.match(route, /useGameScoreboardRead\(numId\)/)
  assert.match(route, /scoreboard === undefined \|\|/)
  assert.match(route, /<ScoreTimeLine divisionId=\{divisionId\} scoreboard=\{scoreboard\}/)
  for (const child of [standardTable, mobileTable, timeline]) assert.doesNotMatch(child, /useGameScoreboard\(/)
  assert.doesNotMatch(adTable, /useAdScoreboard\(/)
  assert.doesNotMatch(kothTable, /useKothScoreboard\(/)
  assert.doesNotMatch(combinedTable, /useCombinedScoreboard\(/)
  assert.doesNotMatch(kothPanel, /useGameAdTargets|gameAdTargets|\/Ad\/Targets/)
  assert.match(kothPanel, /stateData\?\.ip/)
  assert.match(kothPanel, /assertJsonResponse/)
  assert.match(kothPanel, /useCompletionPolling\(\{/)
  assert.match(kothPanel, /active: boolean/)
  assert.match(kothPanel, /AbortController/)
})
