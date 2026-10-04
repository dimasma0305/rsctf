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
