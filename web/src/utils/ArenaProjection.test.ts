import assert from 'node:assert/strict'
import test from 'node:test'
import { buildArenaGeography } from '../pages/games/[id]/arenaGeography'
import { faceLocation, type GlobePoint } from '../pages/games/[id]/arenaGlobeModel'
import { projectSurface } from '../pages/games/[id]/arenaProjection'
import { buildCountryScene } from '../pages/games/[id]/arenaSceneModel'

test('land follows the spherical horizon instead of closing a straight chord inside the ocean', () => {
  // A cap centered on the right horizon: its visible sliver must reach x=935.
  const ring = Array.from({ length: 64 }, (_, i) => {
    const angle = (i * Math.PI) / 32
    return { x: Math.cos(0.6), y: Math.sin(0.6) * Math.sin(angle), z: Math.sin(0.6) * Math.cos(angle) }
  })
  const { fill, edge } = projectSurface(ring, 0, 0)
  assert.match(fill, /A435,435 0 0 [01]/, 'fill connects through the curved silhouette')
  assert.doesNotMatch(edge, /[AZ]/, 'the horizon clip must not become a false inland border')
  const arcs = [...fill.matchAll(/A435,435 0 0 [01] ([\d.]+),([\d.]+)/g)]
  assert.ok(arcs.length)
  for (const [, x, y] of arcs) assert.ok(Math.abs(Math.hypot(Number(x) - 500, Number(y) - 500) - 435) < 0.01)
  assert.deepEqual(projectSurface(ring, Math.PI / 2, 0), { fill: '', edge: '' })
  assert.equal(projectSurface(ring, -Math.PI / 2, 0).fill, projectSurface(ring, -Math.PI / 2, 0).edge)
})

test('rotating natural concave countries produces finite surface paths and bounded circular clips', () => {
  const continents = buildArenaGeography(
    ['Web', 'Pwn', 'Misc', 'Crypto'].map((id, i) => ({
      id,
      challenges: Array.from({ length: 8 }, (_, j) => ({ id: i * 8 + j })),
    }))
  )
  for (let angle = 0; angle < 24; angle++)
    for (const continent of continents)
      for (const country of continent.countries) {
        const paths = projectSurface(country.coast, (angle * Math.PI) / 12, 0.5)
        assert.doesNotMatch(paths.fill + paths.edge, /NaN|Infinity/)
        assert.doesNotMatch(paths.edge, /A/, 'only real country edges are stroked')
      }
})

const dot = (a: GlobePoint, b: GlobePoint) => a.x * b.x + a.y * b.y + a.z * b.z
// Independent gnomonic point-in-polygon test: great-circle edges become lines.
function contains(coast: GlobePoint[], point: GlobePoint, center: GlobePoint) {
  const east = { x: center.z, y: 0, z: -center.x }
  const north = { x: center.y * east.z, y: center.z * east.x - center.x * east.z, z: -center.y * east.x }
  const project = (p: GlobePoint) => ({ x: dot(p, east) / dot(p, center), y: dot(p, north) / dot(p, center) })
  const p = project(point),
    vertices = coast.map(project)
  let inside = false
  for (let i = 0, j = vertices.length - 1; i < vertices.length; j = i++) {
    const a = vertices[i],
      b = vertices[j]
    if (a.y > p.y !== b.y > p.y && p.x < ((b.x - a.x) * (p.y - a.y)) / (b.y - a.y) + a.x) inside = !inside
  }
  return inside
}

test('settlements have varied architecture with spherical foundations inside their own country', () => {
  const styles = new Set<string>()
  for (const count of [1, 3, 12, 150])
    for (const continent of buildArenaGeography(
      ['Web', 'Crypto', 'Forensics'].map((id) => ({
        id,
        challenges: Array.from({ length: count }, (_, i) => ({ id: i + 1 })),
      }))
    )) {
      for (const country of continent.countries) {
        styles.add(country.settlement.style)
        assert.ok(country.settlement.buildings.length <= 8)
        for (const building of country.settlement.buildings) {
          assert.ok(building.height > 0 && building.height <= 0.075)
          for (const p of building.base) {
            assert.ok(Math.abs(Math.hypot(p.x, p.y, p.z) - 1) < 1e-10, 'foundation rests on the globe surface')
            assert.ok(
              contains(country.coast, p, continent.location),
              `no buildings at sea or in a neighboring country: ${continent.id}/${country.id}, count=${count}, style=${country.settlement.style}, p=${JSON.stringify(p)}`
            )
          }
        }
        const landscape = country.landscape
        assert.equal(landscape.peaks.length, 5)
        assert.equal(landscape.trees.length, 18)
        for (const p of [
          ...landscape.patches.flatMap((p) => p.points),
          ...landscape.fields.flatMap((p) => p.points),
          ...landscape.lanes.flat(),
          ...landscape.lake,
          ...landscape.tributary,
          ...landscape.bridge,
          ...landscape.road,
          ...landscape.river,
          ...landscape.peaks.flatMap((p) => p.base),
          ...landscape.trees.flatMap((t) => t.base),
        ]) {
          assert.ok(Math.abs(Math.hypot(p.x, p.y, p.z) - 1) < 1e-10, 'scenery foundations hug the sphere')
          assert.ok(
            contains(country.coast, p, continent.location),
            `terrain must stay in its own country: ${continent.id}/${country.id}`
          )
        }
        const scene = buildCountryScene(country)
        assert.ok(scene.length <= 31, 'bounded detail per country')
        for (const object of scene)
          for (const face of object.faces) {
            assert.ok(Math.abs(Math.hypot(face.normal.x, face.normal.y, face.normal.z) - 1) < 1e-8)
            assert.equal(face.shades.length, 8, 'lighting colors are precomputed, not allocated per frame')
            assert.ok(face.shades.every((color) => /^#[a-f0-9]{6}$/.test(color)))
            for (const p of face.points) {
              const radius = Math.hypot(p.x, p.y, p.z)
              assert.ok(Number.isFinite(radius) && radius >= 0.999 && radius < 1.11, 'finite outward-facing relief')
            }
          }
        const view = faceLocation(country.location)
        assert.ok(projectSurface(country.coast, view.yaw, view.pitch).fill)
      }
    }
  assert.deepEqual([...styles].sort(), ['modern', 'town', 'village'])
})

test('early normal culling agrees with projected triangle winding through a full orbit', () => {
  const countries = buildArenaGeography([{ id: 'Web', challenges: [{ id: 1 }, { id: 2 }, { id: 3 }] }])[0].countries
  const palette = new Map<string, string[]>()
  for (const country of countries)
    for (const object of buildCountryScene(country))
      for (const face of object.faces) {
        if (palette.has(face.color))
          assert.equal(face.shades, palette.get(face.color), 'fixed colors share lighting ramps')
        palette.set(face.color, face.shades)
        for (let turn = 0; turn < 24; turn++) {
          const yaw = (turn * Math.PI) / 12,
            pitch = 0.4
          const rotate = (p: GlobePoint) => ({
            x: p.x * Math.cos(yaw) + p.z * Math.sin(yaw),
            y: p.y * Math.cos(pitch) - (p.z * Math.cos(yaw) - p.x * Math.sin(yaw)) * Math.sin(pitch),
          })
          const [a, b, c] = face.points.map(rotate)
          const winding = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
          const n = face.normal
          const facing = n.y * Math.sin(pitch) + (n.z * Math.cos(yaw) - n.x * Math.sin(yaw)) * Math.cos(pitch)
          if (Math.abs(winding) > 1e-12) assert.equal(facing > 0, winding > 0)
        }
      }
})
