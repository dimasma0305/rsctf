import type { GlobePoint } from './arenaGlobeModel'

interface Point {
  x: number
  y: number
}
export interface GroundPatch {
  points: GlobePoint[]
  color: string
}
export interface Landscape {
  groundColor: string
  patches: GroundPatch[]
  fields: GroundPatch[]
  road: GlobePoint[]
  lanes: GlobePoint[][]
  river: GlobePoint[]
  tributary: GlobePoint[]
  lake: GlobePoint[]
  bridge: GlobePoint[]
  peaks: { base: GlobePoint[]; height: number }[]
  trees: { base: GlobePoint[]; height: number }[]
}
export interface Settlement {
  style: 'modern' | 'town' | 'village'
  buildings: { base: GlobePoint[]; height: number; pitched: boolean; landmark: boolean }[]
}

/** One seeded plan coordinates the valley, streets, bridge and building plots.
 * Every foundation uses the borders' spherical warp and stays in the inscribed
 * disk. Architecture is fictional, never inferred from participant identities.
 */
export function buildCountryDetails(
  center: Point,
  clearance: number,
  seed: number,
  onSphere: (p: Point) => GlobePoint
): { landscape: Landscape; settlement: Settlement } {
  const random = (salt: number) => (Math.imul(seed ^ salt, 1597334677) >>> 0) / 4294967296
  const angle = (random(81) - 0.5) * Math.PI * 1.5
  const point = (x: number, y: number) =>
    onSphere({
      x: center.x + (x * Math.cos(angle) - y * Math.sin(angle)) * clearance,
      y: center.y + (x * Math.sin(angle) + y * Math.cos(angle)) * clearance,
    })
  const ring = (x: number, y: number, rx: number, ry: number, count: number, salt = 0) =>
    Array.from({ length: count }, (_, i) => {
      const a = (i * Math.PI * 2) / count
      const r = salt ? 0.85 + 0.1 * Math.sin(a * 3 + random(salt) * 6) + 0.05 * Math.cos(a * 7) : 1
      return point(x + Math.cos(a) * rx * r, y + Math.sin(a) * ry * r)
    })
  const rect = (x: number, y: number, w: number, h: number) => [
    point(x - w, y - h),
    point(x + w, y - h),
    point(x + w, y + h),
    point(x - w, y + h),
  ]
  const width = (base: GlobePoint[]) => Math.hypot(base[0].x - base[1].x, base[0].y - base[1].y, base[0].z - base[1].z)
  const style = (['modern', 'town', 'village'] as const)[seed % 3]
  const groundColor = ['#789267', '#8c9b71', '#a6a078', '#7e997a'][seed % 4]
  const patches: GroundPatch[] = [
    { points: ring(0, 0, 0.94, 0.91, 36, 11), color: '#71855445' },
    { points: ring(-0.05, -0.32, 0.72, 0.42, 32, 21), color: '#53664d50' },
    { points: ring(0.42, -0.12, 0.32, 0.36, 28, 31), color: '#345f405e' },
    { points: ring(-0.42, 0.31, 0.31, 0.39, 28, 41), color: '#c4bd8054' },
    { points: ring(0.14, 0.3, 0.37, 0.27, 24, 51), color: '#c5ba9570' },
  ]
  const peaks = Array.from({ length: 5 }, (_, i) => {
    const x = -0.48 + i * 0.22,
      y = -0.43 + Math.sin(i * 1.4 + random(22)) * 0.09
    patches.push({ points: ring(x, y, 0.23, 0.19, 18, i + 91), color: '#a4aa815c' })
    const base = ring(x, y, 0.15 + random(i + 33) * 0.035, 0.14, 7, i + 121)
    return { base, height: Math.min(0.075, width(base) * (1.5 + random(i + 39))) }
  })
  const trees = Array.from({ length: 18 }, (_, i) => {
    const a = i * Math.PI * (3 - Math.sqrt(5))
    const r = Math.sqrt((i + 0.5) / 18)
    const base = ring(0.48 + Math.cos(a) * r * 0.18, -0.12 + Math.sin(a) * r * 0.19, 0.038, 0.038, 5)
    return { base, height: Math.min(0.036, width(base) * (2.4 + random(i + 70))) }
  })
  const riverX = (y: number) => -0.28 + Math.sin((y + 0.36) * 7) * 0.085
  const river = Array.from({ length: 32 }, (_, i) => {
    const y = -0.4 + (i / 31) * 1.08
    return point(riverX(y), y)
  })
  const roadY = 0.29
  const buildings = Array.from({ length: style === 'village' ? 6 : 8 }, (_, i) => {
    const x = -0.02 + (i % 4) * 0.145,
      y = roadY + (i < 4 ? -0.105 : 0.105)
    const base = rect(x, y, style === 'modern' ? 0.046 : 0.056, 0.058)
    const landmark = i === 2
    return {
      base,
      height: Math.min(
        style === 'modern' ? 0.075 : 0.06,
        width(base) * (style === 'modern' ? 1.6 + random(i + 251) * 1.8 : landmark ? 1.6 : 0.65)
      ),
      pitched: style !== 'modern',
      landmark,
    }
  })
  const fields = Array.from({ length: 6 }, (_, i) => ({
    points: rect(-0.61 + (i % 2) * 0.125, 0.15 + Math.floor(i / 2) * 0.13, 0.052, 0.052),
    color: ['#b8b772', '#c6ab69', '#91a267'][i % 3],
  }))
  const lanes = [0.0525, 0.1975, 0.3425].map((x) => [point(x, 0.06), point(x, 0.51)])
  lanes.push([point(-0.58, 0.52), point(-0.58, roadY), point(-0.4, roadY)])
  return {
    settlement: { style, buildings },
    landscape: {
      groundColor,
      patches,
      fields,
      road: Array.from({ length: 24 }, (_, i) => point(-0.64 + (i / 23) * 1.2, roadY)),
      lanes,
      river,
      tributary: Array.from({ length: 14 }, (_, i) => {
        const t = i / 13
        return point(0.08 * (1 - t) + riverX(-0.08) * t, -0.39 + t * 0.31 + Math.sin(t * Math.PI) * 0.04)
      }),
      lake: ring(riverX(0.63), 0.63, 0.15, 0.15, 28, 151),
      bridge: rect(riverX(roadY), roadY, 0.09, 0.028),
      peaks,
      trees,
    },
  }
}
