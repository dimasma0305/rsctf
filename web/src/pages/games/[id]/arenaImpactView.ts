import { GLOBE_RADIUS as R, type GlobePoint } from './arenaGlobeModel'
import { createImpactPlanet, plus, times } from './arenaImpactPhysics'
import { createImpactSurface } from './arenaImpactSurface'

// Clip the original map to the surviving terrain and composite the exposed ground and ejecta above it.
export function createImpactView(root: ShadowRoot) {
  const planet = createImpactPlanet()
  const shell = root.getElementById('arenaSurface')!
  let terrain: ReturnType<typeof createImpactSurface> | undefined
  let lastGround = 0,
    lastAir = 0
  const bed = root.getElementById('impactBed') as HTMLCanvasElement
  const air = root.getElementById('impactDebris') as HTMLCanvasElement
  const ground = bed.getContext('2d'),
    space = air.getContext('2d')
  let lastYaw = NaN,
    lastPitch = NaN,
    lastRevision = -1,
    lastSize = 0
  const screen = (u: number) => 500 + R * u
  function clear() {
    planet.reset()
    shell.classList.remove('excavated')
    delete shell.dataset.impacts
    for (const c of [ground, space])
      if (c) {
        c.save()
        c.resetTransform()
        c.clearRect(0, 0, c.canvas.width, c.canvas.height)
        c.restore()
      }
    shell.style.clipPath = ''
    lastRevision = -1
  }
  function draw(yaw: number, pitch: number, size: number, dynamic: boolean) {
    if (!planet.hits || !ground || !space) return
    const px = Math.max(320, Math.min(1050, Math.round(size)))
    const cameraMoved = yaw !== lastYaw || pitch !== lastPitch || px !== lastSize
    const now = performance.now()
    const update = (cameraMoved || planet.revision !== lastRevision) && (lastRevision < 0 || now - lastGround >= 32)
    if (!update && !dynamic) return
    if (!update && now - lastAir < 16) return
    lastAir = now
    if (space.canvas.width !== px || space.canvas.height !== px) space.canvas.width = space.canvas.height = px
    space.setTransform(px / 1000, 0, 0, px / 1000, 0, 0)
    const cy = Math.cos(yaw),
      sy = Math.sin(yaw),
      cp = Math.cos(pitch),
      sp = Math.sin(pitch)
    const view = (v: GlobePoint) => {
      const z = v.z * cy - v.x * sy
      return { x: v.x * cy + v.z * sy, y: v.y * cp - z * sp, z: v.y * sp + z * cp }
    }
    if (update) {
      lastYaw = yaw
      lastPitch = pitch
      lastSize = px
      lastRevision = planet.revision
      lastGround = now
      shell.classList.add('excavated')
      shell.dataset.impacts = String(planet.hits)
      terrain ??= createImpactSurface(planet, ground, shell)
      terrain.render(view, Math.min(650, px))
    }
    space.clearRect(0, 0, 1000, 1000)
    const visiblePoint = (p: GlobePoint) => terrain?.visible(p) ?? p.z >= 0
    // A compression wave propagates along the curved ground from each contact.
    for (const w of planet.waves) {
      const n = w.at,
        len = Math.hypot(n.x, n.z),
        east = len ? { x: n.z / len, y: 0, z: -n.x / len } : { x: 1, y: 0, z: 0 }
      const north = { x: n.y * east.z, y: n.z * east.x - n.x * east.z, z: -n.y * east.x }
      const angle = 0.02 + (w.reach + 0.18) * Math.sqrt(w.age / 0.85)
      space.beginPath()
      let pen = false
      for (let i = 0; i <= 60; i++) {
        const phi = (Math.PI * i) / 30
        const p = plus(
          times(n, Math.cos(angle)),
          times(plus(times(east, Math.cos(phi)), times(north, Math.sin(phi))), Math.sin(angle))
        )
        const projected = view(times(p, planet.surface(p) + 0.008))
        if (projected.z < 0) {
          pen = false
          continue
        }
        if (pen) space.lineTo(screen(projected.x), screen(projected.y))
        else space.moveTo(screen(projected.x), screen(projected.y))
        pen = true
      }
      space.strokeStyle = 'rgba(255,205,149,' + 0.72 * (1 - w.age / 0.85) ** 2 + ')'
      space.lineWidth = 1 + 4 * (1 - w.age / 0.85)
      space.stroke()
    }
    const parcels = planet.active
      .map((b) => ({ b, p: view(b.p) }))
      .filter(({ p }) => visiblePoint(p))
      .sort((a, b) => a.p.z - b.p.z)
    for (const { b, p } of parcels) {
      const x = screen(p.x),
        y = screen(p.y),
        r = Math.max(1.6, Math.cbrt(b.mass) * R * 0.53)
      const glow = Math.exp(-b.age * 1.25)
      space.beginPath()
      let pen = false
      for (const t of [...b.trail, b.p]) {
        const v = view(t)
        if (!visiblePoint(v)) {
          pen = false
          continue
        }
        if (pen) space.lineTo(screen(v.x), screen(v.y))
        else space.moveTo(screen(v.x), screen(v.y))
        pen = true
      }
      space.strokeStyle = 'rgba(243,168,96,' + (0.07 + 0.37 * glow) + ')'
      space.lineWidth = Math.max(0.8, r * 0.32)
      space.stroke()
      space.save()
      space.translate(x, y)
      space.rotate(b.seed + b.age * (0.6 + (b.seed % 5)))
      space.fillStyle =
        'rgb(' +
        Math.round(90 + 165 * glow) +
        ',' +
        Math.round(78 + 104 * glow) +
        ',' +
        Math.round(68 + 43 * glow) +
        ')'
      space.beginPath()
      space.moveTo(-r, -r * 0.4)
      space.lineTo(-r * 0.12, -r * 0.84)
      space.lineTo(r * 0.95, -r * 0.26)
      space.lineTo(r * 0.64, r * 0.64)
      space.lineTo(-r * 0.62, r * 0.76)
      space.closePath()
      space.fill()
      space.fillStyle = 'rgba(37,30,29,.46)'
      space.beginPath()
      space.moveTo(-r, -r * 0.4)
      space.lineTo(r * 0.24, r * 0.12)
      space.lineTo(r * 0.64, r * 0.64)
      space.lineTo(-r * 0.62, r * 0.76)
      space.fill()
      space.restore()
    }
  }
  return { planet, draw, clear }
}
