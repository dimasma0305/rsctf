import type { GlobePoint } from './arenaGlobeModel'

type V = GlobePoint
const TAU = 2 * Math.PI
const rows = 36,
  columns = 72
const G = 0.38,
  h = 1 / 90
const core = 0.43
export const MAX_EJECTA = 112
export const dot = (a: V, b: V) => a.x * b.x + a.y * b.y + a.z * b.z
export const times = (a: V, k: number): V => ({ x: a.x * k, y: a.y * k, z: a.z * k })
export const plus = (a: V, b: V): V => ({ x: a.x + b.x, y: a.y + b.y, z: a.z + b.z })
export const normal = (a: V): V => times(a, 1 / (Math.hypot(a.x, a.y, a.z) || 1))
const cross = (a: V, b: V): V => ({ x: a.y * b.z - a.z * b.y, y: a.z * b.x - a.x * b.z, z: a.x * b.y - a.y * b.x })
const noise = (seed: number) => {
  const k = Math.sin(seed * 78.33 + 1.9) * 43758.5453
  return k - Math.floor(k)
}
const vertex = (row: number, col: number) =>
  row === 0 ? 0 : row === rows ? 1 + (rows - 1) * columns : 1 + (row - 1) * columns + ((col + columns) % columns)
const directions: V[] = [],
  weights: number[] = [],
  faces: number[][] = []
for (let r = 0; r <= rows; r++) {
  const lat = Math.PI * (r / rows - 0.5),
    count = r === 0 || r === rows ? 1 : columns
  const area =
    (Math.sin(Math.min(Math.PI / 2, lat + Math.PI / (2 * rows))) -
      Math.sin(Math.max(-Math.PI / 2, lat - Math.PI / (2 * rows)))) /
    (2 * count)
  for (let c = 0; c < count; c++) {
    const lon = (TAU * c) / columns
    directions.push({ x: Math.sin(lon) * Math.cos(lat), y: Math.sin(lat), z: Math.cos(lon) * Math.cos(lat) })
    weights.push(area)
  }
}
for (let r = 0; r < rows; r++)
  for (let c = 0; c < columns; c++) {
    const a = vertex(r, c),
      b = vertex(r, c + 1),
      d = vertex(r + 1, c),
      e = vertex(r + 1, c + 1)
    if (a !== b) faces.push([a, b, d])
    if (d !== e) faces.push([b, e, d])
  }
function grid(n: V) {
  const col = ((Math.atan2(n.x, n.z) / TAU + 1) % 1) * columns
  const row = (Math.asin(Math.max(-1, Math.min(1, n.y))) / Math.PI + 0.5) * rows
  return {
    row: Math.min(rows - 1, Math.floor(row)),
    col: Math.floor(col),
    v: row - Math.min(rows - 1, Math.floor(row)),
    u: col - Math.floor(col),
  }
}
export interface Ejecta {
  p: V
  v: V
  mass: number
  age: number
  trail: V[]
  seed: number
}
export function createImpactPlanet() {
  const radius = new Float64Array(directions.length).fill(1),
    heat = new Float32Array(directions.length)
  const active: Ejecta[] = []
  const waves: { at: V; age: number; reach: number }[] = []
  let mass = 1,
    escaped = 0,
    hits = 0,
    thermal = 0,
    shockEnergy = 0,
    substep = 0,
    remainder = 0,
    shapeRevision = 0,
    revision = 0
  function surface(n: V) {
    const { row, col, u, v } = grid(n)
    const line = (r: number) => radius[vertex(r, col)] * (1 - u) + radius[vertex(r, col + 1)] * u
    return line(row) * (1 - v) + line(row + 1) * v
  }
  function deposit(n: V, amount: number, kinetic = 0) {
    // Fallback fills depressions and spreads into a wider ejecta blanket once
    // the local ground is full. Limit the rim height without discarding mass.
    let remaining = amount
    for (const reach of [0.025 + Math.min(0.14, amount * 12), 0.26, 0.8, 2.01]) {
      const sites: number[] = [],
        capacity: number[] = []
      let total = 0
      for (let i = 0; i < directions.length; i++) {
        const kernel = reach > 2 ? 1 : Math.max(0, 1 - (1 - dot(n, directions[i])) / reach)
        const room = kernel * Math.max(0, 1.045 ** 3 - radius[i] ** 3)
        if (room > 0) {
          sites.push(i)
          capacity.push(room)
          total += weights[i] * room
        }
      }
      if (!total) continue
      const fraction = Math.min(1, remaining / total)
      for (const [j, i] of sites.entries()) {
        const fill = capacity[j] * fraction
        radius[i] = Math.cbrt(radius[i] ** 3 + fill)
        heat[i] = Math.min(1.35, heat[i] + (kinetic * fill) / amount)
        thermal = Math.max(thermal, heat[i])
      }
      remaining -= total * fraction
      if (remaining < 1e-12) break
    }
    mass += amount
    shapeRevision++
    revision++
  }
  function launch(p: Ejecta) {
    if (active.length < MAX_EJECTA) {
      active.push(p)
      return
    }
    // Coalesce nearby parcels under a burst, conserving mass and linear momentum.
    let best = 0,
      dist = Infinity
    for (let i = 0; i < active.length; i++) {
      const a = active[i].p,
        d = (a.x - p.p.x) ** 2 + (a.y - p.p.y) ** 2 + (a.z - p.p.z) ** 2
      if (d < dist) {
        dist = d
        best = i
      }
    }
    merge(active[best], p)
  }
  function merge(a: Ejecta, b: Ejecta) {
    const m = a.mass + b.mass
    a.p = times(plus(times(a.p, a.mass), times(b.p, b.mass)), 1 / m)
    a.v = times(plus(times(a.v, a.mass), times(b.v, b.mass)), 1 / m)
    a.mass = m
    a.trail = [a.p]
  }
  function mutualGravity(dt: number) {
    for (let i = 0; i < active.length; i++)
      for (let j = active.length - 1; j > i; j--) {
        const a = active[i],
          b = active[j],
          x = b.p.x - a.p.x,
          y = b.p.y - a.p.y,
          z = b.p.z - a.p.z
        const d2 = x * x + y * y + z * z,
          d = Math.sqrt(d2 + 0.0009)
        if (a.age > 0.25 && b.age > 0.25 && d < 0.5 * (Math.cbrt(a.mass) + Math.cbrt(b.mass))) {
          const relative = (a.v.x - b.v.x) ** 2 + (a.v.y - b.v.y) ** 2 + (a.v.z - b.v.z) ** 2
          if (relative < (2 * G * (a.mass + b.mass)) / d) {
            merge(a, b)
            active.splice(j, 1)
            continue
          }
        }
        const force = (G * dt) / (d * d * d)
        a.v.x += x * force * b.mass
        a.v.y += y * force * b.mass
        a.v.z += z * force * b.mass
        b.v.x -= x * force * a.mass
        b.v.y -= y * force * a.mass
        b.v.z -= z * force * a.mass
      }
  }
  function impact(n: V, source: V, count = 1, motion = true) {
    hits += count
    const tangent = plus(times(n, dot(source, n)), times(source, -1))
    const along = normal(
      dot(tangent, tangent) > 1e-8
        ? tangent
        : cross(n, Math.abs(n.y) < 0.9 ? { x: 0, y: 1, z: 0 } : { x: 1, y: 0, z: 0 })
    )
    const across = cross(n, along)
    const basin = 1 - Math.min(1, surface(n))
    // Capture events supply a bearing, but no real mass or speed. Assign
    // dimensionless impactors and compare accumulated shock to binding energy.
    const incidence = Math.sqrt(Math.max(0, (1 - dot(n, source)) / 2))
    const energy = 0.5 * 0.0035 * count * (1.22 + 0.37 * incidence) ** 2
    shockEnergy += 0.48 * energy
    const binding = 0.6 * G * mass ** (5 / 3)
    const disruption = Math.min(1.5, Math.max(0, shockEnergy / binding - 0.12))
    const reach = Math.min(1.9, 0.32 + 0.34 * Math.sqrt(basin) + 0.15 * Math.log1p(count - 1) + disruption * 0.85)
    const angleLimit = 1 - Math.cos(reach)
    if (motion) {
      if (waves.length >= 12) waves.shift()
      waves.push({ at: n, age: 0, reach })
    }
    let removed = 0
    for (let i = 0; i < directions.length; i++) {
      const q = (1 - dot(n, directions[i])) / angleLimit
      if (q >= 1) continue
      const shape = (1 - q) ** 1.45
      const old = radius[i],
        next = Math.max(core, old - 0.13 * Math.sqrt(count) * shape * (0.3 + 0.7 * old ** 2))
      removed += weights[i] * (old ** 3 - next ** 3)
      radius[i] = next
      heat[i] = Math.min(1.35, heat[i] + (0.48 + 0.16 * Math.log(count)) * shape)
    }
    mass -= removed
    shapeRevision++
    thermal = Math.max(thermal, 1.6)
    revision++
    if (removed <= 1e-10) return
    const distance = surface(n)
    const escape = Math.sqrt((2 * G * mass) / distance)
    const parcels = Math.min(18, 9 + Math.floor(Math.log2(count)))
    // Distribute the kinetic share of this impact over the excavated mass.
    const launchWeight = (i: number) => 0.26 + (0.96 * (i + 0.5)) / parcels
    const meanSquare = Array.from({ length: parcels }, (_, i) => launchWeight(i) ** 2).reduce((a, b) => a + b) / parcels
    const speedScale = Math.min(1.8, Math.sqrt((0.6 * energy) / (removed * meanSquare)))
    for (let j = 0; j < parcels; j++) {
      const seed = hits * 67 + j,
        angle = j * 2.39996 + noise(seed) * 0.55
      const side = normal(plus(times(along, 0.35 + Math.cos(angle)), times(across, Math.sin(angle))))
      const at = normal(plus(n, times(side, reach * 0.58 * Math.sqrt(noise(seed + 61)))))
      const position = times(at, surface(at) + 0.015)
      // Launch velocity is independent of escape velocity: as the remnant
      // loses mass, the same impulse sends a greater fraction into space.
      const speed = launchWeight(j) * speedScale
      const direction = normal(
        plus(times(at, 0.25 + 0.7 * noise(seed + 41)), times(side, 0.65 + 0.55 * noise(seed + 73)))
      )
      const parcel = removed / parcels
      if (motion) launch({ p: position, trail: [position], v: times(direction, speed), mass: parcel, age: 0, seed })
      else if (speed >= escape) escaped += parcel
      else deposit(at, parcel)
    }
  }
  function step(dt: number) {
    const decay = Math.exp(-dt * 0.42)
    if (++substep % 3 === 0) mutualGravity(dt * 3)
    for (let i = waves.length - 1; i >= 0; i--) {
      waves[i].age += dt
      if (waves[i].age > 0.85) waves.splice(i, 1)
    }
    if (thermal > 0.006) {
      thermal *= decay
      for (let i = 0; i < heat.length; i++) heat[i] *= decay / (1 + dt * 0.48 * heat[i] ** 3)
      revision++
    }
    for (let i = active.length - 1; i >= 0; i--) {
      const b = active[i],
        r = Math.hypot(b.p.x, b.p.y, b.p.z)
      if (Math.floor(b.age * 22) !== Math.floor((b.age + dt) * 22)) {
        b.trail.push(b.p)
        if (b.trail.length > 9) b.trail.shift()
      }
      // Velocity Verlet integration in the gravitational field of the remnant.
      const half = plus(b.v, times(b.p, (-0.5 * G * mass * dt) / r ** 3))
      b.p = plus(b.p, times(half, dt))
      const length = Math.hypot(b.p.x, b.p.y, b.p.z),
        n = times(b.p, 1 / length)
      b.v = plus(half, times(b.p, (-0.5 * G * mass * dt) / length ** 3))
      b.age += dt
      if (length <= surface(n) && dot(b.v, n) < 0) {
        deposit(n, b.mass, 0.5 * b.mass * dot(b.v, b.v))
        active.splice(i, 1)
      } else if (length > 2.5 && dot(b.v, n) > 0 && dot(b.v, b.v) / 2 > (G * mass) / length) {
        escaped += b.mass
        active.splice(i, 1)
      }
    }
  }
  function advance(dt: number, motion: boolean) {
    shockEnergy *= Math.exp(-Math.max(0, Math.min(dt, 0.1)) / 5)
    if (!motion) {
      if (!active.length && !thermal) return false
      for (const b of active) {
        const r = Math.hypot(b.p.x, b.p.y, b.p.z)
        if (dot(b.v, b.v) / 2 >= (G * mass) / r) escaped += b.mass
        else deposit(normal(b.p), b.mass)
      }
      const changed = active.length > 0 || thermal > 0
      active.length = waves.length = 0
      thermal = 0
      heat.fill(0)
      if (changed) revision++
      return changed
    }
    if (!active.length && thermal <= 0.006) return false
    remainder += Math.max(0, Math.min(dt, 0.1))
    const steps = Math.floor((remainder + 1e-12) / h)
    remainder -= steps * h
    for (let i = 0; i < steps; i++) step(h)
    return steps > 0
  }
  function reset() {
    radius.fill(1)
    heat.fill(0)
    active.length = waves.length = 0
    mass = 1
    shapeRevision++
    escaped = hits = thermal = remainder = shockEnergy = substep = 0
    revision++
  }
  return {
    radius,
    heat,
    directions,
    faces,
    active,
    waves,
    impact,
    advance,
    surface,
    reset,
    get mass() {
      return mass
    },
    get escaped() {
      return escaped
    },
    get hits() {
      return hits
    },
    get revision() {
      return revision
    },
    get shapeRevision() {
      return shapeRevision
    },
  }
}
