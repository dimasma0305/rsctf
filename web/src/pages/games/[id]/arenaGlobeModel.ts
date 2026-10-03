import { projectSpherePoint } from '@Components/competition/model'

export interface GlobePoint {
  x: number
  y: number
  z: number
}
export const GLOBE_RADIUS = 435

/** Evenly spaced fictional locations. Never represents a participant's location. */
export function sphereLocation(index: number, count: number, phase = 0): GlobePoint {
  const y = 0.84 - (1.68 * (index + 0.5)) / Math.max(1, count)
  const angle = index * Math.PI * (3 - Math.sqrt(5)) + phase
  const r = Math.sqrt(1 - y * y)
  return { x: Math.sin(angle) * r, y, z: Math.cos(angle) * r }
}

export function projectGlobe(point: GlobePoint, yaw: number, pitch: number, elevation = 1) {
  const p = projectSpherePoint(point.x, point.y, point.z, yaw, pitch)
  return {
    x: 500 + p.x * GLOBE_RADIUS * elevation,
    y: 500 + p.y * GLOBE_RADIUS * elevation,
    z: p.z,
    visible: p.z > 0.035,
  }
}

export const faceLocation = (p: GlobePoint) => ({ yaw: -Math.atan2(p.x, p.z), pitch: Math.asin(p.y) })

/** A deterministic raised island on the sphere, not a screen-space polygon. */
export function islandCoast(center: GlobePoint, index: number, count: number): GlobePoint[] {
  const radius = Math.min(0.3, 0.95 / Math.sqrt(Math.max(1, count)))
  const length = Math.hypot(center.x, center.z)
  const east = { x: center.z / length, y: 0, z: -center.x / length }
  const north = { x: center.y * east.z, y: center.z * east.x - center.x * east.z, z: -center.y * east.x }
  return Array.from({ length: 16 }, (_, i) => {
    const a = (i / 16) * Math.PI * 2
    const r = radius * (0.78 + 0.16 * Math.sin(i * 2.7 + index * 1.8) + 0.06 * Math.cos(i * 5.1))
    const x = center.x + r * (east.x * Math.cos(a) + north.x * Math.sin(a))
    const y = center.y + r * north.y * Math.sin(a)
    const z = center.z + r * (east.z * Math.cos(a) + north.z * Math.sin(a))
    const n = Math.hypot(x, y, z)
    return { x: x / n, y: y / n, z: z / n }
  })
}

export interface PublicSolver {
  name: string
  color: string
}
export function acceptedTerritorySolvers(
  challengeId: number,
  teams: readonly { name: string; solvedChallenges?: readonly { id: number; type: string; time: number }[] }[],
  bloods: readonly { name: string }[],
  colorOf: (name: string) => string
): PublicSolver[] {
  const solves = teams
    .flatMap((team) =>
      (team.solvedChallenges || [])
        .filter(
          (solve) =>
            solve.id === challengeId && ['FirstBlood', 'SecondBlood', 'ThirdBlood', 'Normal'].includes(solve.type)
        )
        .map((solve) => ({ name: team.name, time: solve.time }))
    )
    .sort((a, b) => a.time - b.time || a.name.localeCompare(b.name))
  // Bloods are the authoritative first three; the public board supplies other solvers.
  return [...new Set([...bloods.map((b) => b.name), ...solves.map((s) => s.name)])].map((name) => ({
    name,
    color: colorOf(name),
  }))
}
