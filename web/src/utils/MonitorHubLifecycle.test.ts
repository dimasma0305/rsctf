import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { act, createElement, type FC, useCallback, useEffect, useState } from 'react'
import { installTestDom } from '../test/installDom'

const monitorPages = [
  { path: 'src/pages/games/[id]/monitor/Events.tsx', fetchName: 'fetchEvents' },
  { path: 'src/pages/games/[id]/monitor/Submissions.tsx', fetchName: 'fetchSubmissions' },
]

test('monitor hubs survive timing revalidation, stop at the boundary, and reconcile afterward', () => {
  for (const { path, fetchName } of monitorPages) {
    const source = readFileSync(path, 'utf8')

    assert.match(source, /const \{ finished \} = useGameStatus\(game\)/, path)
    assert.match(source, /const monitorConnectionActive = Boolean\(game\?\.end\) && !finished/, path)
    assert.match(source, /if \(monitorConnectionActive\) \{[\s\S]*?new signalR\.HubConnectionBuilder\(\)/, path)
    assert.match(source, /\}, \[monitorConnectionActive, numId, t\]\)/, path)
    assert.doesNotMatch(source, /\}, \[game, numId, t\]\)/, path)
    assert.match(source, new RegExp(`const ${fetchName} = useCallback`), path)
    assert.match(source, new RegExp(`useRevalidateWhenPollingStops\\(monitorConnectionActive, ${fetchName}\\)`), path)
    assert.ok(
      source.lastIndexOf('useRevalidateWhenPollingStops(') > source.indexOf('new signalR.HubConnectionBuilder()'),
      `${path} must reconcile after the hub ownership effect so cleanup runs first`
    )
  }
})

class BoundaryFeed {
  readonly authoritative: string[]
  requests = 0
  stops = 0
  inFlight: string | undefined
  private listener: ((message: string) => void) | undefined

  constructor(readonly name: string) {
    this.authoritative = [`${name}-initial`]
  }

  connect(listener: (message: string) => void) {
    this.listener = listener
    return () => {
      this.listener = undefined
      this.stops += 1
    }
  }

  acceptWhileBroadcastIsInFlight(message: string) {
    // The server has committed the row, but the hub callback has not reached
    // this client before its lifecycle cleanup runs.
    this.authoritative.unshift(message)
    this.inFlight = message
  }

  async read() {
    this.requests += 1
    return [...this.authoritative]
  }
}

test('each monitor feed recovers one accepted in-flight boundary message with one final REST read', async (context) => {
  for (const name of ['events', 'submissions']) {
    await context.test(name, async () => {
      const browser = new Window({ url: `https://rsctf.test/games/1/monitor/${name}` })
      const restoreDom = installTestDom(browser)
      const { useRevalidateWhenPollingStops } = await import('../hooks/useGame')
      const { createRoot } = await import('react-dom/client')
      const container = browser.document.createElement('div')
      browser.document.body.append(container)
      const root = createRoot(container)
      const feed = new BoundaryFeed(name)
      ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

      const Probe: FC<{ active: boolean }> = ({ active }) => {
        const [rows, setRows] = useState<string[]>([])
        const refresh = useCallback(async () => setRows(await feed.read()), [feed])

        useEffect(() => {
          void refresh()
        }, [refresh])

        useEffect(() => {
          if (!active) return
          return feed.connect((message) => setRows((current) => [message, ...current]))
        }, [active, feed])

        // Match the production pages: hub ownership is declared first, so its
        // cleanup happens before this one-shot final reconciliation effect.
        useRevalidateWhenPollingStops(active, refresh)

        return createElement('output', null, rows.join(','))
      }

      try {
        await act(async () => {
          root.render(createElement(Probe, { active: true }))
          await Promise.resolve()
        })
        assert.equal(feed.requests, 1)
        assert.equal(container.textContent, `${name}-initial`)

        const boundaryMessage = `${name}-accepted-at-deadline`
        feed.acceptWhileBroadcastIsInFlight(boundaryMessage)

        await act(async () => {
          root.render(createElement(Probe, { active: false }))
          await Promise.resolve()
        })
        assert.equal(feed.stops, 1)
        assert.equal(feed.inFlight, boundaryMessage)
        assert.equal(feed.requests, 2, 'initial snapshot plus exactly one final reconciliation')
        assert.equal(container.textContent, `${boundaryMessage},${name}-initial`)

        await act(async () => {
          root.render(createElement(Probe, { active: false }))
          await Promise.resolve()
        })
        assert.equal(feed.requests, 2, 'remaining stopped must not add polling')
      } finally {
        await act(async () => root.unmount())
        delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
        await browser.happyDOM.close()
        restoreDom()
      }
    })
  }
})
