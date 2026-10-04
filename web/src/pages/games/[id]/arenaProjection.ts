import { projectSpherePoint } from '@Components/competition/model'
import { GLOBE_RADIUS, type GlobePoint } from './arenaGlobeModel'

const point = (p: GlobePoint) => `${(500 + p.x * GLOBE_RADIUS).toFixed(2)},${(500 + p.y * GLOBE_RADIUS).toFixed(2)}`

/** Orthographic surface clipping. The fill follows the spherical limb, while the
 * stroke contains ONLY real coastline/border segments, never the clipping seam.
 * Arena polygons lie in caps smaller than a hemisphere, so limb crossings share
 * a < PI arc; the shorter arc also handles concave bays with multiple crossings.
 */
export function projectSurface(points: readonly GlobePoint[], yaw: number, pitch: number) {
  const rotated = points.map((p) => projectSpherePoint(p.x, p.y, p.z, yaw, pitch))
  const runs: GlobePoint[][] = []
  const firstHidden = rotated.findIndex((p) => p.z < 0)
  if (firstHidden === -1) {
    const outline = rotated.map((p, i) => `${i ? 'L' : 'M'}${point(p)}`).join('') + (points.length ? 'Z' : '')
    return { fill: outline, edge: outline }
  }
  let run: GlobePoint[] | null = null
  for (let j = 0; j < rotated.length; j++) {
    const a = rotated[(firstHidden + j) % rotated.length]
    const b = rotated[(firstHidden + j + 1) % rotated.length]
    if (a.z >= 0 && run) run.push(a)
    if (a.z >= 0 !== b.z >= 0) {
      const t = -a.z / (b.z - a.z)
      const x = a.x + (b.x - a.x) * t,
        y = a.y + (b.y - a.y) * t
      const radius = Math.hypot(x, y)
      const crossing = { x: x / radius, y: y / radius, z: 0 }
      if (b.z >= 0) {
        run = [crossing]
        runs.push(run)
      } else {
        run?.push(crossing)
        run = null
      }
    }
  }
  const edge = runs.map((r) => r.map((p, i) => `${i ? 'L' : 'M'}${point(p)}`).join('')).join('')
  // Join clipped fragments along the actual circular silhouette, not a chord
  // through the ocean. Backtracking limb arcs cancel with SVG's nonzero fill.
  const fill =
    runs
      .map((r, i) => {
        const next = runs[(i + 1) % runs.length][0],
          end = r[r.length - 1]
        const sweep = end.x * next.y - end.y * next.x >= 0 ? 1 : 0
        return (
          r.map((p, j) => `${i === 0 && j === 0 ? 'M' : 'L'}${point(p)}`).join('') +
          `A${GLOBE_RADIUS},${GLOBE_RADIUS} 0 0 ${sweep} ${point(next)}`
        )
      })
      .join('') + (runs.length ? 'Z' : '')
  return { fill, edge }
}
