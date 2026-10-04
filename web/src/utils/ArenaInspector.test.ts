import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { createArenaInspector } from '../pages/games/[id]/arenaInspector'
import { installTestDom } from '../test/installDom'

test('arena inspector provides keyboard tabs, shareable views and explicit focus shortcuts without reads', () => {
  const browser = new Window({ url: 'https://rsctf.test/games/27/attack#challenge=12&arena=teams' })
  const restore = installTestDom(browser)
  const host = document.createElement('div')
  document.body.append(host)
  const root = host.attachShadow({ mode: 'open' })
  root.innerHTML = `<div role="tablist">${['islands', 'teams', 'activity'].map((pane) => `<button role="tab" id="tab-${pane}" data-arena-pane="${pane}" aria-controls="pane-${pane}">${pane}</button>`).join('')}</div>
    ${['islands', 'teams', 'activity'].map((pane) => `<section role="tabpanel" id="pane-${pane}" aria-labelledby="tab-${pane}"></section>`).join('')}
    <input id="territorySearch"><div id="ranklist" tabindex="0"></div><button id="browseIslands"></button><button id="browseTeams"></button>`
  const inspector = createArenaInspector(root)
  const tab = (pane: string) => root.getElementById(`tab-${pane}`) as HTMLButtonElement
  try {
    assert.equal(tab('teams').getAttribute('aria-selected'), 'true')
    assert.equal(root.getElementById('pane-islands')!.hidden, true)
    tab('teams').focus()
    tab('teams').dispatchEvent(
      new browser.KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }) as unknown as Event
    )
    assert.equal(root.activeElement, tab('activity'))
    assert.equal(tab('teams').tabIndex, -1)
    assert.equal(window.location.hash, '#challenge=12&arena=activity')
    root.getElementById('browseIslands')!.click()
    assert.equal(root.activeElement, root.getElementById('territorySearch'))
    assert.equal(window.location.hash, '#challenge=12')
    root.getElementById('browseTeams')!.click()
    assert.equal(root.activeElement, root.getElementById('ranklist'))
    tab('teams').dispatchEvent(new browser.KeyboardEvent('keydown', { key: 'Home', bubbles: true }) as unknown as Event)
    assert.equal(root.activeElement, tab('islands'))
    tab('islands').dispatchEvent(
      new browser.KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true }) as unknown as Event
    )
    assert.equal(root.activeElement, tab('activity'))
    window.history.replaceState(null, '', '#challenge=12&arena=invalid')
    window.dispatchEvent(new Event('hashchange'))
    assert.equal(tab('islands').getAttribute('aria-selected'), 'true')
    inspector.destroy()
    tab('teams').click()
    assert.equal(tab('islands').getAttribute('aria-selected'), 'true', 'teardown removes interaction listeners')
  } finally {
    inspector.destroy()
    browser.happyDOM.abort()
    restore()
  }
})
