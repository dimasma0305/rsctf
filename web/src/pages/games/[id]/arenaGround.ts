import type { GlobePoint } from './arenaGlobeModel'
import type { Landscape } from './arenaLandscape'

/** Ground-level detail shares the exact projection and clipping of the scene. */
export function paintArenaGround(
  ctx: CanvasRenderingContext2D,
  land: Landscape,
  project: (p: GlobePoint) => GlobePoint,
  scale: number,
  detailed: boolean
) {
  const line = (points: GlobePoint[], color: string, width: number) => {
    ctx.beginPath()
    let connected = false
    for (const v of points) {
      const p = project(v)
      if (p.z < 0) {
        connected = false
        continue
      }
      if (connected) ctx.lineTo(p.x, p.y)
      else ctx.moveTo(p.x, p.y)
      connected = true
    }
    ctx.strokeStyle = color
    ctx.lineWidth = width * scale
    ctx.stroke()
  }
  const fill = (points: GlobePoint[], color: string, stroke?: string) => {
    const p = points.map(project)
    if (p.some((v) => v.z < 0)) return
    ctx.beginPath()
    p.forEach((v, i) => (i ? ctx.lineTo(v.x, v.y) : ctx.moveTo(v.x, v.y)))
    ctx.closePath()
    ctx.fillStyle = color
    ctx.fill()
    if (stroke) {
      ctx.strokeStyle = stroke
      ctx.lineWidth = 1.2 * scale
      ctx.stroke()
    }
  }
  for (const patch of land.patches) fill(patch.points, patch.color)
  if (detailed)
    for (const field of land.fields) {
      fill(field.points, field.color, '#6c795b')
      for (const t of [0.25, 0.5, 0.75]) {
        const mix = (a: GlobePoint, b: GlobePoint) => {
          const x = a.x * (1 - t) + b.x * t,
            y = a.y * (1 - t) + b.y * t,
            z = a.z * (1 - t) + b.z * t
          const n = Math.hypot(x, y, z)
          return { x: x / n, y: y / n, z: z / n }
        }
        line([mix(field.points[0], field.points[3]), mix(field.points[1], field.points[2])], '#6e805970', 0.8)
      }
    }
  fill(land.lake, '#4f959e', '#b9c49a')
  for (const path of [land.river, land.tributary]) {
    line(path, '#c1c59b', path === land.river ? 5 : 3)
    line(path, '#4c939d', path === land.river ? 3 : 1.5)
    if (detailed) line(path, '#b1dad1', 0.6)
  }
  for (const path of [land.road, ...(detailed ? land.lanes : [])]) {
    line(path, '#69694f', 4)
    line(path, '#d5c89f', 2.3)
  }
  fill(land.bridge, '#d4c7aa', '#605b4b')
  if (detailed) {
    line([land.bridge[0], land.bridge[1]], '#f3dfac', 1.4)
    line([land.bridge[2], land.bridge[3]], '#f3dfac', 1.4)
  }
}
