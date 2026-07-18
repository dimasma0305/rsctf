/**
 * 2D-canvas VICTORY effects for the MATCH COMPLETE / podium screen: slowly-rotating
 * gold god-rays, drifting confetti and rising gold sparkles over a warm centre glow.
 * A cheap continuous loop (one baked soft-dot sprite; ~30 confetti + ~26 sparkles +
 * 18 ray triangles per frame). Runs while the win overlay is shown — start()/stop().
 */
const TAU = Math.PI * 2
const rnd = (a: number, b: number) => a + Math.random() * (b - a)
const GOLD = '255,198,55'
const COLS = ['255,198,55', '255,255,255', '255,122,58', '120,200,255', '157,255,180', '255,120,150']

function softDot(): HTMLCanvasElement {
  const c = document.createElement('canvas'); c.width = c.height = 48
  const g = c.getContext('2d')!, gr = g.createRadialGradient(24, 24, 0, 24, 24, 24)
  gr.addColorStop(0, 'rgba(255,255,255,1)'); gr.addColorStop(0.4, 'rgba(255,255,255,0.5)'); gr.addColorStop(1, 'rgba(255,255,255,0)')
  g.fillStyle = gr; g.beginPath(); g.arc(24, 24, 24, 0, TAU); g.fill()
  return c
}

type Conf = { x: number; y: number; vx: number; vy: number; w: number; h: number; rot: number; vr: number; col: string; sw: number }
type Spark = { x: number; y: number; r: number; vy: number; tw: number; ph: number }

export function createWinRenderer(canvas: HTMLCanvasElement) {
  const ctx = canvas.getContext('2d')!
  const dpr = Math.min(window.devicePixelRatio || 1, 2)
  const DOT = softDot()
  let W = window.innerWidth || 1, H = window.innerHeight || 1
  let running = false, raf = 0, t0 = 0, lastT = 0
  let conf: Conf[] = [], spark: Spark[] = []

  function size() {
    W = window.innerWidth || 1; H = window.innerHeight || 1
    canvas.width = Math.max(1, Math.round(W * dpr)); canvas.height = Math.max(1, Math.round(H * dpr))
  }
  function seed() {
    const V = Math.min(W, H)
    conf = []; for (let i = 0; i < 34; i++) conf.push({ x: rnd(0, W), y: rnd(-H, 0), vx: rnd(-0.2, 0.2) * V, vy: rnd(0.25, 0.6) * V, w: rnd(0.008, 0.016) * V, h: rnd(0.014, 0.03) * V, rot: rnd(0, TAU), vr: rnd(-4, 4), col: COLS[(Math.random() * COLS.length) | 0], sw: rnd(0.6, 1.6) })
    spark = []; for (let i = 0; i < 26; i++) spark.push({ x: rnd(0, W), y: rnd(0, H), r: rnd(1.5, 4) * (V / 620), vy: rnd(-0.12, -0.03) * V, tw: rnd(2, 6), ph: rnd(0, TAU) })
  }
  function draw(t: number, dt: number) {
    const cx = W / 2, cy = H * 0.4, V = Math.min(W, H)
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0)
    ctx.clearRect(0, 0, W, H)
    // warm centre glow
    const g = ctx.createRadialGradient(cx, cy, 0, cx, cy, V * 0.75)
    g.addColorStop(0, 'rgba(255,200,90,.16)'); g.addColorStop(0.5, 'rgba(255,170,60,.05)'); g.addColorStop(1, 'rgba(255,170,60,0)')
    ctx.fillStyle = g; ctx.fillRect(0, 0, W, H)
    ctx.globalCompositeOperation = 'lighter'
    // rotating god-rays
    const n = 18, rot = t * 0.16
    for (let i = 0; i < n; i++) {
      const a = rot + (i / n) * TAU, wd = 0.1
      ctx.fillStyle = `rgba(${GOLD},${0.05 + 0.03 * Math.sin(t * 1.5 + i)})`
      ctx.beginPath(); ctx.moveTo(cx, cy)
      ctx.lineTo(cx + Math.cos(a - wd) * V * 1.4, cy + Math.sin(a - wd) * V * 1.4)
      ctx.lineTo(cx + Math.cos(a + wd) * V * 1.4, cy + Math.sin(a + wd) * V * 1.4)
      ctx.closePath(); ctx.fill()
    }
    // rising sparkles
    for (const s of spark) {
      s.y += s.vy * dt; if (s.y < -10) { s.y = H + 10; s.x = rnd(0, W) }
      const tw = 0.5 + 0.5 * Math.sin(t * s.tw + s.ph), a = tw * 0.8, r = s.r * (0.7 + 0.5 * tw)
      ctx.globalAlpha = a; ctx.drawImage(DOT, s.x - r, s.y - r, r * 2, r * 2)
    }
    ctx.globalAlpha = 1; ctx.globalCompositeOperation = 'source-over'
    // confetti
    for (const c of conf) {
      c.x += c.vx * dt + Math.sin(t * c.sw + c.rot) * 0.4; c.y += c.vy * dt; c.rot += c.vr * dt
      if (c.y > H + 20) { c.y = -20; c.x = rnd(0, W) }
      ctx.save(); ctx.translate(c.x, c.y); ctx.rotate(c.rot)
      ctx.fillStyle = `rgba(${c.col},.95)`; ctx.fillRect(-c.w / 2, -c.h / 2, c.w, c.h * (0.5 + 0.5 * Math.abs(Math.cos(c.rot))))
      ctx.restore()
    }
  }
  function loop() {
    if (!running) return
    const now = performance.now(), t = (now - t0) / 1000
    const dt = Math.max(0, Math.min(0.05, (now - lastT) / 1000)); lastT = now
    draw(t, dt); raf = requestAnimationFrame(loop)
  }
  return {
    start() {
      size(); seed(); t0 = lastT = performance.now()
      if (!running) { running = true; raf = requestAnimationFrame(loop) }
    },
    stop() {
      running = false; if (raf) cancelAnimationFrame(raf); raf = 0
      try { ctx.setTransform(dpr, 0, 0, dpr, 0, 0); ctx.clearRect(0, 0, W, H) } catch (e) {}
    },
    resize() { const was = running; size(); if (was) seed() },
    destroy() { running = false; if (raf) cancelAnimationFrame(raf); raf = 0 },
  }
}
