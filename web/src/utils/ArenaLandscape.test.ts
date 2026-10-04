import assert from 'node:assert/strict'
import test from 'node:test'
import { buildCountryDetails } from '../pages/games/[id]/arenaLandscape'

interface Point {
  x: number
  y: number
}
const cross = (a: Point, b: Point, c: Point) => (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
const intersects = (a: Point, b: Point, c: Point, d: Point) =>
  cross(a, b, c) * cross(a, b, d) <= 1e-14 && cross(c, d, a) * cross(c, d, b) <= 1e-14
const crosses = (line: Point[], ring: Point[]) =>
  line.slice(1).some((b, i) => ring.some((c, j) => intersects(line[i], b, c, ring[(j + 1) % ring.length])))
const inside = (p: Point, polygon: Point[]) => {
  let result = false
  for (let i = 0, j = polygon.length - 1; i < polygon.length; j = i++) {
    const a = polygon[i],
      b = polygon[j]
    if (a.y > p.y !== b.y > p.y && p.x < ((b.x - a.x) * (p.y - a.y)) / (b.y - a.y) + a.x) result = !result
  }
  return result
}

test('seeded country plans connect streets, river, bridge and lake without building over roads', () => {
  const styles = new Set<string>()
  for (let seed = 1; seed <= 120; seed++) {
    // Inspect the unwarped plan independently of the spherical renderer.
    const { landscape: land, settlement } = buildCountryDetails({ x: 0, y: 0 }, 1, seed, (p) => ({ ...p, z: 1 }))
    styles.add(settlement.style)
    assert.ok(crosses(land.road, land.bridge), 'main street traverses the bridge')
    assert.ok(crosses(land.river, land.bridge), 'bridge spans the actual river')
    assert.ok(inside(land.river.at(-1)!, land.lake), 'river terminates in the lake')
    for (const lane of land.lanes)
      assert.ok(
        land.road.slice(1).some((p, i) => lane.slice(1).some((q, j) => intersects(land.road[i], p, lane[j], q))),
        'side streets connect to the main street'
      )
    for (const building of settlement.buildings) {
      assert.equal(crosses(land.road, building.base), false, 'building plots leave the main street clear')
      for (const lane of land.lanes)
        assert.equal(crosses(lane, building.base), false, 'side streets stay between building plots')
      assert.ok(building.base.every((p) => Math.hypot(p.x, p.y) < 1))
    }
    assert.equal(settlement.buildings.filter((b) => b.landmark).length, 1)
    assert.equal(land.fields.length, 6)
    assert.ok(land.patches.length <= 10)
  }
  assert.equal(styles.size, 3)
})
