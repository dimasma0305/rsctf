/**
 * 2D-canvas "WINDOW FROST" renderer for the scoreboard-freeze cinematic. Dendritic frost
 * ferns (60° hexagonal branching) creep inward from the four corners — like frost spreading
 * across a cold window — with drifting bokeh, suspended glints, a soft shockwave ring and a
 * cold entry flash. Refined/edge-faded palette so the crystal melts into the dark.
 *
 * Cheap by construction: the expensive recursive ferns are BAKED ONCE to an offscreen canvas
 * (with a radial edge-fade mask), then each frame only blits that bitmap — so the per-frame
 * cost is a handful of drawImage calls, not thousands of strokes. Unlike the one-shot
 * first-blood nova this HOLDS: `cover` ramps 0->1 over ~1.8s then stays until stop().
 */
const TAU = Math.PI * 2
const ease = (x: number) => (x < 0.5 ? 2 * x * x : 1 - Math.pow(-2 * x + 2, 2) / 2)
const rnd = (a: number, b: number) => a + Math.random() * (b - a)
const clamp = (v: number, a: number, b: number) => (v < a ? a : v > b ? b : v)
const FERN = '205,234,255'

// soft round glow sprite (tinted/scaled per use) — bokeh, central bloom, glints
function softDot(): HTMLCanvasElement {
  const c = document.createElement('canvas'); c.width = c.height = 64
  const g = c.getContext('2d')!, gr = g.createRadialGradient(32, 32, 0, 32, 32, 32)
  gr.addColorStop(0, 'rgba(255,255,255,1)'); gr.addColorStop(0.4, 'rgba(255,255,255,0.5)'); gr.addColorStop(1, 'rgba(255,255,255,0)')
  g.fillStyle = gr; g.beginPath(); g.arc(32, 32, 32, 0, TAU); g.fill()
  return c
}

type Bok = { x: number; y: number; r: number; vx: number; vy: number; a: number; ph: number; tw: number }
type Gl = { x: number; y: number; r: number; tw: number; ph: number }

export function createFzRenderer(canvas: HTMLCanvasElement) {
  const ctx = canvas.getContext('2d')!
  const dpr = Math.min(window.devicePixelRatio || 1, 2)
  const DOT = softDot()
  let W = window.innerWidth || 1, H = window.innerHeight || 1
  let running = false, raf = 0, t0 = 0
  let baked: HTMLCanvasElement | null = null, bw = 0, bh = 0
  let bokeh: Bok[] = [], glints: Gl[] = []

  function fernBake(o: CanvasRenderingContext2D, x0: number, y0: number, ang: number, len: number, w: number, depth: number, a: number) {
    if (len < 3 || a < 0.03) return
    const x1 = x0 + Math.cos(ang) * len, y1 = y0 + Math.sin(ang) * len
    o.strokeStyle = `rgba(${FERN},${a})`; o.lineWidth = Math.max(0.5, w)
    o.beginPath(); o.moveTo(x0, y0); o.lineTo(x1, y1); o.stroke()
    if (depth <= 0) return
    const nb = clamp(Math.round(len / 26), 2, 6)
    for (let i = 1; i <= nb; i++) {
      const fr = i / (nb + 1), bx = x0 + Math.cos(ang) * len * fr, by = y0 + Math.sin(ang) * len * fr, bl = len * 0.32 * (1 - 0.5 * fr)
      fernBake(o, bx, by, ang + 1.047, bl, w * 0.58, depth - 1, a * 0.82)
      fernBake(o, bx, by, ang - 1.047, bl, w * 0.58, depth - 1, a * 0.82)
    }
  }
  function bake() {
    const oc = document.createElement('canvas'); oc.width = Math.max(1, Math.round(W * dpr)); oc.height = Math.max(1, Math.round(H * dpr))
    const o = oc.getContext('2d')!; o.setTransform(dpr, 0, 0, dpr, 0, 0); o.globalCompositeOperation = 'lighter'; o.lineCap = 'round'; o.lineJoin = 'round'
    const cx = W / 2, cy = H * 0.46, V = Math.min(W, H)
    // ferns growing from each corner toward the centre (three fanned per corner)
    for (const [x, y] of [[0, 0], [W, 0], [0, H], [W, H]]) {
      const ang = Math.atan2(cy - y, cx - x), L = Math.hypot(cx - x, cy - y) * 1.05
      for (let k = -1; k <= 1; k++) fernBake(o, x, y, ang + k * 0.42, rnd(0.78, 0.98) * L, 2.4, 2, 0.42)
    }
    // edge-fade mask: strong in the mid-field, soft at the very centre + far edges (delicate)
    o.globalCompositeOperation = 'destination-in'; o.setTransform(dpr, 0, 0, dpr, 0, 0)
    const mg = o.createRadialGradient(cx, cy, V * 0.1, cx, cy, V * 1.0)
    mg.addColorStop(0, 'rgba(255,255,255,.15)'); mg.addColorStop(0.5, 'rgba(255,255,255,.85)'); mg.addColorStop(1, 'rgba(255,255,255,.2)')
    o.fillStyle = mg; o.fillRect(0, 0, W, H)
    baked = oc; bw = W; bh = H
  }
  function size() {
    W = window.innerWidth || 1; H = window.innerHeight || 1
    canvas.width = Math.max(1, Math.round(W * dpr)); canvas.height = Math.max(1, Math.round(H * dpr))
  }
  function seed() {
    const V = Math.min(W, H)
    bokeh = []; for (let i = 0; i < 16; i++) bokeh.push({ x: rnd(0, W), y: rnd(0, H), r: rnd(0.02, 0.06) * V, vx: rnd(-0.4, 0.4) * (V / 600), vy: rnd(-0.5, -0.1) * (V / 600), a: rnd(0.06, 0.16), ph: rnd(0, TAU), tw: rnd(0.6, 1.4) })
    glints = []; for (let i = 0; i < 34; i++) glints.push({ x: rnd(0.06, 0.94) * W, y: rnd(0.08, 0.92) * H, r: rnd(1.4, 3.6) * (V / 620), tw: rnd(2, 6), ph: rnd(0, TAU) })
  }
  function draw(t: number) {
    const cx = W / 2, cy = H * 0.46, V = Math.min(W, H)
    const cover = t < 1.8 ? ease(t / 1.8) : 1 // ramp then HOLD
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0)
    ctx.clearRect(0, 0, W, H)
    // cold entry flash
    const fl = Math.max(0, 1 - t / 0.32) * 0.4; if (fl > 0) { ctx.fillStyle = `rgba(225,242,255,${fl})`; ctx.fillRect(0, 0, W, H) }
    // icy edge wash
    const g = ctx.createRadialGradient(cx, cy, V * 0.1, cx, cy, V * 0.9)
    g.addColorStop(0, 'rgba(150,210,255,0)'); g.addColorStop(0.6, `rgba(120,185,250,${cover * 0.06})`); g.addColorStop(1, `rgba(150,205,255,${cover * 0.3})`)
    ctx.fillStyle = g; ctx.fillRect(0, 0, W, H)
    ctx.globalCompositeOperation = 'lighter'
    // drifting bokeh
    for (const b of bokeh) {
      b.x += b.vx * 0.016; b.y += b.vy * 0.016
      if (b.y < -b.r) b.y = H + b.r; if (b.x < -b.r) b.x = W + b.r; if (b.x > W + b.r) b.x = -b.r
      const tw = 0.7 + 0.3 * Math.sin(t * b.tw + b.ph), al = cover * b.a * tw
      if (al > 0.01) { ctx.globalAlpha = al; ctx.drawImage(DOT, b.x - b.r, b.y - b.r, b.r * 2, b.r * 2) }
    }
    ctx.globalAlpha = 1
    // baked corner-frost, faded in
    if (baked && cover > 0.01) { ctx.globalAlpha = Math.min(1, cover * 1.05); ctx.drawImage(baked, 0, 0, W, H); ctx.globalAlpha = 1 }
    // one gentle cold ring on entry
    const rr = Math.max(0.1, t * 1.7 * V), ra = Math.max(0, 1 - t / 0.8)
    if (ra > 0.01) { ctx.strokeStyle = `rgba(170,220,255,${ra * 0.4})`; ctx.lineWidth = Math.max(1, V * 0.01 * ra); ctx.beginPath(); ctx.arc(cx, cy, rr, 0, TAU); ctx.stroke() }
    // central bloom
    if (cover > 0.02) { const br = V * 0.16 * cover; ctx.globalAlpha = cover * 0.5; ctx.drawImage(DOT, cx - br, cy - br, br * 2, br * 2); ctx.globalAlpha = 1 }
    // suspended glints
    for (const gl of glints) {
      const tw = 0.5 + 0.5 * Math.sin(t * gl.tw + gl.ph), a = cover * tw * 0.8
      if (a < 0.03) continue; const r = gl.r * (0.8 + 0.6 * tw)
      ctx.globalAlpha = a; ctx.drawImage(DOT, gl.x - r, gl.y - r, r * 2, r * 2)
    }
    ctx.globalAlpha = 1; ctx.globalCompositeOperation = 'source-over'
  }
  function loop() {
    if (!running) return
    draw((performance.now() - t0) / 1000)
    raf = requestAnimationFrame(loop)
  }
  return {
    start() {
      size(); if (!baked || bw !== W || bh !== H) bake(); seed()
      t0 = performance.now()
      if (!running) { running = true; raf = requestAnimationFrame(loop) }
    },
    stop() {
      running = false; if (raf) cancelAnimationFrame(raf); raf = 0
      try { ctx.setTransform(dpr, 0, 0, dpr, 0, 0); ctx.clearRect(0, 0, W, H) } catch (e) {}
    },
    resize() {
      const was = running; size(); if (baked && (bw !== W || bh !== H)) baked = null
      if (was && !baked) bake()
    },
    destroy() { running = false; if (raf) cancelAnimationFrame(raf); raf = 0 },
  }
}
