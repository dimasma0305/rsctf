import type { CountryGeometry } from './arenaGeography'
import { GLOBE_RADIUS, type GlobePoint } from './arenaGlobeModel'
import { paintArenaGround } from './arenaGround'
import { buildCountryScene, raised, type SceneObject } from './arenaSceneModel'

interface ScenicCountry extends CountryGeometry {
  solvers?: { color: string }[]
}
export const MAX_SCENIC_COUNTRIES = 24
export const MAX_SCENIC_OBJECTS = MAX_SCENIC_COUNTRIES * 31

/** One bounded bitmap, static cached meshes, and the existing camera's clock.
 * This decorative layer never handles input, fetches data, or owns an animator.
 */
export function createArenaSettlements(canvas: HTMLCanvasElement) {
  const scenes = new WeakMap<CountryGeometry, SceneObject[]>()
  return (countries: readonly ScenicCountry[], yaw: number, pitch: number, width: number, selected: number | null) => {
    const cy = Math.cos(yaw),
      sy = Math.sin(yaw),
      cp = Math.cos(pitch),
      sp = Math.sin(pitch)
    const project = (p: GlobePoint) => {
      const z = p.z * cy - p.x * sy
      return {
        x: 500 + (p.x * cy + p.z * sy) * GLOBE_RADIUS,
        y: 500 + (p.y * cp - z * sp) * GLOBE_RADIUS,
        z: p.y * sp + z * cp,
      }
    }
    const visible = countries
      .map((country) => ({ country, p: project(country.location) }))
      .filter(({ country, p }) => p.z > 0.12 && country.settlement.buildings[0]?.height * width > 1.8)
      .sort((a, b) => Number(b.country.id === selected) - Number(a.country.id === selected) || b.p.z - a.p.z)
      .slice(0, MAX_SCENIC_COUNTRIES)
      .sort((a, b) => a.p.z - b.p.z)
    canvas.dataset.visibleSettlements = String(visible.length)
    const bitmap = Math.max(400, Math.min(1200, Math.ceil(width * Math.max(1.25, window.devicePixelRatio || 1))))
    if (canvas.width !== bitmap) canvas.width = bitmap
    if (canvas.height !== bitmap) canvas.height = bitmap
    const ctx = canvas.getContext('2d')
    if (!ctx) return
    ctx.setTransform(bitmap / 1000, 0, 0, bitmap / 1000, 0, 0)
    ctx.clearRect(0, 0, 1000, 1000)
    ctx.lineCap = ctx.lineJoin = 'round'
    const trace = (points: GlobePoint[], close = true) => {
      ctx.beginPath()
      let connected = false
      for (const p of points) {
        if (p.z < 0) {
          connected = false
          continue
        }
        if (connected) ctx.lineTo(p.x, p.y)
        else ctx.moveTo(p.x, p.y)
        connected = true
      }
      if (close) ctx.closePath()
    }
    const objects: { object: SceneObject; z: number; alpha: number }[] = []
    let peaks = 0,
      trees = 0
    for (const { country, p } of visible) {
      const alpha = Math.min(1, (p.z - 0.12) / 0.2)
      ctx.globalAlpha = alpha
      const scale = Math.min(1, country.settlement.buildings[0].height / 0.035)
      paintArenaGround(ctx, country.landscape, project, scale, country.settlement.buildings[0].height * width > 8)
      let scene = scenes.get(country)
      if (!scene) {
        scene = buildCountryScene(country)
        scenes.set(country, scene)
      }
      for (const object of scene) {
        const z = project(object.center).z
        // Sub-two-pixel relief adds raster work but no readable detail. Keep its
        // ground patch and roads, with full meshes returning at larger sizes.
        if (z <= 0.05 || object.height * width * 0.435 < 2) continue
        objects.push({ object, z, alpha })
        if (object.kind === 'mountain') peaks++
        if (object.kind === 'tree') trees++
      }
    }
    // Depth-sort ALL objects, not separate building/tree material layers.
    objects.sort((a, b) => a.z - b.z)
    // Rotate the light into world space once. Normals also reject back faces
    // before allocating/projecting all their vertices (orthographic camera).
    const lightX = -0.4 * cy - 0.55 * sp * sy - 0.7 * cp * sy,
      lightY = -0.55 * cp + 0.7 * sp,
      lightZ = -0.4 * sy + 0.55 * sp * cy + 0.7 * cp * cy
    for (const { object, alpha } of objects) {
      ctx.globalAlpha = alpha
      for (const face of object.faces) {
        if (face.window && object.height * width * 0.435 < 12) continue
        const n = face.normal
        if (n.y * sp + (n.z * cy - n.x * sy) * cp <= 0) continue
        const p = face.points.map(project)
        if (p.some((v) => v.z < 0)) continue
        trace(p)
        const light = Math.max(0, n.x * lightX + n.y * lightY + n.z * lightZ)
        ctx.fillStyle = face.shades[Math.min(7, Math.floor(light * 8))]
        ctx.fill()
      }
    }
    // A flag marks a real accepted solve; the gold ring marks only selection.
    for (const { country, p } of visible) {
      if (country.id !== selected && !country.solvers?.length) continue
      ctx.globalAlpha = Math.min(1, (p.z - 0.12) / 0.2)
      const base = project(country.location),
        top = project(raised(country.location, 0.1))
      // At the front of an orthographic globe, a radial pole points at the camera.
      // Keep a small upright pennant legible without claiming a physical location.
      if (country.solvers?.length) {
        top.y -= 12
        ctx.strokeStyle = '#f4e3ac'
        ctx.lineWidth = 1.6
        trace([base, top], false)
        ctx.stroke()
        ctx.fillStyle = country.solvers[0].color
        ctx.beginPath()
        ctx.moveTo(top.x, top.y)
        ctx.lineTo(top.x + 12, top.y + 4)
        ctx.lineTo(top.x, top.y + 9)
        ctx.closePath()
        ctx.fill()
      }
      if (country.id === selected) {
        ctx.strokeStyle = '#ffe3a0'
        ctx.lineWidth = 2
        ctx.beginPath()
        ctx.arc(base.x, base.y, 7, 0, Math.PI * 2)
        ctx.stroke()
      }
    }
    ctx.globalAlpha = 1
    canvas.dataset.visiblePeaks = String(peaks)
    canvas.dataset.visibleTrees = String(trees)
    canvas.dataset.visibleObjects = String(objects.length)
  }
}
