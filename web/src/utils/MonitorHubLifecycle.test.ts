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
  },
  {
    path: 'src/pages/games/[id]/monitor/Submissions.tsx',
    fetchName: 'fetchSubmissions',
    identityName: 'submissionMonitorIdentity',
  },
]

test('monitor hubs survive timing revalidation, stop at the boundary, and reconcile afterward', () => {
  for (const { path, fetchName, identityName } of monitorPages) {
    const source = readFileSync(path, 'utf8')

    assert.match(source, /const \{ finished \} = useGameStatus\(game\)/, path)
    assert.match(source, /const monitorConnectionActive = Boolean\(game\?\.end\) && !finished/, path)
    assert.match(source, /if \(monitorConnectionActive\) \{[\s\S]*?new signalR\.HubConnectionBuilder\(\)/, path)
    assert.match(source, /\}, \[monitorConnectionActive, numId, t\]\)/, path)
    assert.doesNotMatch(source, /\}, \[game, numId, t\]\)/, path)
    assert.match(source, new RegExp(`const ${fetchName} = useCallback`), path)
    assert.match(source, new RegExp(`useRevalidateWhenPollingStops\\(monitorConnectionActive, ${fetchName}\\)`), path)
    assert.match(source, new RegExp(`unreconciledMonitorRows\\([^;]+${identityName}\\)`), path)
    assert.ok(
      source.lastIndexOf('useRevalidateWhenPollingStops(') > source.indexOf('new signalR.HubConnectionBuilder()'),
      `${path} must reconcile after the hub ownership effect so cleanup runs first`
    )
  }
})

class BoundaryFeed<Row> {
  readonly authoritative: Row[]
  requests = 0
  stops = 0
  private listener: ((message: Row) => void) | undefined
  private deferRead = false
  private resolveRead: (() => void) | undefined

  constructor(
    initial: Row,
    private readonly restProjection: (row: Row) => Row
  ) {
    this.authoritative = [initial]
  }

  connect(listener: (message: Row) => void) {
    this.listener = listener
    return () => {
      // SignalR stop is asynchronous. Keep the listener until completeStop so
      // the test can exercise a callback racing the final REST response.
      this.stops += 1
    }
  }

  acceptAndBroadcast(message: Row) {
    this.authoritative.unshift(message)
    this.listener?.(message)
  }

  acceptWhileBroadcastIsInFlight(message: Row) {
    this.authoritative.unshift(message)
  }

  deferNextRead() {
    this.deferRead = true
  }

  read() {
    this.requests += 1
    const snapshot = this.authoritative.map(this.restProjection)
    if (!this.deferRead) return Promise.resolve(snapshot)

    this.deferRead = false
    return new Promise<Row[]>((resolve) => {
      this.resolveRead = () => {
        this.resolveRead = undefined
        resolve(snapshot)
      }
    })
  }

  resolveDeferredRead() {
    this.resolveRead?.()
  }

  completeStop() {
    this.listener = undefined
  }
}

interface BoundaryCase<Row> {
  name: string
  initial: Row
  received: Row
  inFlight: Row
  postFetch: Row
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

test('each monitor feed reconciles received, in-flight, and post-fetch boundary messages once', async (context) => {
  const runCase = async <Row>({
    name,
    initial,
    received,
    inFlight,
    postFetch,
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
        const refresh = useCallback(async () => setSnapshotRows(await feed.read()), [feed])

        useEffect(() => {
          void refresh()
        }, [refresh])

        useEffect(() => {
          if (!active) return
          return feed.connect((message) => {
            pushedRows.current = [message, ...pushedRows.current]
            publishPush((version) => version + 1)
          })
        }, [active, feed])

        // Match the production pages: hub ownership is declared first, so its
        // cleanup happens before this one-shot final reconciliation effect.
        useRevalidateWhenPollingStops(active, refresh)

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

        feed.acceptWhileBroadcastIsInFlight(inFlight)
        feed.deferNextRead()

        await act(async () => {
          root.render(createElement(Probe, { active: false }))
          await Promise.resolve()
        })
        assert.equal(feed.stops, 1)
        assert.equal(feed.requests, 2, 'initial snapshot plus exactly one final reconciliation')

        // stop() is still settling and this callback was already queued. The
        // final REST request captured its snapshot just before this row landed.
        await act(async () => feed.acceptAndBroadcast(postFetch))
        feed.completeStop()
        await act(async () => {
          feed.resolveDeferredRead()
          await Promise.resolve()
        })

        assert.equal(
          container.textContent,
          [postFetch, inFlight, received, initial].map(label).join(','),
          'REST overlap is emitted once, in-flight data is recovered, and the post-fetch push keeps newest-first order'
        )

        await act(async () => {
          root.render(createElement(Probe, { active: false }))
          await Promise.resolve()
        })
        assert.equal(feed.requests, 2, 'remaining stopped must not add polling')
      } finally {
        feed.completeStop()
        await act(async () => {
          feed.resolveDeferredRead()
          root.unmount()
          await Promise.resolve()
        })
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
    postFetch: {
      time: hubTime(4_000),
      type: EventType.Download,
      values: ['post-fetch'],
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
    postFetch: {
      time: hubTime(4_000),
      status: AnswerResult.NotFound,
      answer: 'post-fetch',
      user: 'four',
      team: 'delta',
      challenge: 'D',
    },
    identity: submissionMonitorIdentity,
    label: (row) => row.answer ?? '',
    restProjection: (row) => ({ ...row, time: Date.parse(String(row.time)) }),
  })
})
