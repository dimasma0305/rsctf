import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, type FC } from 'react'
import { installTestDom } from '../test/installDom'

test('challenge polling starts only while open, suspends in the background, and aborts on close', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/1/challenges' })
  const restoreDom = installTestDom(browser)
  context.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 0 })
  let visibilityState: DocumentVisibilityState = 'visible'
  let online = true
  Object.defineProperty(browser.document, 'visibilityState', {
    configurable: true,
    get: () => visibilityState,
  })
  Object.defineProperty(browser.navigator, 'onLine', {
    configurable: true,
    get: () => online,
  })

  const { useChallengePolling } = await import('./useChallengePolling')
  const { SWRConfig } = await import('swr')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const calls = new Map<string, number>()
  let aborted = 0

  const Probe: FC<{ active: boolean; requestKey: string; pending?: boolean }> = ({
    active,
    requestKey,
    pending = false,
  }) => {
    useChallengePolling({
      key: requestKey,
      active,
      refreshInterval: 1_000,
      request: (signal) => {
        calls.set(requestKey, (calls.get(requestKey) ?? 0) + 1)
        if (!pending) return Promise.resolve({ ok: true })
        return new Promise((_resolve, reject) => {
          signal.addEventListener(
            'abort',
            () => {
              aborted += 1
              reject({ name: 'AbortError' })
            },
            { once: true }
          )
        })
      },
    })
    return null
  }
  const cache = new Map()
  const Scope: FC<{ active: boolean; requestKey: string; pending?: boolean }> = (props) =>
    createElement(
      SWRConfig,
      {
        value: {
          provider: () => cache,
          dedupingInterval: 0,
          isVisible: () => visibilityState !== 'hidden',
          isOnline: () => online,
        },
      },
      createElement(Probe, props)
    )
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(Scope, { active: false, requestKey: '/detail/1' })))
    assert.equal(calls.get('/detail/1') ?? 0, 0)

    await act(async () => root.render(createElement(Scope, { active: true, requestKey: '/detail/1' })))
    assert.equal(calls.get('/detail/1'), 1)
    await act(async () => context.mock.timers.tick(1_000))
    assert.equal(calls.get('/detail/1'), 2)

    visibilityState = 'hidden'
    await act(async () => context.mock.timers.tick(5_000))
    assert.equal(calls.get('/detail/1'), 2)
    visibilityState = 'visible'
    online = false
    await act(async () => {
      browser.document.dispatchEvent(new browser.Event('visibilitychange'))
      context.mock.timers.tick(5_000)
    })
    assert.equal(calls.get('/detail/1'), 2)

    online = true
    await act(async () => {
      browser.dispatchEvent(new browser.Event('online'))
      context.mock.timers.tick(1_000)
    })
    assert.equal(calls.get('/detail/1'), 3)

    await act(async () =>
      root.render(createElement(Scope, { active: true, requestKey: '/detail/slow', pending: true }))
    )
    assert.equal(calls.get('/detail/slow'), 1)
    await act(async () =>
      root.render(createElement(Scope, { active: false, requestKey: '/detail/slow', pending: true }))
    )
    assert.equal(aborted, 1)
    await act(async () => context.mock.timers.tick(60_000))
    assert.equal(calls.get('/detail/slow'), 1)
  } finally {
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('permanent challenge failures stop while 429 honors Retry-After with one retry owner', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/1/challenges' })
  const restoreDom = installTestDom(browser)
  context.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 0 })
  const { useChallengePolling } = await import('./useChallengePolling')
  const { SWRConfig } = await import('swr')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const calls = new Map<number, number>()
  const cache = new Map()

  const Probe: FC<{ status: number }> = ({ status }) => {
    const { error } = useChallengePolling({
      key: `/failure/${status}`,
      active: true,
      refreshInterval: 120_000,
      request: async () => {
        const count = (calls.get(status) ?? 0) + 1
        calls.set(status, count)
        if (status === 429 && count > 1) return { ok: true }
        throw {
          response: {
            status,
            headers: status === 429 ? { 'retry-after': '12' } : {},
          },
        }
      },
    })
    const responseStatus = (error as { response?: { status?: number } } | undefined)?.response?.status
    return createElement('output', null, responseStatus === undefined ? 'ok' : String(responseStatus))
  }
  const Scope: FC<{ status: number }> = ({ status }) =>
    createElement(
      SWRConfig,
      {
        value: {
          provider: () => cache,
          dedupingInterval: 0,
          isVisible: () => true,
          isOnline: () => true,
        },
      },
      createElement(Probe, { status })
    )
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    for (const status of [401, 403, 404]) {
      await act(async () => root.render(createElement(Scope, { status })))
      assert.equal(calls.get(status), 1, `HTTP ${status} must issue its initial request`)
      assert.equal(container.textContent, String(status))
      await act(async () => context.mock.timers.tick(60_000))
      assert.equal(calls.get(status), 1)
      assert.equal(container.textContent, String(status))
    }

    await act(async () => root.render(createElement(Scope, { status: 429 })))
    assert.equal(calls.get(429), 1)
    assert.equal(container.textContent, '429')
    await act(async () => context.mock.timers.tick(11_999))
    assert.equal(calls.get(429), 1)
    await act(async () => context.mock.timers.tick(1))
    assert.equal(calls.get(429), 2)
    assert.equal(container.textContent, 'ok')

    await act(async () => root.render(createElement(Scope, { status: 503 })))
    assert.equal(calls.get(503), 1)
    for (let attempt = 0; attempt < 3; attempt += 1) {
      await act(async () => context.mock.timers.tick(30_000))
    }
    assert.equal(calls.get(503), 3)
    assert.equal(container.textContent, '503')
    await act(async () => context.mock.timers.tick(120_000))
    assert.equal(calls.get(503), 3)
  } finally {
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
