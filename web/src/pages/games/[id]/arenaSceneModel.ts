import type { CountryGeometry } from './arenaGeography'
import type { GlobePoint } from './arenaGlobeModel'

interface Face {
  points: GlobePoint[]
  color: string
  window?: boolean
}
export interface SceneFace extends Face {
  normal: GlobePoint
  shades: string[]
}
export interface SceneObject {
  center: GlobePoint
  faces: SceneFace[]
  height: number
  kind: 'building' | 'mountain' | 'tree'
}
export const raised = (p: GlobePoint, height: number): GlobePoint => ({
  x: p.x * (1 + height),
  y: p.y * (1 + height),
  z: p.z * (1 + height),
})
const normalized = (p: GlobePoint): GlobePoint => {
  const n = Math.hypot(p.x, p.y, p.z)
  return { x: p.x / n, y: p.y / n, z: p.z / n }
}
const middle = (a: GlobePoint, b: GlobePoint) => normalized({ x: a.x + b.x, y: a.y + b.y, z: a.z + b.z })
const centerOf = (points: GlobePoint[]) =>
  normalized(points.reduce((a, p) => ({ x: a.x + p.x, y: a.y + p.y, z: a.z + p.z }), { x: 0, y: 0, z: 0 }))
// Only the fixed architectural/terrain palette reaches this cache. Share its
// colors across every mesh instead of allocating eight strings for each face.
const lightingPalette = new Map<string, string[]>()
function shadesFor(color: string) {
  let shades = lightingPalette.get(color)
  if (!shades) {
    const rgb = [1, 3, 5].map((offset) => parseInt(color.slice(offset, offset + 2), 16))
    shades = Array.from(
      { length: 8 },
      (_, i) =>
        `#${rgb
          .map((value) =>
            Math.min(255, Math.round(value * (0.68 + i * 0.065)))
              .toString(16)
              .padStart(2, '0')
          )
          .join('')}`
    )
    lightingPalette.set(color, shades)
  }
  return shades
}

/** Precompute world-space meshes once per topology, not on every animation frame. */
export function buildCountryScene(country: CountryGeometry): SceneObject[] {
  const palette = {
    modern: ['#b5d1d3', '#496b7a', '#dbe3df', '#284653'],
    town: ['#e1cba0', '#958369', '#a14f35', '#334249'],
    village: ['#dec79a', '#948366', '#725b3d', '#334249'],
  }[country.settlement.style]
  const buildings = country.settlement.buildings.map((building, index) => {
    const faces: Face[] = []
    const base = building.base,
      top = base.map((p) => raised(p, building.height))
    for (let side = 0; side < 4; side++) {
      const next = (side + 1) % 4
      faces.push({ points: [base[side], base[next], top[next], top[side]], color: palette[side % 2] })
      const a = middle(base[side], base[next]),
        left = middle(base[side], a),
        right = middle(a, base[next])
      for (let floor = 0; floor < (building.pitched ? 1 : 3); floor++) {
        const low = building.pitched ? 0.5 : 0.23 + floor * 0.23
        faces.push({
          points: [
            raised(left, building.height * low),
            raised(right, building.height * low),
            raised(right, building.height * (low + 0.09)),
            raised(left, building.height * (low + 0.09)),
          ],
          color: palette[3],
          window: true,
        })
      }
    }
    if (building.pitched && building.landmark) {
      const apex = raised(centerOf(base), building.height * 1.65)
      for (let i = 0; i < 4; i++) faces.push({ points: [top[i], top[(i + 1) % 4], apex], color: palette[2] })
    } else if (building.pitched) {
      const left = raised(middle(base[0], base[3]), building.height * 1.6)
      const right = raised(middle(base[1], base[2]), building.height * 1.6)
      faces.push(
        { points: [top[0], top[1], right, left], color: index % 2 ? palette[2] : '#ba7751' },
        { points: [top[2], top[3], left, right], color: '#72452f' },
        { points: [top[1], top[2], right], color: palette[0] },
        { points: [top[3], top[0], left], color: palette[1] }
      )
    } else {
      faces.push({ points: top, color: palette[2] })
      const center = centerOf(base)
      const inset = base.map((p) => middle(p, center))
      const lower = inset.map((p) => raised(p, building.height)),
        upper = inset.map((p) => raised(p, building.height * (building.landmark ? 1.25 : 1.1)))
      for (let i = 0; i < 4; i++)
        faces.push({ points: [lower[i], lower[(i + 1) % 4], upper[(i + 1) % 4], upper[i]], color: palette[1] })
      faces.push({ points: upper, color: building.landmark ? '#e7dfbb' : '#86a5ac' })
    }
    return { center: centerOf(base), faces, height: building.height, kind: 'building' as const }
  })
  const cones = (items: CountryGeometry['landscape']['peaks'], mountain: boolean) =>
    items.map(({ base, height }, index) => {
      const center = centerOf(base),
        top = raised(center, height)
      const colors = mountain
        ? ['#aaa58a', '#8b9275', '#637661', '#5b6a59', '#788568', '#c1baa1']
        : ['#749459', '#3e714c', '#265943', '#315f43', '#567c49']
      const faces: Face[] = base.map((a, i) => ({
        points: [a, base[(i + 1) % base.length], top],
        color: colors[(i + index) % colors.length],
      }))
      if (mountain && index % 3 !== 0)
        base.forEach((a, i) => {
          const interpolate = (p: GlobePoint) => ({
            x: p.x * 0.26 + top.x * 0.74,
            y: p.y * 0.26 + top.y * 0.74,
            z: p.z * 0.26 + top.z * 0.74,
          })
          faces.push({
            points: [interpolate(a), interpolate(base[(i + 1) % base.length]), top],
            color: i < 3 ? '#e5e7d5' : '#c2cdbf',
          })
        })
      return { center, faces, height, kind: mountain ? ('mountain' as const) : ('tree' as const) }
    })
  return [...buildings, ...cones(country.landscape.peaks, true), ...cones(country.landscape.trees, false)].map(
    (object) => ({
      ...object,
      faces: object.faces.map((face) => {
        const [a, b, c] = face.points
        const u = { x: b.x - a.x, y: b.y - a.y, z: b.z - a.z },
          v = { x: c.x - a.x, y: c.y - a.y, z: c.z - a.z }
        const normal = normalized({ x: u.y * v.z - u.z * v.y, y: u.z * v.x - u.x * v.z, z: u.x * v.y - u.y * v.x })
        return { ...face, normal, shades: shadesFor(face.color) }
      }),
    })
  )
}
