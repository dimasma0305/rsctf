import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, type FC } from 'react'
import { installTestDom } from '../test/installDom'

const SAFETY_MS = 5 * 60_000
const MINIMUM_DELAY_MS = 1_000

test('an ahead browser clock cannot force capability renewal before a late server sample', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/1/challenges' })
  const restoreDom = installTestDom(browser)
  const serverStart = 2_000_000_000_000
  const localStart = serverStart + 2 * 60 * 60_000
  const capabilityExpiresAt = serverStart + 10 * 60_000
  context.mock.timers.enable({
    apis: ['Date', 'setInterval', 'setTimeout'],
    now: new Date(localStart),
  })
  const { observeServerTime, serverClockTestApi, useServerClockTimeout } = await import('./ServerClock')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  let renewals = 0
  const Probe: FC = () => {
    useServerClockTimeout(
      () => {
        renewals += 1
      },
      capabilityExpiresAt,
      SAFETY_MS,
      MINIMUM_DELAY_MS
    )
    return createElement('output', null, renewals)
  }
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    serverClockTestApi.reset()
    await act(async () => root.render(createElement(Probe)))

    await act(async () => context.mock.timers.tick(500))
    await act(async () => {
      assert.equal(observeServerTime(serverStart + 500, localStart + 500), true)
    })

    // The browser-only calculation would have fired at one second. The first
    // live sample must cancel it and restore the server-relative five-minute lead.
    await act(async () => context.mock.timers.tick(500))
    assert.equal(renewals, 0)
    await act(async () => context.mock.timers.tick(298_999))
    assert.equal(renewals, 0)
    await act(async () => context.mock.timers.tick(1))
    assert.equal(renewals, 1)
  } finally {
    await act(async () => root.unmount())
    serverClockTestApi.reset()
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('a later clock correction replaces a behind-browser capability timer', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/1/challenges' })
  const restoreDom = installTestDom(browser)
  const serverStart = 2_000_000_000_000
  const localStart = serverStart - 2 * 60 * 60_000
  const capabilityExpiresAt = serverStart + 20 * 60_000
  context.mock.timers.enable({
    apis: ['Date', 'setInterval', 'setTimeout'],
    now: new Date(localStart),
  })
  const { observeServerTime, serverClockTestApi, useServerClockTimeout } = await import('./ServerClock')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  let renewals = 0
  const Probe: FC = () => {
    useServerClockTimeout(
      () => {
        renewals += 1
      },
      capabilityExpiresAt,
      SAFETY_MS,
      MINIMUM_DELAY_MS
    )
    return null
  }
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    serverClockTestApi.reset()
    await act(async () => root.render(createElement(Probe)))
    await act(async () => {
      assert.equal(observeServerTime(serverStart, localStart), true)
    })

    // Eleven local minutes later, a newer response corrects the first estimate
    // ten minutes backward. Its serverTime is still monotonic, so it models the
    // same response ordering used by the production observer.
    await act(async () => context.mock.timers.tick(11 * 60_000))
    await act(async () => {
      assert.equal(observeServerTime(serverStart + 60_000, localStart + 11 * 60_000), true)
    })

    // The first authoritative timer would fire at local minute 15. The later
    // correction moves the safe renewal point to local minute 25 instead.
    await act(async () => context.mock.timers.tick(4 * 60_000))
    assert.equal(renewals, 0)
    await act(async () => context.mock.timers.tick(9 * 60_000 + 59_999))
    assert.equal(renewals, 0)
    await act(async () => context.mock.timers.tick(1))
    assert.equal(renewals, 1)
  } finally {
    await act(async () => root.unmount())
    serverClockTestApi.reset()
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
