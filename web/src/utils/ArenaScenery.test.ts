import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { createArenaOcean } from '../pages/games/[id]/arenaAtmosphere'
import { buildArenaGeography } from '../pages/games/[id]/arenaGeography'
import { faceLocation } from '../pages/games/[id]/arenaGlobeModel'
import { createArenaSettlements, MAX_SCENIC_COUNTRIES } from '../pages/games/[id]/arenaSettlements'
import { installTestDom } from '../test/installDom'

function canvasProbe() {
  const fills: unknown[] = []
  let clears = 0
  const finite = (...args: number[]) => args.forEach((n) => assert.ok(Number.isFinite(n)))
  const context = {
    fillStyle: '' as unknown,
    globalAlpha: 1,
    setTransform: finite,
    moveTo: finite,
    lineTo: finite,
    arc: finite,
    clearRect: () => {
      clears++
    },
    beginPath() {},
    closePath() {},
    stroke() {},
    fill() {
      fills.push(this.fillStyle)
    },
    createRadialGradient: () => ({ addColorStop() {} }),
  }
  const canvas = { width: 0, height: 0, dataset: {} as DOMStringMap, getContext: () => context }
  return { canvas: canvas as unknown as HTMLCanvasElement, fills, clears: () => clears }
}

test('ocean backdrop is cached by theme/size and its bitmap stays bounded', () => {
  const probe = canvasProbe(),
    draw = createArenaOcean(probe.canvas)
  for (let frame = 0; frame < 100; frame++) draw(true, 600)
  assert.equal(probe.clears(), 1, 'rotation must not redraw the static ocean')
  draw(false, 600)
  assert.equal(probe.clears(), 2, 'theme changes redraw the ocean')
  draw(false, 6000)
  assert.equal(probe.canvas.width, 1200)
  draw(false, 10)
  assert.equal(probe.canvas.width, 400)
})

test('scenery has fixed draw bounds, hides the back hemisphere and cannot invent solve flags', async () => {
  const browser = new Window(),
    restore = installTestDom(browser)
  try {
    const country = buildArenaGeography([{ id: 'Web', challenges: [{ id: 1 }] }])[0].countries[0]
    // A controlled small grove: visible in the large view, below two pixels in
    // the narrow view. A single large country otherwise has full-height trees.
    country.landscape.trees = country.landscape.trees.map((tree) => ({ ...tree, height: 0.005 }))
    const countries = Array.from({ length: 60 }, (_, id) => ({ ...country, id, solvers: [] as { color: string }[] }))
    const probe = canvasProbe(),
      draw = createArenaSettlements(probe.canvas),
      view = faceLocation(country.location)
    draw(countries, view.yaw, view.pitch, 6000, 0)
    assert.equal(probe.canvas.width, 1200)
    assert.equal(Number(probe.canvas.dataset.visibleSettlements), MAX_SCENIC_COUNTRIES)
    assert.ok(Number(probe.canvas.dataset.visibleObjects) <= MAX_SCENIC_COUNTRIES * 22)
    const fullObjects = Number(probe.canvas.dataset.visibleObjects)
    assert.equal(Number(probe.canvas.dataset.visibleTrees), MAX_SCENIC_COUNTRIES * 14)
    assert.equal(Number(probe.canvas.dataset.visiblePeaks), MAX_SCENIC_COUNTRIES * 3)
    const unsolvedFills = probe.fills.length
    countries[0].solvers = [{ color: '#fedcba' }]
    probe.fills.length = 0
    draw(countries, view.yaw, view.pitch, 6000, 0)
    assert.equal(probe.fills.length, unsolvedFills + 1, 'only an accepted solver adds a flag')
    assert.equal(probe.fills.filter((color) => color === '#fedcba').length, 1)
    draw(countries, view.yaw, view.pitch, 240, 0)
    assert.ok(
      Number(probe.canvas.dataset.visibleObjects) < fullObjects,
      'small screens omit unreadable sub-pixel meshes'
    )
    draw(countries, view.yaw + Math.PI, -view.pitch, 560, null)
    assert.equal(Number(probe.canvas.dataset.visibleObjects), 0)
    assert.equal(Number(probe.canvas.dataset.visibleSettlements), 0)
    assert.equal(probe.clears(), 4, 'the previous frame is cleared even when everything is hidden')
  } finally {
    restore()
    await browser.happyDOM.close()
  }
})
