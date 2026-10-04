import type { GlobePoint } from './arenaGlobeModel'
import type { GroundPatch } from './arenaLandscape'

interface Point {
  x: number
  y: number
}

/** Broad landcover follows the full cell, not just the settlement's safe disk.
 * Clip in the convex source plane, then apply the same warp as every border.
 */
export function buildTerrainCover(
  cell: Point[],
  center: Point,
  seed: number,
  onSphere: (p: Point) => GlobePoint
): GroundPatch[] {
  // Keep enough inset for the rendered great-circle approximation at narrow,
  // highly warped coastal cells, not merely for the unwarped polygon.
  const boundary = cell.map((p) => ({ x: center.x + (p.x - center.x) * 0.78, y: center.y + (p.y - center.y) * 0.78 }))
  const width = Math.max(...cell.map((p) => p.x)) - Math.min(...cell.map((p) => p.x))
  const height = Math.max(...cell.map((p) => p.y)) - Math.min(...cell.map((p) => p.y))
  return ['#4168464d', '#c0b57454', '#7b90624d', '#c7c29438'].map((color, index) => {
    const angle = index * 2.4 + (seed % 19)
    let points = Array.from({ length: 36 }, (_, i) => {
      const a = (i * Math.PI) / 18
      const r = 1 + 0.13 * Math.sin(a * 3 + angle) + 0.06 * Math.cos(a * 7 - angle)
      return {
        x: center.x + width * (Math.cos(angle) * 0.21 + Math.cos(a) * 0.38 * r),
        y: center.y + height * (Math.sin(angle) * 0.2 + Math.sin(a) * 0.32 * r),
      }
    })
    for (let i = 0; i < boundary.length && points.length; i++) {
      const a = boundary[i],
        b = boundary[(i + 1) % boundary.length]
      const distance = (p: Point) => (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
      const result: Point[] = []
      for (let j = 0; j < points.length; j++) {
        const p = points[j],
          q = points[(j + 1) % points.length],
          dp = distance(p),
          dq = distance(q)
        if (dp >= 0) result.push(p)
        if (dp >= 0 !== dq >= 0) {
          const t = dp / (dp - dq)
          result.push({ x: p.x + (q.x - p.x) * t, y: p.y + (q.y - p.y) * t })
        }
      }
      points = result
    }
    return { points: points.map(onSphere), color }
  })
}
