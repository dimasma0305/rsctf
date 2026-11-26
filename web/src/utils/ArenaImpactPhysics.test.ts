import assert from 'node:assert/strict'
import test from 'node:test'
import { createImpactPlanet, dot, MAX_EJECTA, normal } from '../pages/games/[id]/arenaImpactPhysics'

const victim = { x: 0, y: 0, z: 1 },
  source = { x: 1, y: 0, z: 0 }
function conserved(p: ReturnType<typeof createImpactPlanet>) {
  assert.ok(Math.abs(p.mass + p.escaped + p.active.reduce((m, b) => m + b.mass, 0) - 1) < 1e-8)
}
test('one impact excavates the target, heats it, and splits ejecta across the escape threshold', () => {
  const p = createImpactPlanet()
  assert.equal(p.surface(victim), 1)
  p.impact(victim, source)
  assert.ok(p.surface(victim) < 0.92 && p.surface(source) === 1)
  assert.ok(Math.max(...p.heat) > 0.4)
  const energy = p.active.map((b) => dot(b.v, b.v) / 2 - (0.38 * p.mass) / Math.hypot(b.p.x, b.p.y, b.p.z))
  assert.ok(Math.min(...energy) < 0 && Math.max(...energy) > 0)
  const excavatedMass = p.mass
  for (let i = 0; i < 1200; i++) p.advance(1 / 60, true)
  assert.ok(p.mass > excavatedMass, 'bound ejecta return mass to the ground')
  assert.ok(p.escaped > 0, 'unbound ejecta clear the scene')
  assert.ok(Math.max(...p.heat) < 0.04)
  conserved(p)
})
test('low-speed contacting parcels attract and accrete in flight', () => {
  const p = createImpactPlanet()
  p.impact(victim, source)
  const m = p.active.reduce((s, b) => s + b.mass, 0)
  p.active.splice(2)
  p.active.forEach((b, i) => {
    b.mass = m / 2
    b.p = { x: i * 0.025, y: 0, z: 1.22 }
    b.v = { x: 0.35, y: 0, z: 0 }
    b.age = 1
  })
  p.advance(1 / 30, true)
  assert.equal(p.active.length, 1)
  assert.ok(Math.abs(p.active[0].mass - m) < 1e-12)
  conserved(p)
})
test('bursts conserve mass and fixed timesteps converge across display refresh rates', () => {
  const p = createImpactPlanet(),
    q = createImpactPlanet()
  for (let i = 0; i < 160; i++) {
    const target = normal({ x: Math.cos(i * 0.8), y: 0.24, z: Math.sin(i * 0.8) })
    for (const world of [p, q]) world.impact(target, source)
  }
  assert.ok(p.active.length <= MAX_EJECTA)
  for (let i = 0; i < 120; i++) p.advance(1 / 60, true)
  for (let i = 0; i < 60; i++) q.advance(1 / 30, true)
  conserved(p)
  conserved(q)
  assert.ok(Math.abs(p.mass - q.mass) < 1e-8)
  assert.equal(p.active.length, q.active.length)
  assert.ok(Math.min(...p.radius) >= 0.43)
  p.advance(0, false)
  assert.equal(p.active.length, 0)
  assert.equal(p.advance(1 / 60, false), false)
  conserved(p)
  p.reset()
  assert.equal(p.mass, 1)
  assert.equal(p.hits, 0)
  assert.equal(p.escaped, 0)
})
