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
  let motion = false
  const host = document.createElement('div')
  document.body.append(host)
  const root = host.attachShadow({ mode: 'open' })
  root.innerHTML = `<div id="arena"><canvas id="globeSurface"></canvas><svg id="territories"></svg><svg id="conquestRoutes"></svg><div id="globePins"></div></div>
    <div id="jeop"></div><div id="territoryDetail"></div><input id="territorySearch">
    <span id="challengeCount"></span><span id="territorySummary"></span><progress id="territoryProgress"></progress><p id="territoryResults"></p>
    <button data-territory-filter="all"></button><button data-territory-filter="solved"></button><button data-territory-filter="open"></button>
    ${['rotateBtn', 'globeLeft', 'globeRight', 'globeUp', 'globeDown', 'globeReset'].map((id) => `<button id="${id}"></button>`).join('')}`
  const canvas = root.getElementById('globeSurface') as HTMLCanvasElement
  canvas.getContext = (() => null) as typeof canvas.getContext
  const teams = [{ id: 'p1', name: '<img src=x onerror=alert(1)>', color: '#123456', x: 0, y: 0 }]
  const globe = createArenaGlobe({
    root,
    teams: () => teams,
    hills: () => [],
    frozen: () => frozen,
    motion: () => motion,
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
    assert.equal(root.querySelectorAll('.continent-outline').length, 1)
    assert.equal(root.querySelector('.country')?.getAttribute('data-continent'), 'Web')
    assert.equal(root.querySelector('.continent-group')?.getAttribute('aria-label'), 'Web continent')
    const originalCountry = root.querySelector('.country')
    const cityLayer = root.querySelector('.settlements')
    assert.equal(cityLayer?.getAttribute('aria-hidden'), 'true')
    assert.equal(root.querySelectorAll('.settlement').length, 24)
    assert.equal(root.querySelectorAll('.country-border').length, 1)
    assert.equal(root.querySelector('.country-border')?.getAttribute('vector-effect'), 'non-scaling-stroke')
    const originalCoast = originalCountry?.getAttribute('d')
    assert.equal(root.querySelectorAll('script,img').length, 0)
    const choice = root.querySelector<HTMLButtonElement>('.territory-choice')!
    choice.focus()
    globe.setData(cats)
    assert.equal(root.activeElement, choice, 'identical polls preserve directory focus')
    assert.equal(root.querySelector('.country'), originalCountry, 'polls reuse geometry and DOM')
    assert.equal(root.querySelector('.settlements'), cityLayer, 'polls do not rebuild city decorations')
    assert.equal(root.querySelector('.country')?.getAttribute('d'), originalCoast)
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
    ;(root.querySelector('[data-territory-filter="open"]') as HTMLButtonElement).click()
    assert.equal(choice.hidden, true)
    assert.match(root.getElementById('territoryResults')!.textContent!, /No countries match/)
    ;(root.querySelector('[data-territory-filter="solved"]') as HTMLButtonElement).click()
    assert.equal(choice.hidden, false)
    assert.equal((root.getElementById('territoryProgress') as HTMLProgressElement).value, 1)
    ;(root.querySelector('[data-territory-filter="all"]') as HTMLButtonElement).click()
    globe.focusTeam('p1')
    const route = root.querySelector('#conquestRoutes path')
    assert.ok(route)
    globe.layout()
    assert.equal(root.querySelector('#conquestRoutes path'), route, 'camera redraw reuses the existing route node')
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
    assert.equal(root.querySelectorAll('.continent-group .territory-choice').length, 2)
    assert.equal(root.querySelectorAll('.continent-outline').length, 1, 'same-category countries share one continent')
    assert.equal(
      globe.solveByTitle(0, 0, cats[0].challenges[0].name, teams[0]),
      false,
      'ambiguous feed titles must await snapshot'
    )
    ;(root.querySelector('.continent-choice') as HTMLButtonElement).click()
    assert.equal(window.location.hash, '#continent=Web')
    assert.equal(root.querySelectorAll('.territory-choice[aria-pressed="true"]').length, 0)
    globe.setData([{ ...cats[0], id: 'Crypto', name: 'Crypto', challenges: cats[0].challenges }])
    assert.equal(
      root.querySelector('.country')?.getAttribute('data-continent'),
      'Crypto',
      'category changes rebuild topology even when challenge IDs are unchanged'
    )
    assert.equal(root.querySelector('.continent-label')?.textContent, 'Crypto')
    globe.refreshMotion()
    assert.equal((root.getElementById('rotateBtn') as HTMLButtonElement).disabled, true)
    ;(root.getElementById('globeRight') as HTMLButtonElement).click()
    const yaw = root.getElementById('arena')!.dataset.globeYaw
    globe.tick(500, 0.05)
    assert.equal(root.getElementById('arena')!.dataset.globeYaw, yaw, 'motion off does not move the camera')
    motion = true
    globe.focusTeam('p1')
    assert.equal(root.getElementById('arena')!.dataset.globeYaw, yaw, 'focus does not snap with motion on')
    globe.tick(516, 0.016)
    assert.notEqual(root.getElementById('arena')!.dataset.globeYaw, yaw)
    globe.focusTeam(null)
    const deselected = root.getElementById('arena')!.dataset.globeYaw
    globe.tick(520, 0.004)
    assert.equal(
      root.getElementById('arena')!.dataset.globeYaw,
      deselected,
      'clearing selection cancels its transition'
    )
    globe.focusTeam('p1')
    motion = false
    globe.refreshMotion()
    const settled = root.getElementById('arena')!.dataset.globeYaw
    globe.tick(532, 0.016)
    assert.equal(
      root.getElementById('arena')!.dataset.globeYaw,
      settled,
      'disabling motion settles a pending transition'
    )
    globe.destroy()
    root
      .getElementById('arena')!
      .dispatchEvent(new browser.KeyboardEvent('keydown', { key: 'ArrowRight' }) as unknown as Event)
    assert.equal(root.getElementById('arena')!.dataset.globeYaw, settled, 'teardown removes camera listeners')
  } finally {
    globe.destroy()
    host.remove()
    restore()
    await browser.happyDOM.close()
  }
})
