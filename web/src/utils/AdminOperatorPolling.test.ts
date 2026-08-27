import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test, { type TestContext } from 'node:test'
import { act, createElement, type FC, useState } from 'react'
import type { BareFetcher, SWRConfiguration } from 'swr'
import type { AdEngineMetadataModel, AdGameStateModel } from '../Api'
import {
  ADMIN_OPERATOR_POLL_MS,
  adminOperatorPolling,
  adminOperatorView,
  useAdminAdState,
  useAdminKothState,
  useAdminOperatorEngines,
} from '../hooks/useGame'
import { installTestDom } from '../test/installDom'

const START = 1_800_000_000_000
const END = START + 60_000

const adGrid: AdGameStateModel = {
  currentRound: 7,
  roundStartedAt: START,
  roundEndsAt: START + ADMIN_OPERATOR_POLL_MS,
  scoringPaused: false,
  scoringPausedAt: null,
  challenges: [],
  teams: [],
}

const kothState = {
  epochTicks: 12,
  cycleTicks: 3,
  championCooldownTicks: 1,
  claimConfirmationTicks: 2,
  tickSeconds: 5,
  scoringGeneratedAt: START,
  latestRound: 7,
  currentRoundEndsAt: START + ADMIN_OPERATOR_POLL_MS,
  scoringPaused: false,
  scoringPausedAt: null,
  hills: [],
  teams: [],
}

type Counts = Record<'engines' | 'grid' | 'live' | 'koth', number>

const flush = async () => {
  await Promise.resolve()
  await Promise.resolve()
  await Promise.resolve()
}

const runPollingScenario = async (
  context: TestContext,
  metadata: AdEngineMetadataModel,
  verify: (controls: {
    counts: Counts
    tick: (milliseconds: number) => Promise<void>
    setView: (view: 'ad' | 'koth') => Promise<void>
    setNow: (now: number) => Promise<void>
    setHidden: (hidden: boolean) => Promise<void>
    setOnline: (online: boolean) => Promise<void>
  }) => Promise<void>
) => {
  const browser = new Window({ url: 'https://rsctf.test/admin/games/41/ad' })
  const restoreDom = installTestDom(browser)
  context.mock.timers.enable({ apis: ['setTimeout'], now: START })
  let hidden = false
  let online = true
  Object.defineProperty(browser.document, 'visibilityState', {
    configurable: true,
    get: () => (hidden ? 'hidden' : 'visible'),
  })
  const counts: Counts = { engines: 0, grid: 0, live: 0, koth: 0 }
  const fetcher: BareFetcher<unknown> = async (request) => {
    switch (request) {
      case '/api/edit/games/41/ad/Engines':
        counts.engines += 1
        return metadata
      case '/api/edit/games/41/ad/State':
        counts.grid += 1
        return adGrid
      case '/api/edit/games/41/ad/Live':
        counts.live += 1
        return { ...adGrid, serverTime: START, services: [] }
      case '/api/edit/games/41/ad/koth/state':
        counts.koth += 1
        return kothState
      default:
        throw new Error(`unexpected operator request: ${String(request)}`)
    }
  }
  const swrConfig: SWRConfiguration = {
    provider: () => new Map(),
    fetcher,
    dedupingInterval: 0,
    isVisible: () => !hidden,
    isOnline: () => online,
  }
  const { SWRConfig } = await import('swr')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  let chooseView: ((view: 'ad' | 'koth') => void) | undefined
  let chooseNow: ((now: number) => void) | undefined

  const Probe: FC = () => {
    const [preferred, setPreferred] = useState<'ad' | 'koth'>('ad')
    const [now, setNow] = useState(START + 1_000)
    chooseView = setPreferred
    chooseNow = setNow
    const { engineMetadata } = useAdminOperatorEngines(41)
    const activeView = adminOperatorView(preferred, engineMetadata)
    const polling = adminOperatorPolling(engineMetadata, now)
    const adEnabled = engineMetadata?.hasAttackDefense === true && activeView === 'ad'
    const kothEnabled = engineMetadata?.hasKoth === true && activeView === 'koth'
    useAdminAdState(41, adEnabled, polling)
    useAdminKothState(41, kothEnabled, polling)
    return createElement('output', null, activeView)
  }

  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  try {
    await act(async () => {
      root.render(createElement(SWRConfig, { value: swrConfig }, createElement(Probe)))
      await flush()
    })
    await verify({
      counts,
      tick: async (milliseconds) => {
        let remaining = milliseconds
        while (remaining > 0) {
          const step = Math.min(remaining, ADMIN_OPERATOR_POLL_MS)
          await act(async () => {
            context.mock.timers.tick(step)
            await flush()
          })
          remaining -= step
        }
      },
      setView: async (view) => {
        await act(async () => {
          chooseView?.(view)
          await flush()
        })
      },
      setNow: async (now) => {
        await act(async () => {
          chooseNow?.(now)
          await flush()
        })
      },
      setHidden: async (nextHidden) => {
        hidden = nextHidden
        await act(async () => {
          browser.document.dispatchEvent(new browser.Event('visibilitychange'))
          await flush()
        })
      },
      setOnline: async (nextOnline) => {
        online = nextOnline
        await act(async () => {
          browser.dispatchEvent(new browser.Event(nextOnline ? 'online' : 'offline'))
          await flush()
        })
      },
    })
  } finally {
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
}

test('pure events request only their configured operator engine on fake five-second ticks', async (context) => {
  await runPollingScenario(
    context,
    { hasAttackDefense: true, hasKoth: false, start: START, end: END, serverTime: START },
    async ({ counts, tick }) => {
      assert.deepEqual(counts, { engines: 1, grid: 1, live: 1, koth: 0 })
      await tick(ADMIN_OPERATOR_POLL_MS * 2)
      assert.deepEqual(counts, { engines: 1, grid: 1, live: 3, koth: 0 })
    }
  )

  await runPollingScenario(
    context,
    { hasAttackDefense: false, hasKoth: true, start: START, end: END, serverTime: START },
    async ({ counts, tick }) => {
      assert.deepEqual(counts, { engines: 1, grid: 0, live: 0, koth: 1 })
      await tick(ADMIN_OPERATOR_POLL_MS * 2)
      assert.deepEqual(counts, { engines: 1, grid: 0, live: 0, koth: 3 })
    }
  )
})

test('hybrid operator polling follows the selected view and stops hidden or ended reads', async (context) => {
  await runPollingScenario(
    context,
    { hasAttackDefense: true, hasKoth: true, start: START, end: END, serverTime: START },
    async ({ counts, tick, setView, setNow, setHidden, setOnline }) => {
      assert.deepEqual(counts, { engines: 1, grid: 1, live: 1, koth: 0 })
      await tick(ADMIN_OPERATOR_POLL_MS)
      assert.equal(counts.live, 2)

      await setView('koth')
      assert.deepEqual(counts, { engines: 1, grid: 1, live: 2, koth: 1 })
      await tick(ADMIN_OPERATOR_POLL_MS * 2)
      assert.deepEqual(counts, { engines: 1, grid: 1, live: 2, koth: 3 })

      await setHidden(true)
      await tick(ADMIN_OPERATOR_POLL_MS * 2)
      assert.equal(counts.koth, 3)
      await setHidden(false)

      const beforeOffline = counts.koth
      await setOnline(false)
      await tick(ADMIN_OPERATOR_POLL_MS * 2)
      assert.equal(counts.koth, beforeOffline)
      await setOnline(true)
      const afterReconnect = counts.koth
      assert.ok(afterReconnect === beforeOffline || afterReconnect === beforeOffline + 1)

      await setNow(END)
      const finalReads = counts.koth
      assert.ok(
        finalReads === afterReconnect || finalReads === afterReconnect + 1,
        'transition performs at most one final revalidation'
      )
      await tick(ADMIN_OPERATOR_POLL_MS * 3)
      assert.equal(counts.koth, finalReads)
    }
  )
})
