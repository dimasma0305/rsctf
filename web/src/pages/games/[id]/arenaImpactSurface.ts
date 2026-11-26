import { GLOBE_RADIUS as R, type GlobePoint } from './arenaGlobeModel'
import type { createImpactPlanet } from './arenaImpactPhysics'

type Planet = ReturnType<typeof createImpactPlanet>
const tileSize = 128
const bound = (v: number) => Math.min(1, Math.max(0, v))

// The same mineral field follows each surface point through rotation. Hot
// fissures glow first; surrounding rock only incandesces at higher temperature.
function materials() {
  const tile = new Uint8Array(tileSize ** 2),
    palette = new Uint8Array(64 * 32 * 32 * 3)
  for (let y = 0; y < tileSize; y++)
    for (let x = 0; x < tileSize; x++) {
      const a = (x * Math.PI) / 64,
        b = (y * Math.PI) / 64
      const fold = Math.sin(2 * a + 3 * b + 1.9 * Math.sin(3 * a - b) + 0.72 * Math.sin(8 * b + 4 * a))
      const ridge = Math.max(0, 1 - Math.abs(fold) * 4.2)
      tile[y * tileSize + x] = Math.floor(
        31 * bound(0.16 + 0.15 * Math.sin(5 * a - 4 * b) + 0.1 * Math.sin(12 * a + 9 * b) + ridge * 0.69)
      )
    }
  for (let k = 0; k < 64; k++)
    for (let l = 0; l < 32; l++)
      for (let g = 0; g < 32; g++) {
        const heat = k / 49,
          light = l / 31,
          mineral = g / 31
        const lava = bound(heat * (0.38 + 0.76 * mineral) + 0.46 * Math.max(0, heat - 0.77))
        const base = (0.34 + 0.66 * light) * (0.75 + 0.28 * mineral)
        const i = ((k * 32 + l) * 32 + g) * 3
        palette[i] = Math.min(255, 62 * base + 228 * Math.min(1, lava * 1.55))
        palette[i + 1] = Math.min(245, 67 * base + 153 * lava ** 2)
        palette[i + 2] = Math.min(220, 68 * base + 60 * lava ** 4)
      }
  return { tile, palette }
}

export function createImpactSurface(planet: Planet, ctx: CanvasRenderingContext2D, shell: HTMLElement) {
  const { tile, palette } = materials(),
    n = planet.directions.length
  const x = new Float32Array(n),
    y = x.slice(),
    z = x.slice(),
    normalX = x.slice(),
    normalY = x.slice(),
    normalZ = x.slice(),
    lighting = x.slice(),
    opacity = x.slice()
  const u = planet.directions.map((v) => (Math.atan2(v.x, v.z) / (2 * Math.PI) + 0.5) * tileSize * 5)
  const v = planet.directions.map((p) => (Math.asin(p.y) / Math.PI + 0.5) * tileSize * 3)
  let size = 0,
    depth = new Float32Array(0),
    frame: ImageData
  function render(view: (p: GlobePoint) => GlobePoint, requested: number) {
    if (size !== requested) {
      size = requested
      ctx.canvas.width = ctx.canvas.height = size
      frame = ctx.createImageData(size, size)
      depth = new Float32Array(size * size)
    }
    depth.fill(-4)
    frame.data.fill(0)
    normalX.fill(0)
    normalY.fill(0)
    normalZ.fill(0)
    const S = (size * R) / 1000
    for (let i = 0; i < n; i++) {
      const p = view(planet.directions[i]),
        r = planet.radius[i]
      x[i] = p.x * r
      y[i] = p.y * r
      z[i] = p.z * r
      opacity[i] = bound(Math.abs(r - 1) * 22 + planet.heat[i] * 0.18)
    }
    for (const [a, b, c] of planet.faces) {
      const dx = x[b] - x[a],
        dy = y[b] - y[a],
        dz = z[b] - z[a],
        ex = x[c] - x[a],
        ey = y[c] - y[a],
        ez = z[c] - z[a]
      const nx = dy * ez - dz * ey,
        ny = dz * ex - dx * ez,
        nz = dx * ey - dy * ex
      for (const i of [a, b, c]) {
        normalX[i] += nx
        normalY[i] += ny
        normalZ[i] += nz
      }
    }
    for (let i = 0; i < n; i++) {
      const len = Math.hypot(normalX[i], normalY[i], normalZ[i]) || 1
      lighting[i] = bound((-0.48 * normalX[i] - 0.51 * normalY[i] + 0.71 * normalZ[i]) / len)
      x[i] = size * 0.5 + S * x[i]
      y[i] = size * 0.5 + S * y[i]
    }
    for (const [a, b, c] of planet.faces) {
      const ax = x[a],
        ay = y[a],
        bx = x[b] - ax,
        by = y[b] - ay,
        cx = x[c] - ax,
        cy = y[c] - ay
      const area = bx * cy - by * cx
      if (area <= 0.001) continue
      const x0 = Math.max(0, Math.floor(Math.min(ax, x[b], x[c]))),
        x1 = Math.min(size - 1, Math.ceil(Math.max(ax, x[b], x[c])))
      const y0 = Math.max(0, Math.floor(Math.min(ay, y[b], y[c]))),
        y1 = Math.min(size - 1, Math.ceil(Math.max(ay, y[b], y[c])))
      const ub = u[b] - u[a] - Math.round((u[b] - u[a]) / (5 * tileSize)) * 5 * tileSize
      const uc = u[c] - u[a] - Math.round((u[c] - u[a]) / (5 * tileSize)) * 5 * tileSize
      const changed = opacity[a] + opacity[b] + opacity[c] > 0.001
      for (let py = y0; py <= y1; py++) {
        let wb = ((x0 + 0.5 - ax) * cy - (py + 0.5 - ay) * cx) / area,
          wc = (bx * (py + 0.5 - ay) - by * (x0 + 0.5 - ax)) / area
        for (let px = x0; px <= x1; px++, wb += cy / area, wc -= by / area) {
          const wa = 1 - wb - wc
          if (wa < 0 || wb < 0 || wc < 0) continue
          const id = py * size + px,
            alt = wa * z[a] + wb * z[b] + wc * z[c]
          if (alt <= depth[id]) continue
          depth[id] = alt
          if (!changed) {
            frame.data[id * 4 + 3] = 0
            continue
          }
          const light = Math.round(31 * (wa * lighting[a] + wb * lighting[b] + wc * lighting[c]))
          const heat = Math.min(63, Math.round(49 * (wa * planet.heat[a] + wb * planet.heat[b] + wc * planet.heat[c])))
          const tx = Math.floor(u[a] + wb * ub + wc * uc) & 127,
            ty = Math.floor(wa * v[a] + wb * v[b] + wc * v[c]) & 127
          const color = ((heat * 32 + light) * 32 + tile[ty * 128 + tx]) * 3
          const out = id * 4
          frame.data[out] = palette[color]
          frame.data[out + 1] = palette[color + 1]
          frame.data[out + 2] = palette[color + 2]
          frame.data[out + 3] = 255 * (wa * opacity[a] + wb * opacity[b] + wc * opacity[c])
        }
      }
    }
    ctx.putImageData(frame, 0, 0)
    // Clip the original illustration to the actual visible limb. Using the
    // depth buffer here avoids cutting tile-shaped holes through the artwork.
    const outline = []
    for (let i = 0; i < 180; i++) {
      const angle = (i * Math.PI) / 90,
        dx = Math.cos(angle),
        dy = Math.sin(angle)
      let r = S * 1.06
      for (; r > 0; r -= 0.8) {
        const px = Math.floor(size * 0.5 + r * dx),
          py = Math.floor(size * 0.5 + r * dy)
        if (depth[py * size + px] > -4) break
      }
      outline.push((50 + (100 * r * dx) / size).toFixed(2) + '% ' + (50 + (100 * r * dy) / size).toFixed(2) + '%')
    }
    shell.style.clipPath = 'polygon(' + outline.join(',') + ')'
  }
  return {
    render,
    visible(p: GlobePoint) {
      const px = Math.floor(size * (0.5 + (p.x * R) / 1000)),
        py = Math.floor(size * (0.5 + (p.y * R) / 1000))
      return px < 0 || py < 0 || px >= size || py >= size || p.z > depth[py * size + px] - 0.018
    },
  }
}
