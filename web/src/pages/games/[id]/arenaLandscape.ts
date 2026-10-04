import type { GlobePoint } from './arenaGlobeModel'

interface Point {
  x: number
  y: number
}
export interface Landscape {
  meadows: GlobePoint[][]
  road: GlobePoint[]
  river: GlobePoint[]
  peaks: { base: GlobePoint[]; height: number }[]
  trees: { base: GlobePoint[]; height: number }[]
}

/** All details live in the cell's inscribed disk, then use the SAME spherical
 * warp as its borders. No geographic/cultural inference from participant data.
 */
export function buildLandscape(
  center: Point,
  clearance: number,
  seed: number,
  onSphere: (p: Point) => GlobePoint
): Landscape {
  const random = (salt: number) => (Math.imul(seed ^ salt, 1597334677) >>> 0) / 4294967296
  const point = (x: number, y: number) => onSphere({ x: center.x + x * clearance, y: center.y + y * clearance })
  const ring = (x: number, y: number, radius: number, count: number, irregular = false) =>
    Array.from({ length: count }, (_, i) => {
      const a = (i * Math.PI * 2) / count
      const r = radius * (irregular ? 0.8 + random(i + 431) * 0.2 : 1)
      return point(x + Math.cos(a) * r, y + Math.sin(a) * r)
    })
  const width = (base: GlobePoint[]) => Math.hypot(base[0].x - base[1].x, base[0].y - base[1].y, base[0].z - base[1].z)
  const peaks = Array.from({ length: 3 }, (_, i) => {
    const base = ring(-0.36 + i * 0.25, -0.39 + Math.sin(i * 2 + random(2)) * 0.1, 0.17, 6)
    return { base, height: Math.min(0.085, width(base) * (1.8 + random(i + 33))) }
  })
  const trees = Array.from({ length: 14 }, (_, i) => {
    const a = i * Math.PI * (3 - Math.sqrt(5))
    const r = Math.sqrt((i + 0.5) / 14) * 0.23
    const base = ring(0.43 + Math.cos(a) * r, 0.08 + Math.sin(a) * r, 0.035 + random(i + 62) * 0.014, 5)
    return { base, height: Math.min(0.038, width(base) * (2.8 + random(i + 70))) }
  })
  return {
    meadows: [ring(0.42, 0.08, 0.31, 18, true), ring(-0.42, 0.04, 0.25, 18, true)],
    road: Array.from({ length: 18 }, (_, i) => {
      const t = i / 17
      return point(-0.48 + t * 0.84, 0.29 + Math.sin(t * Math.PI * 2) * 0.07)
    }),
    river: Array.from({ length: 24 }, (_, i) => {
      const t = i / 23
      return point(-0.16 - t * 0.33 + Math.sin(t * Math.PI * 3) * 0.06, -0.27 + t * 0.83)
    }),
    peaks,
    trees,
  }
}
