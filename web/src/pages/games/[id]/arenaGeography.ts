import { sphereLocation, type GlobePoint } from './arenaGlobeModel'

interface Point {
  x: number
  y: number
}
interface Category {
  id: string
  challenges: readonly { id: number }[]
}
export interface CountryGeometry {
  id: number
  location: GlobePoint
  coast: GlobePoint[]
  settlement: {
    style: 'modern' | 'town' | 'village'
    buildings: { base: GlobePoint[]; height: number; pitched: boolean }[]
  }
}
export interface ContinentGeometry {
  id: string
  location: GlobePoint
  labelLocation: GlobePoint
  coast: GlobePoint[]
  countries: CountryGeometry[]
}

const TAU = Math.PI * 2
const COAST_SAMPLES = 256
const disk = Array.from({ length: COAST_SAMPLES }, (_, i) => ({
  x: Math.cos((i * TAU) / COAST_SAMPLES),
  y: Math.sin((i * TAU) / COAST_SAMPLES),
}))
const seedOf = (id: string) => {
  let seed = 2166136261
  for (const char of id) seed = Math.imul(seed ^ char.charCodeAt(0), 16777619) >>> 0
  return seed
}
const phase = (seed: number, salt: number) => ((Math.imul(seed ^ salt, 1597334677) >>> 0) / 4294967296) * TAU
const dot = (a: GlobePoint, b: GlobePoint) => a.x * b.x + a.y * b.y + a.z * b.z

/** A convex half-plane clip. Cells tile a disk before one shared coastline warp. */
function clip(points: Point[], a: Point, b: Point) {
  const nx = b.x - a.x,
    ny = b.y - a.y
  const limit = (b.x * b.x + b.y * b.y - a.x * a.x - a.y * a.y) / 2
  const result: Point[] = []
  for (let i = 0; i < points.length; i++) {
    const p = points[i],
      q = points[(i + 1) % points.length]
    const dp = p.x * nx + p.y * ny - limit,
      dq = q.x * nx + q.y * ny - limit
    if (dp <= 0) result.push(p)
    if (dp <= 0 !== dq <= 0) {
      const t = dp / (dp - dq)
      result.push({ x: p.x + (q.x - p.x) * t, y: p.y + (q.y - p.y) * t })
    }
  }
  return result
}
function cellsFor(sites: Point[]) {
  return sites.map((site, i) => {
    let cell = disk
    for (let j = 0; j < sites.length && cell.length; j++) if (j !== i) cell = clip(cell, site, sites[j])
    return cell
  })
}
function centroid(points: Point[]): Point {
  let area = 0,
    x = 0,
    y = 0
  for (let i = 0; i < points.length; i++) {
    const a = points[i],
      b = points[(i + 1) % points.length]
    const cross = a.x * b.y - b.x * a.y
    area += cross
    x += (a.x + b.x) * cross
    y += (a.y + b.y) * cross
  }
  return Math.abs(area) < 1e-12 ? { x: 0, y: 0 } : { x: x / (3 * area), y: y / (3 * area) }
}

/** Sample shared borders identically in either direction before warping them. */
function densify(points: Point[]) {
  return points.flatMap((p, i) => {
    const q = points[(i + 1) % points.length]
    const steps = Math.max(1, Math.ceil(Math.hypot(p.x - q.x, p.y - q.y) / 0.045))
    return Array.from({ length: steps }, (_, step) => ({
      x: p.x + ((q.x - p.x) * step) / steps,
      y: p.y + ((q.y - p.y) * step) / steps,
    }))
  })
}

/** Pure fictional geography, generated only when category/challenge membership changes.
 * Convex cells share borders; the continuous radial warp keeps them adjacent and
 * non-overlapping while producing irregular coastlines and curved inland borders.
 * No random state, team data, score, viewport or animation frame affects geography.
 */
export function buildArenaGeography(categories: readonly Category[]): ContinentGeometry[] {
  const sorted = categories
    .filter((c) => c.challenges.length)
    .toSorted((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0))
  const centers = sorted.map((_, i) => sphereLocation(i, sorted.length, 0.2))
  return sorted.map((category, index) => {
    const center = centers[index],
      seed = seedOf(category.id)
    const nearest = Math.min(
      Math.PI,
      ...centers.filter((_, i) => i !== index).map((p) => Math.acos(Math.max(-1, Math.min(1, dot(center, p)))))
    )
    const radius = Math.min(1.2, nearest * 0.47)
    const length = Math.hypot(center.x, center.z)
    const east = { x: center.z / length, y: 0, z: -center.x / length }
    const north = { x: center.y * east.z, y: length, z: -center.y * east.x }
    const bay = (angle: number, direction: number, width: number) => {
      const delta = Math.atan2(Math.sin(angle - direction), Math.cos(angle - direction))
      return Math.exp(-(delta * delta) / width)
    }
    const coastRadius = (angle: number) =>
      0.72 +
      0.17 * Math.sin(2 * angle + phase(seed, 1)) +
      0.09 * Math.cos(3 * angle + phase(seed, 2)) +
      0.045 * Math.sin(7 * angle + phase(seed, 3)) +
      0.016 * Math.cos(13 * angle + phase(seed, 4)) +
      0.008 * Math.sin(29 * angle + phase(seed, 5)) +
      0.004 * Math.cos(53 * angle + phase(seed, 6)) +
      0.12 * bay(angle, phase(seed, 12), 0.055) -
      0.32 * bay(angle, phase(seed, 10), 0.12) -
      0.2 * bay(angle, phase(seed, 11), 0.04)
    const maximum = Math.max(...disk.map((p) => coastRadius(Math.atan2(p.y, p.x)))) * 1.01
    const rotation = phase(seed, 7),
      squeeze = 0.66 + 0.25 * Math.sin(phase(seed, 8)) ** 2
    const onSphere = (p: Point): GlobePoint => {
      const angle = Math.atan2(p.y, p.x)
      const scale = Math.max(0.13, coastRadius(angle)) / maximum
      // Compose invertible radial, radius-dependent twist, and affine warps.
      // The same transform applies to coast, countries and building foundations.
      const r0 = Math.hypot(p.x, p.y)
      // Positive radial derivative, with coastal detail fading toward the interior.
      const distance = (r0 * scale) / (scale + (1 - scale) * r0)
      const twisted = angle + 0.65 * distance * distance * Math.sin(phase(seed, 13))
      const px = Math.cos(twisted) * distance,
        py = Math.sin(twisted) * distance * squeeze
      const x = px * Math.cos(rotation) - py * Math.sin(rotation)
      const y = px * Math.sin(rotation) + py * Math.cos(rotation)
      const r = Math.hypot(x, y),
        arc = r * radius
      const tangent = r > 1e-12 ? Math.sin(arc) / r : radius
      const axial = Math.cos(arc)
      return {
        x: center.x * axial + (east.x * x + north.x * y) * tangent,
        y: center.y * axial + north.y * y * tangent,
        z: center.z * axial + (east.z * x + north.z * y) * tangent,
      }
    }
    const ids = category.challenges.map((c) => c.id).toSorted((a, b) => a - b)
    let sites = ids.map((_, i) => {
      const a = i * Math.PI * (3 - Math.sqrt(5)) + phase(seed, 9)
      const r = Math.sqrt((i + 0.5) / ids.length) * 0.83
      return { x: Math.cos(a) * r, y: Math.sin(a) * r }
    })
    // Two deterministic relaxation passes avoid tiny, crowded countries at the rim.
    for (let pass = 0; pass < 2; pass++) sites = cellsFor(sites).map(centroid)
    const cells = cellsFor(sites)
    const coast = disk.map(onSphere)
    // Projection uses downward-positive screen Y. Anchor the label above the land.
    const labelLocation = coast.reduce((highest, p) => (dot(p, north) < dot(highest, north) ? p : highest))
    return {
      id: category.id,
      location: center,
      labelLocation,
      coast,
      countries: cells.map((cell, i) => {
        const middle = centroid(cell)
        const clearance = Math.min(
          ...cell.map((a, j) => {
            const b = cell[(j + 1) % cell.length]
            return (
              Math.abs((b.x - a.x) * (middle.y - a.y) - (b.y - a.y) * (middle.x - a.x)) /
              Math.max(1e-12, Math.hypot(b.x - a.x, b.y - a.y))
            )
          })
        )
        const style = (['modern', 'town', 'village'] as const)[seedOf(`${category.id}:${ids[i]}`) % 3]
        const unit = Math.min(0.075, clearance * 0.18)
        const buildings = Array.from({ length: style === 'village' ? 4 : 5 }, (_, b) => {
          const x = middle.x + ((b % 3) - 1) * unit * 1.8
          const y = middle.y + clearance * 0.22 + Math.floor(b / 3) * unit * 1.7
          const size = unit * (style === 'modern' ? 0.55 : 0.68)
          const base = [
            [-1, -1],
            [1, -1],
            [1, 1],
            [-1, 1],
          ].map(([dx, dy]) => onSphere({ x: x + dx * size, y: y + dy * size }))
          const width = Math.hypot(base[0].x - base[1].x, base[0].y - base[1].y, base[0].z - base[1].z)
          return {
            base,
            height: Math.min(0.065, width * (style === 'modern' ? 1.8 + (b % 3) * 0.9 : 0.7)),
            pitched: style !== 'modern',
          }
        })
        return {
          id: ids[i],
          location: onSphere(middle),
          coast: densify(cell).map(onSphere),
          settlement: { style, buildings },
        }
      }),
    }
  })
}
