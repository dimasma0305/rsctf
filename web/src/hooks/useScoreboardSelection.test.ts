import assert from 'node:assert/strict'
import test from 'node:test'
import { Window } from 'happy-dom'
import { act, createElement, type FC } from 'react'
import { installTestDom } from '../test/installDom'
import type { ScoreboardItem } from '../Api'

test('team details retain the latest observed row across page changes and close explicitly', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/7/scoreboard' })
  const restore = installTestDom(browser)
  const { createRoot } = await import('react-dom/client')
  const { useScoreboardSelection } = await import('./useScoreboardSelection')
  const { ViewerIdentityScope } = await import('../utils/ViewerIdentity')
  let current: ReturnType<typeof useScoreboardSelection> | undefined
  const Probe: FC<{ game: number; rows: ScoreboardItem[] }> = ({ game, rows }) => {
    current = useScoreboardSelection(game, rows)
    return createElement('output', null, current.currentItem?.name)
  }
  const host = browser.document.createElement('div')
  browser.document.body.append(host)
  const root = createRoot(host)
  const first: ScoreboardItem = { id: 30, name: 'Boundary team', score: 100 }
  const updated: ScoreboardItem = { ...first, score: 200 }
  const render = async (rows: ScoreboardItem[], game = 7, scope = 'user:test:Admin') => {
    await act(async () => root.render(createElement(ViewerIdentityScope, { scope }, createElement(Probe, { game, rows }))))
  }
  try {
    await render([first])
    await act(async () => current!.selectItem(first))
    await render([updated])
    await render([{ id: 31, name: 'Next team' }])
    assert.equal(current!.currentItem?.id, 30)
    assert.equal(current!.currentItem?.score, 200)
    await act(async () => current!.clearSelection())
    await render([updated])
    assert.equal(current!.currentItem, null, 'returning to the page cannot reopen closed details')
    await act(async () => current!.selectItem(updated))
    await render([updated], 8)
    assert.equal(current!.currentItem, null, 'a different game cannot reuse the selection')
    await act(async () => current!.selectItem(updated))
    await render([updated], 8, 'anonymous')
    assert.equal(current!.currentItem, null, 'a different viewer cannot retain monitor details')
    await render([updated], 8)
    assert.equal(current!.currentItem, null, 'returning to an old viewer cannot restore a closed selection')
  } finally {
    await act(async () => root.unmount())
    await browser.happyDOM.abort()
    restore()
  }
})
