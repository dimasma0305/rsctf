import type { CountryGeometry } from './arenaGeography'
import type { GlobePoint } from './arenaGlobeModel'

export interface SceneFace {
  points: GlobePoint[]
  color: string
  window?: boolean
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

/** Precompute world-space meshes once per topology, not on every animation frame. */
export function buildCountryScene(country: CountryGeometry): SceneObject[] {
  const palette = {
    modern: ['#b5d1d3', '#496b7a', '#dbe3df', '#284653'],
    town: ['#e1cba0', '#958369', '#a14f35', '#334249'],
    village: ['#dec79a', '#948366', '#725b3d', '#334249'],
  }[country.settlement.style]
  const buildings = country.settlement.buildings.map((building) => {
    const faces: SceneFace[] = []
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
    if (building.pitched) {
      const left = raised(middle(base[0], base[3]), building.height * 1.6)
      const right = raised(middle(base[1], base[2]), building.height * 1.6)
      faces.push(
        { points: [top[0], top[1], right, left], color: palette[2] },
        { points: [top[2], top[3], left, right], color: '#72452f' },
        { points: [top[1], top[2], right], color: palette[0] },
        { points: [top[3], top[0], left], color: palette[1] }
      )
    } else faces.push({ points: top, color: palette[2] })
    return { center: centerOf(base), faces, height: building.height, kind: 'building' as const }
  })
  const cones = (items: CountryGeometry['landscape']['peaks'], mountain: boolean) =>
    items.map(({ base, height }, index) => {
      const center = centerOf(base),
        top = raised(center, height)
      const colors = mountain
        ? ['#aaa58a', '#8b9275', '#637661', '#5b6a59', '#788568', '#c1baa1']
        : ['#749459', '#3e714c', '#265943', '#315f43', '#567c49']
      const faces: SceneFace[] = base.map((a, i) => ({
        points: [a, base[(i + 1) % base.length], top],
        color: colors[(i + index) % colors.length],
      }))
      if (mountain)
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
  return [...buildings, ...cones(country.landscape.peaks, true), ...cones(country.landscape.trees, false)]
}
