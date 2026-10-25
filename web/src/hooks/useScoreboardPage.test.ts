import assert from 'node:assert/strict'
import test from 'node:test'
import { Window } from 'happy-dom'
import { act, createElement, type FC } from 'react'
import { installTestDom } from '../test/installDom'

test('paging, lookup and search each fetch a bounded server page through one SWR owner', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/7/scoreboard' })
  const restore = installTestDom(browser)
  const { createRoot } = await import('react-dom/client')
  const { SWRConfig } = await import('swr')
  const { useScoreboardPage } = await import('./useScoreboardPage')
  const calls: string[] = []
  let current: ReturnType<typeof useScoreboardPage> | undefined
  const Probe: FC<{ division: number | null; mobile: boolean }> = ({ division, mobile }) => {
    current = useScoreboardPage(7, division, mobile)
    return createElement('output', null, current.scoreboard?.total)
  }
  const host = browser.document.createElement('div')
  browser.document.body.append(host)
  const root = createRoot(host)
  const cache = new Map()
  const render = (division: number | null = null, mobile = false) => root.render(createElement(SWRConfig, {
    value: {
      provider: () => cache,
      dedupingInterval: 0,
      fetcher: async (path: string) => {
        calls.push(path)
        const query = new URL(path, browser.location.origin).searchParams
        return { total: 1000, skip: query.has('locateTeam') ? 900 : Number(query.get('skip')),
          count: Number(query.get('count')), items: [], snapshotVersion: 'one' }
      },
    },
  }, createElement(Probe, { division, mobile })))
  const settle = async () => { await act(async () => { await new Promise(resolve => setTimeout(resolve, 20)) }) }
  try {
    await act(async () => render())
    await settle()
    assert.equal(calls.length, 1)
    assert.match(calls[0], /skip=0&count=30/)
    await act(async () => current!.paging.setPage(3))
    await settle()
    assert.match(calls.at(-1)!, /skip=60&count=30/)
    await act(async () => current!.paging.findTeam('Team 901'))
    await settle()
    assert.match(calls.at(-1)!, /locateTeam=Team\+901/)
    assert.equal(current!.paging.page, 31)
    await act(async () => current!.paging.setKeyword('needle'))
    await act(async () => { await new Promise(resolve => setTimeout(resolve, 350)) })
    await settle()
    assert.match(calls.at(-1)!, /skip=0&count=30&search=needle/)
    assert.doesNotMatch(calls.at(-1)!, /locateTeam/)
    await act(async () => render(2, true))
    await settle()
    assert.match(calls.at(-1)!, /count=10&divisionId=2/)
    assert.ok(calls.every(path => path.includes('/scoreboard/page?')))
  } finally {
    await act(async () => root.unmount())
    await browser.happyDOM.abort()
    restore()
  }
})
