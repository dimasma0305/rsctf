import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { act, createElement, type FC, useCallback, useEffect, useRef, useState } from 'react'
import { AnswerResult, EventType, type GameEvent, type Submission } from '../Api'
import { installTestDom } from '../test/installDom'
import { gameEventMonitorIdentity, submissionMonitorIdentity, unreconciledMonitorRows } from './MonitorFeed'

const monitorPages = [
  {
    path: 'src/pages/games/[id]/monitor/Events.tsx',
    fetchName: 'fetchEvents',
    identityName: 'gameEventMonitorIdentity',
    callbackDependencies: /\[activePage, hideContainerEvents, debouncedSearch, numId, t\]/,
  },
  {
    path: 'src/pages/games/[id]/monitor/Submissions.tsx',
    fetchName: 'fetchSubmissions',
    identityName: 'submissionMonitorIdentity',
    callbackDependencies: /\[activePage, type, debouncedSearch, numId, t\]/,
  },
]

test('monitor hubs survive timing revalidation, stop at the boundary, and reconcile afterward', () => {
  for (const { path, fetchName, identityName, callbackDependencies } of monitorPages) {
    const source = readFileSync(path, 'utf8')

    assert.match(source, /const \{ finished \} = useGameStatus\(game\)/, path)
    assert.match(source, /const monitorConnectionActive = Boolean\(game\?\.end\) && !finished/, path)
    assert.match(source, /if \(monitorConnectionActive\) \{[\s\S]*?new signalR\.HubConnectionBuilder\(\)/, path)
    assert.match(source, /\}, \[monitorConnectionActive, numId, t\]\)/, path)
    assert.doesNotMatch(source, /\}, \[game, numId, t\]\)/, path)
    assert.match(source, new RegExp(`const ${fetchName} = useCallback`), path)
    assert.match(source, callbackDependencies, `${path} must retire stale backfills whenever its query scope changes`)
    assert.match(source, /monitorHubStop\.current = connection\.stop\(\)/, path)
    assert.match(
      source,
      new RegExp(`useRevalidateWhenPollingStops\\(monitorConnectionActive, ${fetchName}, waitForMonitorHubStop\\)`),
      path
    )
    assert.match(source, new RegExp(`unreconciledMonitorRows\\([^;]+${identityName}\\)`), path)
    assert.ok(
      source.lastIndexOf('useRevalidateWhenPollingStops(') > source.indexOf('new signalR.HubConnectionBuilder()'),
      `${path} must reconcile after the hub ownership effect so cleanup runs first`
    )
  }
})

class ScopedMonitorFeed {
  readonly requests: string[] = []
  private readonly pending = new Map<string, Array<(rows: string[]) => void>>()

  read(scope: string) {
    this.requests.push(scope)
    return new Promise<string[]>((resolve) => {
      const resolvers = this.pending.get(scope) ?? []
      resolvers.push(resolve)
      this.pending.set(scope, resolvers)
    })
  }

  complete(scope: string, rows: string[]) {
    const resolvers = this.pending.get(scope)
    const resolve = resolvers?.shift()
    if (resolvers?.length === 0) this.pending.delete(scope)
    resolve?.(rows)
  }

  completeAll(scope: string, rows: string[]) {
    while (this.pending.has(scope)) this.complete(scope, rows)
  }
}

test('query changes cancel pending final backfills for both monitor feeds', async (context) => {
  const cases = [
    {
      name: 'events filter, page, and search',
      initialScope: JSON.stringify({ game: 1, page: 1, hideContainer: false, search: '' }),
      nextScope: JSON.stringify({ game: 1, page: 2, hideContainer: true, search: 'operator' }),
    },
    {
      name: 'submissions type, page, and search',
      initialScope: JSON.stringify({ game: 1, page: 1, type: 'All', search: '' }),
      nextScope: JSON.stringify({ game: 1, page: 2, type: AnswerResult.WrongAnswer, search: 'team' }),
    },
  ]

  for (const { name, initialScope, nextScope } of cases) {
    await context.test(name, async () => {
      const browser = new Window({ url: 'https://rsctf.test/games/1/monitor' })
      const restoreDom = installTestDom(browser)
      const { useRevalidateWhenPollingStops } = await import('../hooks/useGame')
      const { createRoot } = await import('react-dom/client')
      const container = browser.document.createElement('div')
      browser.document.body.append(container)
      const root = createRoot(container)
      const feed = new ScopedMonitorFeed()
      let finishStop: (() => void) | undefined
      const pendingStop = new Promise<void>((resolve) => {
        finishStop = resolve
      })
      ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

      const Probe: FC<{ active: boolean; scope: string }> = ({ active, scope }) => {
        const [rows, setRows] = useState<string[]>([])
        const hubStop = useRef<Promise<void>>(Promise.resolve())
        const waitForHubStop = useCallback(() => hubStop.current, [])
        const refresh = useCallback(async () => setRows(await feed.read(scope)), [feed, scope])

        useEffect(() => {
          void refresh()
        }, [refresh])

        useEffect(() => {
          if (!active) return
          return () => {
            hubStop.current = pendingStop
          }
        }, [active])

        useRevalidateWhenPollingStops(active, refresh, waitForHubStop)
        return createElement('output', null, rows.join(','))
      }

      try {
        await act(async () => {
          root.render(createElement(Probe, { active: true, scope: initialScope }))
          await Promise.resolve()
        })
        assert.deepEqual(feed.requests, [initialScope])
        await act(async () => {
          feed.complete(initialScope, ['initial rows'])
          await Promise.resolve()
        })

        await act(async () => {
          root.render(createElement(Probe, { active: false, scope: initialScope }))
          await Promise.resolve()
        })
        assert.deepEqual(feed.requests, [initialScope], 'the closeout read must remain behind pending stop()')

        await act(async () => {
          root.render(createElement(Probe, { active: false, scope: nextScope }))
          await Promise.resolve()
        })
        assert.deepEqual(feed.requests, [initialScope, nextScope])

        await act(async () => {
          feed.complete(nextScope, ['new scope rows'])
          await Promise.resolve()
        })
        assert.equal(container.textContent, 'new scope rows', 'the newer query completes before hub shutdown')

        await act(async () => {
          finishStop?.()
          await Promise.resolve()
          await Promise.resolve()
        })
        feed.completeAll(initialScope, ['stale final rows'])
        await act(async () => {
          await Promise.resolve()
        })

        assert.deepEqual(
          feed.requests,
          [initialScope, nextScope],
          'the obsolete post-stop callback must not issue its old-scope HTTP request'
        )
        assert.equal(container.textContent, 'new scope rows', 'an obsolete final snapshot must not replace newer rows')
      } finally {
        finishStop?.()
        feed.completeAll(initialScope, [])
        feed.completeAll(nextScope, [])
        await act(async () => {
          root.unmount()
          await Promise.resolve()
        })
        delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
        await browser.happyDOM.close()
        restoreDom()
      }
    })
  }
})

class BoundaryFeed<Row> {
  readonly authoritative: Row[]
  requests = 0
  stops = 0
  private listener: ((message: Row) => void) | undefined
  private resolveStop: (() => void) | undefined

  constructor(
    initial: Row,
    private readonly restProjection: (row: Row) => Row
  ) {
    this.authoritative = [initial]
  }

  connect(listener: (message: Row) => void) {
    this.listener = listener
    return () => {
      this.stops += 1
      return new Promise<void>((resolve) => {
        this.resolveStop = () => {
          this.listener = undefined
          this.resolveStop = undefined
          resolve()
        }
      })
    }
  }

  acceptAndBroadcast(message: Row) {
    this.authoritative.unshift(message)
    this.listener?.(message)
  }

  acceptWhileBroadcastIsInFlight(message: Row) {
    this.authoritative.unshift(message)
  }

  read() {
    this.requests += 1
    return Promise.resolve(this.authoritative.map(this.restProjection))
  }

  completeStop() {
    if (this.resolveStop) this.resolveStop()
    else this.listener = undefined
  }
}

interface BoundaryCase<Row> {
  name: string
  initial: Row
  received: Row
  inFlight: Row
  postStop: Row
  identity: (row: Row) => string
  label: (row: Row) => string
  restProjection: (row: Row) => Row
}

const hubTime = (milliseconds: number) => new Date(milliseconds).toISOString() as unknown as number

test('monitor reconciliation consumes snapshot identities as an occurrence-aware stable multiset', () => {
  const repeated: GameEvent = {
    time: 1_000,
    type: EventType.Normal,
    values: ['same-payload'],
    user: 'operator',
    team: 'staff',
  }
  const pushed = [repeated, { ...repeated }, { ...repeated }]
  const snapshot = [{ ...repeated }, { ...repeated }]

  assert.equal(unreconciledMonitorRows(pushed, snapshot, gameEventMonitorIdentity).length, 1)
})

test('each browser feed serializes stop and backfill across received, in-flight, and post-stop commits', async (context) => {
  const runCase = async <Row>({
    name,
    initial,
    received,
    inFlight,
    postStop,
    identity,
    label,
    restProjection,
  }: BoundaryCase<Row>) => {
    await context.test(name, async () => {
      const browser = new Window({ url: `https://rsctf.test/games/1/monitor/${name}` })
      const restoreDom = installTestDom(browser)
      const { useRevalidateWhenPollingStops } = await import('../hooks/useGame')
      const { createRoot } = await import('react-dom/client')
      const container = browser.document.createElement('div')
      browser.document.body.append(container)
      const root = createRoot(container)
      const feed = new BoundaryFeed(initial, restProjection)
      ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

      const Probe: FC<{ active: boolean }> = ({ active }) => {
        const pushedRows = useRef<Row[]>([])
        const [snapshotRows, setSnapshotRows] = useState<Row[]>([])
        const [, publishPush] = useState(0)
        const hubStop = useRef<Promise<void>>(Promise.resolve())
        const waitForHubStop = useCallback(() => hubStop.current, [])
        const refresh = useCallback(async () => setSnapshotRows(await feed.read()), [feed])

        useEffect(() => {
          void refresh()
        }, [refresh])

        useEffect(() => {
          if (!active) return
          const stop = feed.connect((message) => {
            pushedRows.current = [message, ...pushedRows.current]
            publishPush((version) => version + 1)
          })
          return () => {
            hubStop.current = stop()
          }
        }, [active, feed])

        useRevalidateWhenPollingStops(active, refresh, waitForHubStop)

        const visibleRows = [...unreconciledMonitorRows(pushedRows.current, snapshotRows, identity), ...snapshotRows]
        return createElement('output', null, visibleRows.map(label).join(','))
      }

      try {
        await act(async () => {
          root.render(createElement(Probe, { active: true }))
          await Promise.resolve()
        })
        assert.equal(feed.requests, 1)
        assert.equal(container.textContent, label(initial))

        await act(async () => feed.acceptAndBroadcast(received))
        assert.equal(container.textContent, `${label(received)},${label(initial)}`)

        await act(async () => {
          root.render(createElement(Probe, { active: false }))
          await Promise.resolve()
        })
        assert.equal(feed.stops, 1)
        assert.equal(feed.requests, 1, 'the final read must wait for asynchronous listener removal')

        // This pre-close operation commits while stop() is still in flight,
        // but its queued boundary broadcast never reaches the listener.
        feed.acceptWhileBroadcastIsInFlight(inFlight)
        feed.completeStop()
        // The listener is gone. This committed operation's attempted broadcast
        // is therefore lost, but it lands before the serialized HTTP snapshot.
        feed.acceptAndBroadcast(postStop)
        await act(async () => {
          await Promise.resolve()
        })
        assert.equal(feed.requests, 2, 'one initial read plus exactly one serialized final backfill')

        assert.equal(
          container.textContent,
          [postStop, inFlight, received, initial].map(label).join(','),
          'already-received rows stay unique and both lost boundary broadcasts are recovered newest-first'
        )

        await act(async () => {
          root.render(createElement(Probe, { active: false }))
          await Promise.resolve()
        })
        assert.equal(feed.requests, 2, 'remaining stopped must not add polling')
      } finally {
        await act(async () => {
          root.unmount()
          await Promise.resolve()
        })
        feed.completeStop()
        delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
        await browser.happyDOM.close()
        restoreDom()
      }
    })
  }

  await runCase<GameEvent>({
    name: 'events',
    initial: { time: hubTime(1_000), type: EventType.Normal, values: ['initial'], user: 'one', team: 'alpha' },
    received: { time: hubTime(2_000), type: EventType.FlagSubmit, values: ['received'], user: 'two', team: 'beta' },
    inFlight: {
      time: hubTime(3_000),
      type: EventType.CheatDetected,
      values: ['in-flight'],
      user: 'three',
      team: 'gamma',
    },
    postStop: {
      time: hubTime(4_000),
      type: EventType.Download,
      values: ['post-stop'],
      user: 'four',
      team: 'delta',
    },
    identity: gameEventMonitorIdentity,
    label: (row) => row.values[0],
    restProjection: (row) => ({ ...row, time: Date.parse(String(row.time)) }),
  })
  await runCase<Submission>({
    name: 'submissions',
    initial: {
      time: hubTime(1_000),
      status: AnswerResult.WrongAnswer,
      answer: 'initial',
      user: 'one',
      team: 'alpha',
      challenge: 'A',
    },
    received: {
      time: hubTime(2_000),
      status: AnswerResult.Accepted,
      answer: 'received',
      user: 'two',
      team: 'beta',
      challenge: 'B',
    },
    inFlight: {
      time: hubTime(3_000),
      status: AnswerResult.CheatDetected,
      answer: 'in-flight',
      user: 'three',
      team: 'gamma',
      challenge: 'C',
    },
    postStop: {
      time: hubTime(4_000),
      status: AnswerResult.NotFound,
      answer: 'post-stop',
      user: 'four',
      team: 'delta',
      challenge: 'D',
    },
    identity: submissionMonitorIdentity,
    label: (row) => row.answer ?? '',
    restProjection: (row) => ({ ...row, time: Date.parse(String(row.time)) }),
  })
})
