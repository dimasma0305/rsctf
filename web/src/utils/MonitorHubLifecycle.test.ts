import { HubConnectionBuilder, type HubConnection } from '@microsoft/signalr'
import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { act, createElement, type FC, useCallback, useEffect, useRef, useState } from 'react'
import { AnswerResult, EventType, type GameEvent, type MonitorSubmission } from '../Api'
import { useRecoveringHub } from '../hooks/useRecoveringHub'
import { installTestDom } from '../test/installDom'
import {
  currentMonitorBufferRows,
  currentMonitorSnapshotRows,
  gameEventMonitorIdentity,
  mergeGameEventBuffer,
  mergeSubmissionBuffer,
  monitorCursorPushIsCurrent,
  monitorEventPushIsCurrent,
  monitorPushIsCurrent,
  monitorSnapshotIsCurrent,
  receiveMonitorSubmissions,
  rebaseGameEventBuffer,
  rebaseSubmissionBuffer,
  submissionMatchesMonitorFilter,
  submissionMonitorFilterScope,
  submissionMonitorIdentity,
  unreconciledMonitorRows,
} from './MonitorFeed'

const monitorPages = [
  {
    path: 'src/pages/games/[id]/monitor/Events.tsx',
    fetchName: 'fetchEvents',
    finalName: 'reconcileEvents',
    identityName: 'gameEventMonitorIdentity',
    callbackDependencies: /\[loadSnapshot, t\]/,
  },
  {
    path: 'src/pages/games/[id]/monitor/Submissions.tsx',
    fetchName: 'fetchSubmissions',
    finalName: 'reconcileSubmissions',
    identityName: 'submissionMonitorIdentity',
    callbackDependencies: /\[loadSnapshot, t\]/,
  },
]

test('monitor hubs survive timing revalidation, stop at the boundary, and reconcile afterward', () => {
  const recoveringHub = readFileSync('src/hooks/useRecoveringHub.ts', 'utf8')
  assert.match(recoveringHub, /stopPromise\.current = controller\.stop\(\)/)
  assert.match(recoveringHub, /return \{ state, waitForStop \}/)

  for (const { path, fetchName, finalName, identityName, callbackDependencies } of monitorPages) {
    const source = readFileSync(path, 'utf8')

    assert.match(source, /const \{ finished(?:, status: gameStatus)? \} = useGameStatus\(game\)/, path)
    assert.match(source, /const monitorConnectionActive = Boolean\(game\?\.end\) && !finished/, path)
    assert.match(
      source,
      /const \{ waitForStop: waitForMonitorHubStop \} = useRecoveringHub\(\{[\s\S]*?active: monitorConnectionActive/,
      path
    )
    assert.match(source, /url: `\/hub\/monitor\?game=\$\{numId\}`/, path)
    assert.doesNotMatch(source, /new signalR\.HubConnectionBuilder\(\)/, path)
    if (path.endsWith('Events.tsx')) {
      assert.match(source, /api\.game\.gameEventBackfill\(numId/, path)
      assert.match(source, /page < MAX_BACKFILL_PAGES/, path)
      assert.match(source, /mergeGameEventBuffer\(incoming, newEvents\.current, MAX_BUFFERED_EVENTS\)/, path)
      assert.match(source, /ownerKey: gameStatus/, path)
      assert.match(
        source,
        /monitorEventPushIsCurrent\([\s\S]*?activeFeedScope\.current,[\s\S]*?cursorInitialized\.current,[\s\S]*?eventCursor\.current,[\s\S]*?message\.cursor/,
        path
      )
      assert.match(source, /monitorSnapshotIsCurrent\(/, path)
      assert.match(source, /const \{ scope: viewerScope \} = useViewerIdentity\(\)/, path)
      assert.match(
        source,
        /const loadSnapshot = useCallback[\s\S]*?\[activePage, hideContainerEvents, debouncedSearch, numId, snapshotScope\]/,
        path
      )
      const fallback = source.slice(
        source.indexOf('// Cap recovery at ten pages'),
        source.indexOf('const { waitForStop: waitForMonitorHubStop }')
      )
      assert.match(
        fallback,
        /const checkpoint = await api\.game\.gameEventBackfill\(numId, \{\}, \{ signal \}\)[\s\S]*const snapshot = await loadSnapshot\(\)[\s\S]*if \(!isCurrent\(\) \|\| snapshot === undefined\) return[\s\S]*rebaseAtCheckpoint\(checkpoint\.data\.nextCursor\)/,
        `${path} must not skip a large reconnect gap when its replacement snapshot fails`
      )
    } else {
      assert.match(source, /api\.game\.gameSubmissionBackfill\(numId/, path)
      assert.match(source, /page < MAX_BACKFILL_PAGES/, path)
      assert.match(source, /receiveMonitorSubmissions\([\s\S]*?MAX_BUFFERED_SUBMISSIONS/, path)
      assert.match(source, /ownerKey: gameStatus/, path)
      assert.match(
        source,
        /monitorCursorPushIsCurrent\([\s\S]*?activeFeedScope\.current,[\s\S]*?cursorInitialized\.current,[\s\S]*?submissionCursor\.current,[\s\S]*?message\.cursor/,
        path
      )
      assert.match(source, /monitorSnapshotIsCurrent\(/, path)
      assert.match(source, /const \{ scope: viewerScope \} = useViewerIdentity\(\)/, path)
      assert.match(source, /const feedScope = JSON\.stringify\(\[viewerScope, numId\]\)/, path)
      assert.match(
        source,
        /const snapshotScope = JSON\.stringify\(\[feedScope, activePage, type, debouncedSearch\]\)/,
        path
      )
      assert.match(
        source,
        /const submissionFilterScope = submissionMonitorFilterScope\(feedScope, type, debouncedSearch\)/,
        path
      )
      assert.match(source, /const submissionRequest = useRef\(new LatestRequest\(\)\)/, path)
      assert.match(source, /const recoveryRequest = useRef\(new LatestRequest\(\)\)/, path)
      assert.match(source, /currentMonitorSnapshotRows\(snapshotScope, submissionSnapshot\)/, path)
      assert.match(
        source,
        /currentMonitorBufferRows\([\s\S]*?submissionFilterScope,[\s\S]*?bufferedSubmissionScope\.current,[\s\S]*?newSubmissions\.current[\s\S]*?\)/,
        path
      )
      assert.match(
        source,
        /const loadSnapshot = useCallback[\s\S]*?\[activePage, type, debouncedSearch, numId, snapshotScope\]/,
        path
      )
      assert.match(
        source,
        /activeSubmissionFilterScope\.current === requestedSubmissionFilterScope/,
        `${path} must fence a stale reconnect batch when the active filter changes`
      )
      assert.match(
        source,
        /mergeSubmissionBuffer\(bufferedSubmissions, submissions \?\? \[\], ITEM_COUNT_PER_PAGE\)/,
        path
      )
      assert.match(source, /key=\{item\.id\}/, path)
      const fallback = source.slice(
        source.indexOf('// Cap recovery at ten pages'),
        source.indexOf('const { waitForStop: waitForMonitorHubStop }')
      )
      assert.match(
        fallback,
        /const checkpoint = await api\.game\.gameSubmissionBackfill\(numId, \{\}, \{ signal \}\)[\s\S]*const snapshot = await loadSnapshot\(\)[\s\S]*if \(!isCurrent\(\) \|\| snapshot === undefined\) return[\s\S]*rebaseAtCheckpoint\(checkpoint\.data\.nextCursor\)/,
        `${path} must not skip a large reconnect gap when its replacement snapshot fails`
      )
    }
    assert.doesNotMatch(source, /\}, \[game, numId, t\]\)/, path)
    assert.match(source, new RegExp(`const ${fetchName} = useCallback`), path)
    assert.match(source, callbackDependencies, `${path} must retire stale backfills whenever its query scope changes`)
    assert.match(
      source,
      new RegExp(`useRevalidateWhenPollingStops\\(monitorConnectionActive, ${finalName}, waitForMonitorHubStop\\)`),
      path
    )
    assert.match(source, new RegExp(`unreconciledMonitorRows\\([^;]+${identityName}\\)`), path)
    assert.ok(
      source.lastIndexOf('useRevalidateWhenPollingStops(') > source.indexOf('useRecoveringHub({'),
      `${path} must reconcile after the hub ownership effect so cleanup runs first`
    )
  }
})

type HookHubHandler = (...arguments_: unknown[]) => void

class HookHub {
  startCalls = 0
  stopCalls = 0
  keepAliveIntervalInMilliseconds = 0
  serverTimeoutInMilliseconds = 0
  private readonly handlers = new Map<string, HookHubHandler[]>()
  private closeHandler: (error?: Error) => void = () => undefined
  private reconnectingHandler: (error?: Error) => void = () => undefined
  private reconnectedHandler: (connectionId?: string) => void = () => undefined

  start = async () => {
    this.startCalls += 1
  }

  stop = async () => {
    this.stopCalls += 1
  }

  on(name: string, handler: HookHubHandler) {
    const registered = this.handlers.get(name) ?? []
    registered.push(handler)
    this.handlers.set(name, registered)
  }

  onclose(handler: (error?: Error) => void) {
    this.closeHandler = handler
  }

  onreconnecting(handler: (error?: Error) => void) {
    this.reconnectingHandler = handler
  }

  onreconnected(handler: (connectionId?: string) => void) {
    this.reconnectedHandler = handler
  }

  emit(name: string, ...arguments_: unknown[]) {
    for (const handler of this.handlers.get(name) ?? []) handler(...arguments_)
  }
}

const settleHookLifecycle = async () => {
  for (let index = 0; index < 8; index += 1) await Promise.resolve()
}

const withFakeHookHubs = async (run: (hubs: HookHub[]) => Promise<void>) => {
  const hubs: HookHub[] = []
  const originalBuild = HubConnectionBuilder.prototype.build
  HubConnectionBuilder.prototype.build = function buildFakeHub() {
    const hub = new HookHub()
    hubs.push(hub)
    return hub as unknown as HubConnection
  }
  try {
    await run(hubs)
  } finally {
    HubConnectionBuilder.prototype.build = originalBuild
  }
}

test('monitor Events re-arms authoritative recovery when coming becomes ongoing', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/1/monitor/events' })
  const restoreDom = installTestDom(browser)
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const recoveries: string[] = []
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  const Probe: FC<{ phase: 'coming' | 'ongoing' }> = ({ phase }) => {
    useRecoveringHub({
      active: true,
      url: '/hub/monitor?game=1',
      ownerKey: phase,
      handlers: { ReceivedGameEvent: () => undefined },
      revalidate: () => {
        recoveries.push(phase)
        if (phase === 'coming') throw Object.assign(new Error('event has not started'), { statusCode: 400 })
      },
      pollingIntervalMs: 0,
    })
    return createElement('output', null, phase)
  }

  try {
    await withFakeHookHubs(async (hubs) => {
      await act(async () => {
        root.render(createElement(Probe, { phase: 'coming' }))
        await settleHookLifecycle()
      })
      assert.deepEqual(recoveries, ['coming'], 'the intentional prestart denial is observed once')
      assert.equal(hubs.length, 1)

      await act(async () => {
        root.render(createElement(Probe, { phase: 'ongoing' }))
        await settleHookLifecycle()
      })
      assert.deepEqual(recoveries, ['coming', 'ongoing'], 'the ongoing owner establishes a fresh durable cursor')
      assert.equal(hubs.length, 2, 'the lifecycle phase replaces the permanently suppressed recovery owner')
      assert.equal(hubs[0].stopCalls, 1, 'the coming transport is stopped before its replacement owns the feed')
    })
  } finally {
    await act(async () => {
      root.unmount()
      await settleHookLifecycle()
    })
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('monitor Events ignores initialized pushes covered by its durable cursor', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/1/monitor/events' })
  const restoreDom = installTestDom(browser)
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  const event = (cursor: number): GameEvent => ({
    id: cursor,
    cursor,
    time: cursor,
    type: EventType.Normal,
    values: [`event-${cursor}`],
  })

  const Probe: FC = () => {
    const activeScope = useRef('viewer:1/game:1')
    const cursorInitialized = useRef(false)
    const durableCursor = useRef(0)
    const [visible, setVisible] = useState<GameEvent[]>([])

    useRecoveringHub({
      active: true,
      url: '/hub/monitor?game=1',
      ownerKey: 'ongoing',
      handlers: {
        ReceivedGameEvent: (raw) => {
          const message = raw as GameEvent
          if (
            !monitorEventPushIsCurrent(
              activeScope.current,
              'viewer:1/game:1',
              false,
              cursorInitialized.current,
              durableCursor.current,
              message.cursor
            )
          )
            return
          setVisible((current) => mergeGameEventBuffer([message], current, 500))
        },
      },
      revalidate: () => {
        durableCursor.current = 10
        cursorInitialized.current = true
      },
      pollingIntervalMs: 0,
    })

    return createElement('output', null, visible.map(({ cursor }) => cursor).join(','))
  }

  try {
    await withFakeHookHubs(async (hubs) => {
      await act(async () => {
        root.render(createElement(Probe))
        await settleHookLifecycle()
      })
      assert.equal(hubs.length, 1)

      await act(async () => {
        hubs[0].emit('ReceivedGameEvent', event(9))
        hubs[0].emit('ReceivedGameEvent', event(10))
        hubs[0].emit('ReceivedGameEvent', event(11))
        await settleHookLifecycle()
      })
      assert.equal(container.textContent, '11', 'only a commit newer than the initialized durable cursor is live')
    })
  } finally {
    await act(async () => {
      root.unmount()
      await settleHookLifecycle()
    })
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('monitor Submissions ignores initialized pushes covered by its durable cursor', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/1/monitor/submissions' })
  const restoreDom = installTestDom(browser)
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  const submission = (cursor: number): MonitorSubmission => ({
    id: cursor,
    cursor,
    time: cursor,
    status: AnswerResult.Accepted,
    answer: `submission-${cursor}`,
  })

  const Probe: FC = () => {
    const activeScope = useRef('viewer:1/game:1')
    const cursorInitialized = useRef(false)
    const durableCursor = useRef(0)
    const [visible, setVisible] = useState<MonitorSubmission[]>([])

    useRecoveringHub({
      active: true,
      url: '/hub/monitor?game=1',
      ownerKey: 'ongoing',
      handlers: {
        ReceivedSubmissions: (raw) => {
          const message = raw as MonitorSubmission
          if (
            !monitorCursorPushIsCurrent(
              activeScope.current,
              'viewer:1/game:1',
              false,
              cursorInitialized.current,
              durableCursor.current,
              message.cursor
            )
          )
            return
          setVisible((current) => mergeSubmissionBuffer([message], current, 500))
        },
      },
      revalidate: () => {
        durableCursor.current = 10
        cursorInitialized.current = true
      },
      pollingIntervalMs: 0,
    })

    return createElement('output', null, visible.map(({ cursor }) => cursor).join(','))
  }

  try {
    await withFakeHookHubs(async (hubs) => {
      await act(async () => {
        root.render(createElement(Probe))
        await settleHookLifecycle()
      })
      assert.equal(hubs.length, 1)

      await act(async () => {
        hubs[0].emit('ReceivedSubmissions', submission(9))
        hubs[0].emit('ReceivedSubmissions', submission(10))
        hubs[0].emit('ReceivedSubmissions', submission(11))
        await settleHookLifecycle()
      })
      assert.equal(container.textContent, '11', 'only a commit newer than the initialized durable cursor is live')
    })
  } finally {
    await act(async () => {
      root.unmount()
      await settleHookLifecycle()
    })
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
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
    id: 1,
    cursor: 1,
    time: 1_000,
    type: EventType.Normal,
    values: ['same-payload'],
    user: 'operator',
    team: 'staff',
  }
  const pushed = [repeated, { ...repeated }, { ...repeated }]
  const snapshot = [{ ...repeated }, { ...repeated }]

  assert.equal(unreconciledMonitorRows(pushed, snapshot, gameEventMonitorIdentity).length, 1)
  assert.equal(gameEventMonitorIdentity(repeated), '1')
})

test('event pushes and reconnect pages merge in cursor order without duplicate identities', () => {
  const event = (id: number, cursor: number): GameEvent => ({
    id,
    cursor,
    time: cursor,
    type: EventType.Normal,
    values: [`event-${id}`],
  })
  const merged = mergeGameEventBuffer([event(2, 12), event(1, 11), event(2, 12)], [event(3, 13), event(1, 11)], 2)
  assert.deepEqual(
    merged.map(({ id, cursor }) => [id, cursor]),
    [
      [3, 13],
      [2, 12],
    ]
  )
})

const monitorSubmission = (
  id: number,
  status: AnswerResult = AnswerResult.Accepted,
  overrides: Partial<MonitorSubmission> = {}
): MonitorSubmission => ({
  id,
  cursor: id,
  time: id,
  status,
  answer: `submission-${id}`,
  ...overrides,
})

test('submission pushes and reconnect pages merge in cursor order without duplicate identities', () => {
  const merged = mergeSubmissionBuffer(
    [
      monitorSubmission(2, AnswerResult.Accepted, { cursor: 12 }),
      monitorSubmission(1, AnswerResult.WrongAnswer, { cursor: 11 }),
    ],
    [
      monitorSubmission(3, AnswerResult.Accepted, { cursor: 13 }),
      monitorSubmission(2, AnswerResult.CheatDetected, { cursor: 12 }),
    ],
    2
  )

  assert.deepEqual(
    merged.map(({ id, cursor }) => [id, cursor]),
    [
      [3, 13],
      [2, 12],
    ]
  )
  assert.equal(submissionMonitorIdentity(merged[1]), '2')
})

test('submission realtime rows remain deduplicated and capped through 5k sustained pushes', () => {
  let buffered: MonitorSubmission[] = []
  for (let cursor = 1; cursor <= 5_000; cursor += 1) {
    buffered = mergeSubmissionBuffer([monitorSubmission(cursor)], buffered, 500)
  }

  assert.equal(buffered.length, 500)
  assert.equal(new Set(buffered.map(({ id }) => id)).size, 500)
  assert.deepEqual([buffered[0].cursor, buffered.at(-1)?.cursor], [5_000, 4_501])

  buffered = mergeSubmissionBuffer(
    [monitorSubmission(5_000, AnswerResult.Accepted, { answer: 'updated duplicate' })],
    buffered,
    500
  )
  assert.equal(buffered.length, 500)
  assert.equal(buffered[0].answer, 'updated duplicate')
})

test('more than five hundred nonmatching pushes cannot evict a matching submission', () => {
  const type = AnswerResult.Accepted
  const search = 'needle'
  let buffered = receiveMonitorSubmissions(
    [monitorSubmission(1, type, { answer: 'needle' })],
    [],
    500,
    type,
    search
  ).rows

  for (let cursor = 2; cursor <= 602; cursor += 1) {
    const submission =
      cursor % 2 === 0
        ? monitorSubmission(cursor, AnswerResult.WrongAnswer, { answer: 'needle' })
        : monitorSubmission(cursor, type, { answer: 'unrelated' })
    const received = receiveMonitorSubmissions([submission], buffered, 500, type, search)
    assert.equal(received.accepted, false)
    assert.equal(received.rows, buffered, 'irrelevant traffic must not churn the scoped recovery buffer')
    buffered = received.rows
  }

  assert.deepEqual(
    buffered.map(({ id }) => id),
    [1]
  )

  const reconnect = receiveMonitorSubmissions(
    [
      monitorSubmission(2_000, type, { answer: 'needle' }),
      ...Array.from({ length: 501 }, (_, index) =>
        monitorSubmission(2_001 + index, AnswerResult.WrongAnswer, { answer: 'needle' })
      ),
    ],
    buffered,
    500,
    type,
    search
  )
  assert.equal(reconnect.accepted, true)
  assert.deepEqual(
    reconnect.rows.map(({ id }) => id),
    [2_000, 1],
    'reconnect pages must also filter before applying the recovery cap'
  )
})

test('submission recovery scopes follow filters but remain stable across pages', () => {
  const feedScope = JSON.stringify(['viewer:admin', 7])
  const acceptedScope = submissionMonitorFilterScope(feedScope, AnswerResult.Accepted, '  NEEDLE  ')
  const sameQueryOnAnotherPage = submissionMonitorFilterScope(feedScope, AnswerResult.Accepted, 'needle')
  const wrongAnswerScope = submissionMonitorFilterScope(feedScope, AnswerResult.WrongAnswer, 'needle')
  const buffered = [monitorSubmission(1, AnswerResult.Accepted, { answer: 'needle' })]

  assert.equal(acceptedScope, sameQueryOnAnotherPage)
  assert.notEqual(acceptedScope, wrongAnswerScope)
  assert.equal(currentMonitorBufferRows(acceptedScope, acceptedScope, buffered), buffered)
  assert.deepEqual(currentMonitorBufferRows(wrongAnswerScope, acceptedScope, buffered), [])
})

test('submission page one renders fifty rows while recovery retains five hundred', () => {
  const buffered = Array.from({ length: 500 }, (_, index) => monitorSubmission(index + 1))
  const snapshot = Array.from({ length: 50 }, (_, index) => monitorSubmission(501 + index))
  const visible = mergeSubmissionBuffer(buffered, snapshot, 50)

  assert.equal(visible.length, 50, 'page one must render one page, not the entire recovery buffer')
  assert.equal(buffered.length, 500, 'the page cap must not shrink reconnect recovery state')
  assert.deepEqual([visible[0].cursor, visible.at(-1)?.cursor], [550, 501])
})

test('submission live rows apply the active result and normalized search filters', () => {
  const accepted = monitorSubmission(1, AnswerResult.Accepted, {
    answer: 'FLAG{accepted}',
    team: 'Blue Team',
    challenge: 'Warm Up',
  })

  assert.equal(submissionMatchesMonitorFilter(accepted, AnswerResult.Accepted, '  blue   team  '), true)
  assert.equal(submissionMatchesMonitorFilter(accepted, AnswerResult.WrongAnswer, 'blue team'), false)
  assert.equal(submissionMatchesMonitorFilter(accepted, 'All', 'flag{accepted}'), true)
  assert.equal(submissionMatchesMonitorFilter(accepted, 'All', 'red team'), false)
  assert.equal(submissionMatchesMonitorFilter(accepted, 'All', ' '.repeat(513) + 'red team'), true)
  assert.equal(submissionMatchesMonitorFilter(accepted, 'All', '\u0085blue\u0085team'), true)

  const asciiCase = monitorSubmission(2, AnswerResult.Accepted, { team: 'Istanbul' })
  assert.equal(submissionMatchesMonitorFilter(asciiCase, 'All', 'istanbul'), true)

  const source = readFileSync('src/utils/MonitorFeed.ts', 'utf8')
  assert.doesNotMatch(source, /toLocaleLowerCase/)
})

test('a delayed game-A snapshot and late game-A push cannot enter game B', () => {
  const gameAScope = JSON.stringify([1, 1, false, ''])
  const gameBScope = JSON.stringify([2, 1, false, ''])
  assert.equal(monitorSnapshotIsCurrent(gameBScope, gameAScope, 2, 1), false)
  assert.equal(monitorSnapshotIsCurrent(gameBScope, gameBScope, 2, 2), true)
  assert.equal(monitorPushIsCurrent(2, 1, false), false)
  assert.equal(monitorPushIsCurrent(2, 2, false), true)
  assert.equal(monitorPushIsCurrent(2, 2, true), false)
  assert.equal(currentMonitorSnapshotRows(gameBScope, { scope: gameAScope, rows: ['game-A'] }), undefined)
  assert.deepEqual(currentMonitorSnapshotRows(gameBScope, { scope: gameBScope, rows: ['game-B'] }), ['game-B'])
  assert.deepEqual(currentMonitorBufferRows('account:B/game:2', 'account:A/game:1', ['game-A']), [])
  assert.deepEqual(currentMonitorBufferRows('account:B/game:2', 'account:B/game:2', ['game-B']), ['game-B'])
})

test('a large-gap fallback rebases stale pages and preserves post-checkpoint pushes in order', () => {
  const event = (cursor: number): GameEvent => ({
    id: cursor,
    cursor,
    time: cursor,
    type: EventType.Normal,
    values: [`event-${cursor}`],
  })
  const recoveredPages = Array.from({ length: 500 }, (_, index) => event(501 + index))
  const buffered = mergeGameEventBuffer([event(1501)], recoveredPages, 500)
  const rebased = rebaseGameEventBuffer(buffered, 1500)
  const snapshot = Array.from({ length: 30 }, (_, index) => event(1500 - index))
  const visible = mergeGameEventBuffer(rebased, snapshot, 500)

  assert.deepEqual(
    visible.map(({ cursor }) => cursor),
    Array.from({ length: 31 }, (_, index) => 1501 - index)
  )

  const recoveredSubmissions = Array.from({ length: 500 }, (_, index) => monitorSubmission(501 + index))
  const bufferedSubmissions = mergeSubmissionBuffer([monitorSubmission(1501)], recoveredSubmissions, 500)
  const rebasedSubmissions = rebaseSubmissionBuffer(bufferedSubmissions, 1500)
  const submissionSnapshot = Array.from({ length: 30 }, (_, index) => monitorSubmission(1500 - index))
  const visibleSubmissions = mergeSubmissionBuffer(rebasedSubmissions, submissionSnapshot, 500)

  assert.deepEqual(
    visibleSubmissions.map(({ cursor }) => cursor),
    Array.from({ length: 31 }, (_, index) => 1501 - index)
  )
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
    initial: {
      id: 1,
      cursor: 1,
      time: hubTime(1_000),
      type: EventType.Normal,
      values: ['initial'],
      user: 'one',
      team: 'alpha',
    },
    received: {
      id: 2,
      cursor: 2,
      time: hubTime(2_000),
      type: EventType.FlagSubmit,
      values: ['received'],
      user: 'two',
      team: 'beta',
    },
    inFlight: {
      id: 3,
      cursor: 3,
      time: hubTime(3_000),
      type: EventType.CheatDetected,
      values: ['in-flight'],
      user: 'three',
      team: 'gamma',
    },
    postStop: {
      id: 4,
      cursor: 4,
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
  await runCase<MonitorSubmission>({
    name: 'submissions',
    initial: {
      id: 1,
      cursor: 1,
      time: 1_000,
      status: AnswerResult.WrongAnswer,
      answer: 'initial',
      user: 'one',
      team: 'alpha',
      challenge: 'A',
    },
    received: {
      id: 2,
      cursor: 2,
      time: 2_000,
      status: AnswerResult.Accepted,
      answer: 'received',
      user: 'two',
      team: 'beta',
      challenge: 'B',
    },
    inFlight: {
      id: 3,
      cursor: 3,
      time: 3_000,
      status: AnswerResult.CheatDetected,
      answer: 'in-flight',
      user: 'three',
      team: 'gamma',
      challenge: 'C',
    },
    postStop: {
      id: 4,
      cursor: 4,
      time: 4_000,
      status: AnswerResult.NotFound,
      answer: 'post-stop',
      user: 'four',
      team: 'delta',
      challenge: 'D',
    },
    identity: submissionMonitorIdentity,
    label: (row) => row.answer,
    restProjection: (row) => ({ ...row }),
  })
})
