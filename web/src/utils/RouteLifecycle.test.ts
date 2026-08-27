import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, type FC, type PropsWithChildren, useState } from 'react'
import type { Key, SWRConfiguration } from 'swr'
import { challengeIdFromHash, ownedChallengeIdFromHash } from '../components/ChallengePanel'
import { shouldReadChallenge } from '../components/GameChallengeModal'
import { installTestDom } from '../test/installDom'
import { RouteLifecycleBoundary, viewerIdentityMiddleware, ViewerIdentityScope } from './ViewerIdentity'

type Deferred<T> = {
  promise: Promise<T>
  resolve: (value: T) => void
  reject: (error: unknown) => void
}

const deferred = <T>(): Deferred<T> => {
  let resolve!: (value: T) => void
  let reject!: (error: unknown) => void
  const promise = new Promise<T>((accept, fail) => {
    resolve = accept
    reject = fail
  })
  return { promise, resolve, reject }
}

test('challenge hashes require ownership in the current response before any read is eligible', () => {
  assert.equal(challengeIdFromHash('#7-ret2win'), 7)
  assert.equal(challengeIdFromHash('#7'), 7)
  for (const invalid of ['', '#', '#0-zero', '#-7', '#7wrong', '#9007199254740993-too-large']) {
    assert.equal(challengeIdFromHash(invalid), null, invalid)
  }

  assert.equal(ownedChallengeIdFromHash('#7-a', [7, 8]), 7, 'game A owns challenge 7')
  assert.equal(ownedChallengeIdFromHash('#7-b', []), null, 'a slow game B response owns nothing yet')
  assert.equal(ownedChallengeIdFromHash('#7-b', [8]), null, 'a noncolliding game B rejects the old id')
  assert.equal(ownedChallengeIdFromHash('#7-b', [7, 9]), 7, 'a colliding id opens only after game B proves ownership')

  const reads: string[] = []
  const schedule = (gameId: number, challengeId: number, opened: boolean, owned: boolean) => {
    if (!shouldReadChallenge(opened, owned, gameId, challengeId)) return
    reads.push(`/api/game/${gameId}/challenges/${challengeId}`)
    reads.push(`/api/game/${gameId}/challenges/${challengeId}/solvers`)
  }

  schedule(1, 7, true, true)
  schedule(2, 7, true, false)
  schedule(2, 99, true, false)
  schedule(2, 7, false, true)
  assert.deepEqual(reads, ['/api/game/1/challenges/7', '/api/game/1/challenges/7/solvers'])
  assert.equal(
    reads.some((path) => path.includes('/api/game/2/')),
    false,
    'no invalid B/A request is scheduled'
  )
})

test('SPA game, query, and account navigation remounts desktop and mobile route-local state', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/1/challenges#7-a' })
  const restoreDom = installTestDom(browser)
  const { MemoryRouter, useNavigate } = await import('react-router')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const dirty = new Map<string, () => void>()
  let navigate: ReturnType<typeof useNavigate> | undefined
  let mountSequence = 0

  const NavigationCapture: FC = () => {
    navigate = useNavigate()
    return null
  }
  const RouteStateProbe: FC<{ viewport: 'desktop' | 'mobile' }> = ({ viewport }) => {
    const [mount] = useState(() => ++mountSequence)
    const [challenge, setChallenge] = useState('closed')
    const [writeup, setWriteup] = useState('closed')
    const [team, setTeam] = useState('closed')
    const [search, setSearch] = useState('empty')
    const [modal, setModal] = useState('closed')
    const [live, setLive] = useState('empty')
    dirty.set(viewport, () => {
      setChallenge('open')
      setWriteup('open')
      setTeam('selected')
      setSearch('needle')
      setModal('open')
      setLive('buffered')
    })
    return createElement(
      'output',
      { id: `${viewport}-route-state` },
      [mount, challenge, writeup, team, search, modal, live].join(':')
    )
  }
  const RouteState: FC = () =>
    createElement(
      RouteLifecycleBoundary,
      null,
      createElement(RouteStateProbe, { viewport: 'desktop' }),
      createElement(RouteStateProbe, { viewport: 'mobile' })
    )
  const App: FC<PropsWithChildren<{ scope: string }>> = ({ scope }) =>
    createElement(
      MemoryRouter,
      { initialEntries: ['/games/1/challenges#7-a'] },
      createElement(ViewerIdentityScope, { scope }, createElement(NavigationCapture), createElement(RouteState))
    )
  const values = () => [
    container.querySelector('#desktop-route-state')?.textContent ?? '',
    container.querySelector('#mobile-route-state')?.textContent ?? '',
  ]
  const assertClean = () => {
    for (const value of values()) assert.match(value, /^\d+:closed:closed:closed:empty:closed:empty$/)
  }
  const assertDirty = () => {
    for (const value of values()) assert.match(value, /^\d+:open:open:selected:needle:open:buffered$/)
  }
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(App, { scope: 'user:admin:Admin' })))
    assertClean()
    await act(async () => {
      dirty.get('desktop')?.()
      dirty.get('mobile')?.()
    })
    assertDirty()

    // A hash selects within the same loaded game and must not destroy the
    // current route component before ownership is evaluated.
    await act(async () => navigate?.('/games/1/challenges#8-another'))
    assertDirty()

    await act(async () => navigate?.('/games/2/challenges'))
    assertClean()
    await act(async () => {
      dirty.get('desktop')?.()
      dirty.get('mobile')?.()
      navigate?.('/games/2/challenges?division=blue')
    })
    assertClean()

    await act(async () => {
      dirty.get('desktop')?.()
      dirty.get('mobile')?.()
    })
    assertDirty()
    await act(async () => root.render(createElement(App, { scope: 'user:player:User' })))
    assertClean()
  } finally {
    await act(async () => root.unmount())
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('SWR never publishes previous game, query, or account data while a replacement read is slow or rejected', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/1/challenges' })
  const restoreDom = installTestDom(browser)
  const { default: useSWR, SWRConfig } = await import('swr')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const requests: { key: Key; pending: Deferred<{ label: string }> }[] = []
  const fetcher = (key: Key) => {
    const pending = deferred<{ label: string }>()
    requests.push({ key, pending })
    return pending.promise
  }
  const cache = new Map()
  const swrConfig: SWRConfiguration = {
    provider: () => cache,
    fetcher,
    dedupingInterval: 0,
    keepPreviousData: true,
    shouldRetryOnError: false,
    use: [viewerIdentityMiddleware],
  }
  const Probe: FC<{ requestKey: Key }> = ({ requestKey }) => {
    const { data, error } = useSWR<{ label: string }, Error>(requestKey)
    return createElement('output', null, error ? 'error' : (data?.label ?? 'loading'))
  }
  const App: FC<{ scope: string; requestKey: Key }> = ({ scope, requestKey }) =>
    createElement(
      ViewerIdentityScope,
      { scope },
      createElement(SWRConfig, { value: swrConfig }, createElement(Probe, { requestKey }))
    )
  const settle = async (index: number, value: string) => {
    await act(async () => {
      requests[index].pending.resolve({ label: value })
      await requests[index].pending.promise
    })
  }
  const reject = async (index: number) => {
    await act(async () => {
      requests[index].pending.reject(new Error('forbidden'))
      await requests[index].pending.promise.catch(() => undefined)
    })
  }
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(App, { scope: 'user:a:User', requestKey: '/api/game/1/details' })))
    assert.equal(container.textContent, 'loading')
    assert.equal(requests.length, 1)
    assert.equal(requests[0].key, '/api/game/1/details', 'the cache-only viewer marker never reaches HTTP')
    await settle(0, 'game A')
    assert.equal(container.textContent, 'game A')

    await act(async () => root.render(createElement(App, { scope: 'user:a:User', requestKey: '/api/game/2/details' })))
    assert.equal(container.textContent, 'loading', 'game A is not painted under game B')
    assert.equal(requests[1].key, '/api/game/2/details')
    await reject(1)
    assert.equal(container.textContent, 'error', 'a forbidden/missing B never falls back to game A')

    await act(async () => root.render(createElement(App, { scope: 'user:b:User', requestKey: '/api/game/2/details' })))
    assert.equal(container.textContent, 'loading', 'account B never inherits account A data or errors')
    await settle(2, 'game B for account B')
    assert.equal(container.textContent, 'game B for account B')

    const alpha: Key = ['/api/game', { search: 'alpha' }]
    const beta: Key = ['/api/game', { search: 'beta' }]
    await act(async () => root.render(createElement(App, { scope: 'user:b:User', requestKey: alpha })))
    assert.equal(container.textContent, 'loading')
    assert.deepEqual(requests[3].key, alpha)
    await settle(3, 'alpha results')
    assert.equal(container.textContent, 'alpha results')

    await act(async () => root.render(createElement(App, { scope: 'user:b:User', requestKey: beta })))
    assert.equal(container.textContent, 'loading', 'alpha results are not painted for beta')
    assert.deepEqual(requests[4].key, beta)
    await reject(4)
    assert.equal(container.textContent, 'error')

    await act(async () =>
      root.render(createElement(App, { scope: 'user:non-member:User', requestKey: '/api/game/1/details' }))
    )
    assert.equal(container.textContent, 'loading', 'a non-member account never inherits the accepted member response')
    assert.equal(requests[5].key, '/api/game/1/details')
    await reject(5)
    assert.equal(container.textContent, 'error', 'the non-member rejection cannot reveal the accepted member response')
  } finally {
    await act(async () => root.unmount())
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('the shared game timing hook preserves viewer scoping and live-read readiness', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/17' })
  const restoreDom = installTestDom(browser)
  const { SWRConfig } = await import('swr')
  const { useGameAccess } = await import('../hooks/useGame')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const pending: Deferred<Record<string, unknown>>[] = []
  const requests: Key[] = []
  const fetcher = (key: Key) => {
    requests.push(key)
    const read = deferred<Record<string, unknown>>()
    pending.push(read)
    return read.promise
  }
  const config: SWRConfiguration = {
    provider: () => new Map(),
    fetcher,
    dedupingInterval: 0,
    shouldRetryOnError: false,
    use: [viewerIdentityMiddleware],
  }
  const Probe: FC = () => {
    const { game, liveReadReady } = useGameAccess(17)
    return createElement('output', null, `${game?.title ?? 'loading'}:${liveReadReady ? 'ready' : 'waiting'}`)
  }
  const App: FC<{ scope: string }> = ({ scope }) =>
    createElement(ViewerIdentityScope, { scope }, createElement(SWRConfig, { value: config }, createElement(Probe)))
  const game = (title: string) => ({
    id: 17,
    title,
    start: Date.now() - 1_000,
    end: Date.now() + 60_000,
    status: 'Accepted',
    practiceMode: false,
    serverTime: Date.now(),
  })
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(App, { scope: 'user:a:User' })))
    assert.deepEqual(requests, ['/api/game/17'])
    assert.equal(container.textContent, 'loading:waiting')
    await act(async () => {
      pending[0].resolve(game('account A'))
      await pending[0].promise
    })
    assert.equal(container.textContent, 'account A:ready')

    await act(async () => root.render(createElement(App, { scope: 'user:b:User' })))
    assert.equal(container.textContent, 'loading:waiting')
    assert.deepEqual(requests, ['/api/game/17', '/api/game/17'])
    await act(async () => {
      pending[1].resolve(game('account B'))
      await pending[1].promise
    })
    assert.equal(container.textContent, 'account B:ready')
  } finally {
    await act(async () => root.unmount())
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
