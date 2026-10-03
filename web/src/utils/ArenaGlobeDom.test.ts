import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { createArenaGlobe } from '../pages/games/[id]/arenaGlobe'
import type { JeopCategory } from '../pages/games/[id]/arenaJeopardy'
import { installTestDom } from '../test/installDom'

test('globe preserves snapshot/freeze boundaries, safe labels, focus and direct island links', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/27/attack#challenge=1' })
  const restore = installTestDom(browser)
  let frozen = false
  const host = document.createElement('div')
  document.body.append(host)
  const root = host.attachShadow({ mode: 'open' })
  root.innerHTML = `<div id="arena"><canvas id="globeSurface"></canvas><svg id="territories"></svg><svg id="conquestRoutes"></svg><div id="globePins"></div></div>
    <div id="jeop"></div><div id="territoryDetail"></div><input id="territorySearch">
    <span id="challengeCount"></span><span id="territorySummary"></span>
    ${['rotateBtn', 'globeLeft', 'globeRight', 'globeUp', 'globeDown', 'globeReset'].map((id) => `<button id="${id}"></button>`).join('')}`
  const canvas = root.getElementById('globeSurface') as HTMLCanvasElement
  canvas.getContext = (() => null) as typeof canvas.getContext
  const teams = [{ id: 'p1', name: '<img src=x onerror=alert(1)>', color: '#123456', x: 0, y: 0 }]
  const globe = createArenaGlobe({
    root,
    teams: () => teams,
    hills: () => [],
    frozen: () => frozen,
    motion: () => false,
    selectTeam: () => {},
  })
  const cats: JeopCategory[] = [
    {
      id: 'Web',
      name: 'Web',
      color: '#123456',
      challenges: [{ id: 1, name: '<script>title</script>', base: 100, solveCount: 0, solvers: [] }],
    },
  ]
  try {
    globe.setData(cats)
    assert.equal(root.querySelector('.island-pin')?.getAttribute('aria-pressed'), 'true')
    assert.equal(root.querySelectorAll('script,img').length, 0)
    const choice = root.querySelector<HTMLButtonElement>('.territory-choice')!
    choice.focus()
    globe.setData(cats)
    assert.equal(root.activeElement, choice, 'identical polls preserve directory focus')
    const link = root.querySelector<HTMLAnchorElement>('#territoryDetail a')!
    link.focus()
    globe.setData(cats)
    assert.equal(root.activeElement, link, 'identical polls preserve detail focus')
    frozen = true
    globe.solveByTitle(0, 0, cats[0].challenges[0].name, teams[0])
    assert.match(root.getElementById('territoryDetail')!.textContent!, /no accepted solves/)
    frozen = false
    globe.solveByTitle(0, 0, cats[0].challenges[0].name, teams[0])
    assert.match(root.getElementById('territoryDetail')!.textContent!, /1 accepted solves/)
    assert.equal(root.querySelectorAll('script,img').length, 0)
    globe.solveByTitle(0, 0, cats[0].challenges[0].name, teams[0])
    assert.match(
      root.getElementById('territoryDetail')!.textContent!,
      /1 accepted solves/,
      'duplicate feed events do not inflate solve count'
    )
    globe.setData(cats)
    assert.match(
      root.getElementById('territoryDetail')!.textContent!,
      /0 accepted solves/,
      'public snapshot is authoritative'
    )
    globe.setData([{ ...cats[0], challenges: [...cats[0].challenges, { ...cats[0].challenges[0], id: 2 }] }])
    assert.equal(
      globe.solveByTitle(0, 0, cats[0].challenges[0].name, teams[0]),
      false,
      'ambiguous feed titles must await snapshot'
    )
    globe.refreshMotion()
    assert.equal((root.getElementById('rotateBtn') as HTMLButtonElement).disabled, true)
    const yaw = root.getElementById('arena')!.dataset.globeYaw
    globe.tick(500, 0.05)
    assert.equal(root.getElementById('arena')!.dataset.globeYaw, yaw, 'motion off does not move the camera')
    globe.destroy()
    root
      .getElementById('arena')!
      .dispatchEvent(new browser.KeyboardEvent('keydown', { key: 'ArrowRight' }) as unknown as Event)
    assert.equal(root.getElementById('arena')!.dataset.globeYaw, yaw, 'teardown removes camera listeners')
  } finally {
    globe.destroy()
    host.remove()
    restore()
    await browser.happyDOM.close()
  }
})
