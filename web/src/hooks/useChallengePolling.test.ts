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
    assert.equal(aborted, 0, 'opening a challenge must not abort its initial request')
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

test('a due recovery waits for a hidden or offline modal to become active again', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/1/challenges' })
  const restoreDom = installTestDom(browser)
  context.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 0 })
  context.mock.method(Math, 'random', () => 0.5)
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
  const { createChallengeRecoveryOwner } = await import('../utils/ChallengePolling')
  const { useChallengePolling } = await import('./useChallengePolling')
  const { SWRConfig } = await import('swr')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const recoveryOwner = createChallengeRecoveryOwner()
  let calls = 0

  const Probe: FC = () => {
    useChallengePolling({
      key: '/detail/deferred',
      active: true,
      refreshInterval: 0,
      revalidateOnFocus: false,
      revalidateOnReconnect: false,
      recoveryOwner,
      recoveryKey: 'detail',
      request: async () => {
        calls += 1
        if (calls === 1) throw { response: { status: 503 } }
        return { ok: true }
      },
    })
    return null
  }
  const Scope: FC = () =>
    createElement(
      SWRConfig,
      {
        value: {
          provider: () => new Map(),
          dedupingInterval: 0,
          isVisible: () => visibilityState !== 'hidden',
          isOnline: () => online,
        },
      },
      createElement(Probe)
    )
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(Scope)))
    assert.equal(calls, 1)
    visibilityState = 'hidden'
    online = false
    await act(async () => context.mock.timers.tick(2_000))
    assert.equal(calls, 1, 'the due recovery must not run while the page is inactive')
    assert.equal(recoveryOwner.pendingEntryCount(), 1)

    visibilityState = 'visible'
    await act(async () => {
      browser.document.dispatchEvent(new browser.Event('visibilitychange'))
      await Promise.resolve()
    })
    assert.equal(calls, 1, 'visibility alone must not bypass the offline fence')
    assert.equal(recoveryOwner.pendingEntryCount(), 1)

    online = true
    await act(async () => {
      browser.dispatchEvent(new browser.Event('online'))
      for (let index = 0; index < 4; index += 1) await Promise.resolve()
    })
    assert.equal(recoveryOwner.pendingEntryCount(), 0)
    assert.equal(calls, 2, 'the retained recovery resumes once both fences allow it')
  } finally {
    recoveryOwner.cancelAll()
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('detail and solver recovery share one timer without merging their request results', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/1/challenges' })
  const restoreDom = installTestDom(browser)
  context.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 0 })
  context.mock.method(Math, 'random', () => 0.5)
  const { createChallengeRecoveryOwner } = await import('../utils/ChallengePolling')
  const { useChallengePolling } = await import('./useChallengePolling')
  const { SWRConfig } = await import('swr')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const recoveryOwner = createChallengeRecoveryOwner()
  const calls = new Map<string, number>()

  const Read: FC<{ resource: 'detail' | 'solvers' }> = ({ resource }) => {
    const { data } = useChallengePolling({
      key: `/challenge/${resource}`,
      active: true,
      refreshInterval: resource === 'detail' ? 0 : 30_000,
      recoveryOwner,
      recoveryKey: resource,
      request: async () => {
        const count = (calls.get(resource) ?? 0) + 1
        calls.set(resource, count)
        if (count === 1) throw { response: { status: 503 } }
        return { resource }
      },
    })
    return createElement('output', { 'data-resource': resource }, data?.resource ?? 'error')
  }
  const Scope: FC = () =>
    createElement(
      SWRConfig,
      {
        value: {
          provider: () => new Map(),
          dedupingInterval: 0,
          isVisible: () => true,
          isOnline: () => true,
        },
      },
      createElement(Read, { resource: 'detail' }),
      createElement(Read, { resource: 'solvers' })
    )
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(Scope)))
    assert.deepEqual(Object.fromEntries(calls), { detail: 1, solvers: 1 })
    assert.equal(recoveryOwner.pendingEntryCount(), 2)
    assert.equal(recoveryOwner.pendingTimerCount(), 1)
    await act(async () => context.mock.timers.tick(30_000))
    assert.deepEqual(Object.fromEntries(calls), { detail: 2, solvers: 2 })
    assert.equal(container.querySelector('[data-resource="detail"]')?.textContent, 'detail')
    assert.equal(container.querySelector('[data-resource="solvers"]')?.textContent, 'solvers')
  } finally {
    recoveryOwner.cancelAll()
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('an explicit recovery clears the terminal latch for a later bounded transient retry', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/1/challenges' })
  const restoreDom = installTestDom(browser)
  context.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 0 })
  context.mock.method(Math, 'random', () => 0.5)
  const { useChallengePolling } = await import('./useChallengePolling')
  const { SWRConfig } = await import('swr')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const cache = new Map()
  let calls = 0
  let retry: (() => Promise<unknown>) | null = null
  let retainedData: { ok: boolean } | undefined

  const Probe: FC = () => {
    const { data, error, mutate } = useChallengePolling({
      key: '/detail/recoverable',
      active: true,
      refreshInterval: 120_000,
      request: async () => {
        calls += 1
        if (calls === 1) throw { response: { status: 404 } }
        if (calls === 3) throw { response: { status: 503 } }
        return { ok: true }
      },
    })
    retainedData = data
    retry = () => mutate()
    const responseStatus = (error as { response?: { status?: number } } | undefined)?.response?.status
    return createElement('output', null, responseStatus === undefined ? 'ok' : String(responseStatus))
  }
  const Scope: FC = () =>
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
      createElement(Probe)
    )
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(Scope)))
    assert.equal(calls, 1)
    assert.equal(container.textContent, '404')
    await act(async () => context.mock.timers.tick(60_000))
    assert.equal(calls, 1, 'the permanent failure must remain paused until an explicit retry')

    await act(async () => {
      await retry?.()
    })
    assert.equal(calls, 2)
    assert.equal(container.textContent, 'ok')
    assert.deepEqual(retainedData, { ok: true })

    await act(async () => {
      await retry?.().catch(() => undefined)
    })
    assert.equal(calls, 3)
    assert.equal(container.textContent, '503')
    assert.deepEqual(retainedData, { ok: true }, 'a failed refresh must retain the last usable detail')
    await act(async () => context.mock.timers.tick(1_999))
    assert.equal(calls, 3)
    await act(async () => context.mock.timers.tick(1))
    assert.equal(calls, 4, 'the recovered key must regain its bounded automatic retry')
    assert.equal(container.textContent, 'ok')
  } finally {
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
