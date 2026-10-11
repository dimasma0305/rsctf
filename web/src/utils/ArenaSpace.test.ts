import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { createArenaSpace, parallaxOffset, starField } from '../pages/games/[id]/arenaSpace'
import { installTestDom } from '../test/installDom'

test('star field is deterministic, bounded, and seed-sensitive', () => {
  const sky = starField(7, 120)
  assert.equal(sky.length, 120)
  assert.deepEqual(sky, starField(7, 120))
  assert.notDeepEqual(sky, starField(8, 120))
  for (const star of sky) {
    assert.ok(star.x >= 0 && star.x < 1 && star.y >= 0 && star.y < 1)
    assert.ok(star.r >= 0.6 && star.r <= 2.2)
    assert.ok(star.a >= 0.35 && star.a <= 1)
  }
})

test('parallax stays bounded for any camera yaw and ignores a non-finite yaw', () => {
  for (const yaw of [0, 1, Math.PI, 40, -1234.5]) {
    const { x, y } = parallaxOffset(yaw, 24)
    assert.ok(Math.abs(x) <= 24 && Math.abs(y) <= 6)
  }
  assert.deepEqual(parallaxOffset(Number.NaN, 24), { x: 0, y: 6 })
})

test('backdrop layers sit first in the globe panel, move only when the yaw changes, and tear down', () => {
  const browser = new Window({ url: 'https://rsctf.test/games/27/attack' })
  const restore = installTestDom(browser)
  try {
    const wrap = document.createElement('section')
    wrap.innerHTML = '<div id="arena"></div>'
    document.body.append(wrap)
    const space = createArenaSpace(wrap)
    assert.deepEqual(
      [...wrap.children].map((c) => c.className),
      ['space-nebula', 'space-far', 'space-near', '']
    )
    assert.ok([...wrap.children].slice(0, 3).every((c) => c.getAttribute('aria-hidden') === 'true'))
    space.paint(true)
    space.paint(false)
    space.parallax(0.5)
    const far = wrap.querySelector<HTMLElement>('.space-far')!
    const near = wrap.querySelector<HTMLElement>('.space-near')!
    assert.match(far.style.transform, /translate3d\(4\.31px, 1\.97px, 0\)/)
    assert.match(near.style.transform, /translate3d\(11\.51px, 5\.27px, 0\)/)
    const before = near.style.transform
    space.parallax(0.5)
    assert.equal(near.style.transform, before)
    space.parallax(1.5)
    assert.notEqual(near.style.transform, before)
    space.destroy()
    assert.deepEqual(
      [...wrap.children].map((c) => c.id),
      ['arena']
    )
  } finally {
    restore()
  }
})
