import assert from 'node:assert/strict'
import test from 'node:test'
import { buildArenaGeography } from '../pages/games/[id]/arenaGeography'
import type { GlobePoint } from '../pages/games/[id]/arenaGlobeModel'

const dot = (a: GlobePoint, b: GlobePoint) => a.x * b.x + a.y * b.y + a.z * b.z
const area = (points: GlobePoint[], origin: GlobePoint) =>
  Math.abs(
    points.reduce((sum, a, i) => {
      const b = points[(i + 1) % points.length]
      const det =
        origin.x * (a.y * b.z - a.z * b.y) + origin.y * (a.z * b.x - a.x * b.z) + origin.z * (a.x * b.y - a.y * b.x)
      return sum + 2 * Math.atan2(det, 1 + dot(origin, a) + dot(a, b) + dot(b, origin))
    }, 0)
  )
const category = (id: string, count: number, start = 1) => ({
  id,
  challenges: Array.from({ length: count }, (_, i) => ({ id: start + i })),
})

test('categories form deterministic continents independent of snapshot ordering and scores', () => {
  const categories = [category('Web', 4), category('Crypto', 3, 10), category('Empty', 0)]
  const geography = buildArenaGeography(categories)
  assert.equal(geography.length, 2)
  assert.deepEqual(
    geography.map((c) => [c.id, c.countries.length]),
    [
      ['Crypto', 3],
      ['Web', 4],
    ]
  )
  assert.deepEqual(
    geography,
    buildArenaGeography(
      categories
        .toReversed()
        .map((c) => ({
          ...c,
          challenges: c.challenges.toReversed().map((ch) => ({ ...ch, solveCount: 30, base: 100 })),
        }))
    )
  )
  assert.deepEqual(buildArenaGeography([]), [])
  assert.deepEqual(buildArenaGeography([category('Empty', 0)]), [])
  assert.notDeepEqual(geography[0].coast, buildArenaGeography([category('Different', 3), category('Web', 4)])[0].coast)
})

test('natural coastline and country borders tile a sphere without gaps or overlap', () => {
  for (const count of [1, 2, 3, 12, 49, 150, 500]) {
    const [continent] = buildArenaGeography([category('Forensics', count)])
    assert.equal(continent.countries.length, count)
    assert.equal(continent.coast.length, 256)
    const wholeArea = area(continent.coast, continent.location)
    const countryAreas = continent.countries.map((c) => area(c.coast, continent.location))
    assert.ok(
      countryAreas.every((a) => a > 0),
      'every challenge has a nonempty country'
    )
    assert.ok(
      Math.abs(countryAreas.reduce((s, a) => s + a, 0) - wholeArea) / wholeArea < 0.001,
      'countries cover the continent without overlapping or leaving sea seams'
    )
    for (const p of [
      continent.location,
      continent.labelLocation,
      ...continent.coast,
      ...continent.countries.flatMap((c) => [c.location, ...c.coast]),
    ]) {
      assert.ok(Number.isFinite(p.x) && Number.isFinite(p.y) && Number.isFinite(p.z))
      assert.ok(Math.abs(Math.hypot(p.x, p.y, p.z) - 1) < 1e-10)
    }
    if (count > 1) {
      const keys = continent.countries.map(
        (c) => new Set(c.coast.map((p) => [p.x, p.y, p.z].map((n) => n.toFixed(7)).join(',')))
      )
      for (let i = 0; i < keys.length; i++)
        assert.ok(
          keys.some((other, j) => i !== j && [...keys[i]].filter((p) => other.has(p)).length >= 2),
          'countries share real borders, not separate island outlines'
        )
    }
  }
})

test('continent coastlines stay in disjoint spherical caps even with many categories', () => {
  for (const count of [2, 5, 12, 40]) {
    const continents = buildArenaGeography(Array.from({ length: count }, (_, i) => category(`Category ${i}`, 2, i * 2)))
    const radii = continents.map((c) =>
      Math.max(...c.coast.map((p) => Math.acos(Math.max(-1, Math.min(1, dot(c.location, p))))))
    )
    for (let i = 0; i < count; i++)
      for (let j = i + 1; j < count; j++) {
        const separation = Math.acos(Math.max(-1, Math.min(1, dot(continents[i].location, continents[j].location))))
        assert.ok(radii[i] + radii[j] < separation, 'category landmasses cannot overlap')
      }
  }
})
