/**
 * PixiJS v8 WebGL renderer for the JEOPARDY constellation layer (Phase 2 of the
 * Pixi migration). Separate from fxRenderer.ts on purpose: the jeopardy stars live
 * in WRAP-pixel space across the whole `.arena-wrap` (the side bands), whereas the
 * combat FX live in the square arena's 0..1000 space. Keeping a second, isolated
 * Pixi Application avoids restructuring the already-shipped FX renderer and cleanly
 * separates the two coordinate systems / blend regimes.
 *
 * WHY: at ~40 non-dense challenges the SVG path ran 40 infinite CSS opacity
 * animations (each re-rasterizing a glow-sized SVG region every refresh) — that was
 * the lag. Here the star bodies are GPU particles whose alpha is the SAME twinkle
 * formula computed per frame, and glows/crosshairs/lines/rings/lasers are batched
 * Graphics. The #jeop SVG is kept ONLY as the static text + invisible `.chhit`
 * hit-test layer (no more `.twk`), so nothing repaints continuously.
 *
 * Lifecycle mirrors fxRenderer: async init, autoStart:false, own overlay canvas,
 * `ready` stays false until init resolves (and forever if WebGL fails) so the host
 * keeps the full SVG path as a zero-regression fallback. The host drives it from the
 * single engine rAF loop (render(now) → one app.render()). setStars() does a FULL
 * teardown+rebuild every call because layout() destroys the challenge objects'
 * identity each relayout — a reconcile-by-ref (like fxRenderer's) would leak.
 */
import { Application, Graphics, ParticleContainer, Particle, Rectangle, Texture } from 'pixi.js'
import { prefersReducedMotion } from './reducedMotion'

export interface JeopRenderer {
  readonly ready: boolean
  setStars(cats: any[], dense: boolean): void
  render(now: number, frozen?: boolean): void
  beam(tx: number, ty: number, sx: number, sy: number, sr: number, col: string, big?: boolean): void
  flash(x: number, y: number, r: number, col: string): void
  resize(W: number, H: number, wx0: number, wy0: number, wsize: number): void
  active(): boolean
  destroy(): void
}

const colNum = (c: string): number => {
  if (typeof c !== 'string') return 0xffffff
  if (c[0] === '#') { const h = c.slice(1); return parseInt(h.length === 3 ? h.replace(/./g, '$&$&') : h, 16) }
  return 0xffffff
}

const STAR_TEX_R = 30 // baked star radius in texture px; particle scale = c.r / STAR_TEX_R
function bakeStar(): Texture {
  const c = document.createElement('canvas'); c.width = c.height = 64
  const g = c.getContext('2d')!
  const R = STAR_TEX_R, r = R * 0.3, cx = 32, cy = 32
  g.fillStyle = '#fff'; g.beginPath()
  g.moveTo(cx, cy - R); g.lineTo(cx + r, cy - r); g.lineTo(cx + R, cy); g.lineTo(cx + r, cy + r)
  g.lineTo(cx, cy + R); g.lineTo(cx - r, cy + r); g.lineTo(cx - R, cy); g.lineTo(cx - r, cy - r)
  g.closePath(); g.fill()
  return Texture.from(c)
}

export function createJeopRenderer(wrapEl: HTMLElement, opts?: { onReady?: () => void }): JeopRenderer {
  const app = new Application()
  let ready = false, disposed = false, dense = false
  let wx0 = 0, wy0 = 0, wsize = 0, lastNow = 0, lastDraw = 0
  let lastW = 1, lastH = 1 // latest wrap size from resize(); applied once init resolves
  const canvas = document.createElement('canvas')
  canvas.style.cssText = 'position:absolute;inset:0;width:100%;height:100%;pointer-events:none;z-index:4'
  wrapEl.appendChild(canvas) // last child of .arena-wrap → paints over #jeop (z4) & .arena, under #fsBtn/#jtip (z9)

  let starTex: Texture
  let glowG: Graphics, lineG: Graphics, overG: Graphics, beamG: Graphics, starsPC: ParticleContainer
  let cats: any[] = []
  let pairs: { o: any; star: Particle }[] = []
  const fxq: any[] = [] // jeopardy lasers/flashes, advanced in render()

  const r0 = wrapEl.getBoundingClientRect()
  lastW = Math.max(r0.width, 1); lastH = Math.max(r0.height, 1)
  ;(async () => {
    await app.init({ canvas, backgroundAlpha: 0, antialias: true, autoStart: false, autoDensity: true, resolution: window.devicePixelRatio || 1, width: lastW, height: lastH, preference: 'webgpu' })
    if (disposed) { app.destroy({ removeView: true }, { children: true, texture: true }); return }
    starTex = bakeStar()
    const bounds = new Rectangle(-2000, -2000, 8000, 8000)
    glowG = new Graphics(); glowG.blendMode = 'add' // glow halos (tinted)
    lineG = new Graphics() // constellation guide lines (under stars)
    starsPC = new ParticleContainer({ dynamicProperties: { position: true, color: true }, boundsArea: bounds }) // star bodies
    overG = new Graphics() // white cores + crosshairs + solved rings (over stars)
    beamG = new Graphics(); beamG.blendMode = 'add' // lasers + flashes (topmost)
    app.stage.addChild(glowG, lineG, starsPC, overG, beamG)
    app.renderer.resize(lastW, lastH) // apply the latest wrap size (sizeCanvas may have run while !ready)
    ready = true
    rebuild() // build particles from any stars handed over before init resolved
    opts?.onReady?.()
  })().catch(() => { ready = false /* keep the SVG fallback alive */ })

  // FULL teardown + rebuild. layout() does CHALLENGES.length=0 and rebuilds NEW spread-cloned
  // objects each relayout, so a back-ref reconcile would orphan particles — rebuild from scratch.
  function rebuild() {
    if (!ready) return
    for (const { star } of pairs) starsPC.removeParticle(star)
    pairs = []
    glowG.clear(); lineG.clear(); overG.clear()
    if (dense || !cats.length) { app.render(); return } // dense/mobile → SVG renders the static stars; Pixi idle
    for (const cat of cats) {
      if (!cat.ch || !cat.ch.length) continue
      const tint = colNum(cat.color)
      for (const o of cat.ch) {
        const sc = Math.max(o.r, 0.5) / STAR_TEX_R
        const star = new Particle({ texture: starTex, anchorX: 0.5, anchorY: 0.5, x: o.x, y: o.y, tint, scaleX: sc, scaleY: sc })
        starsPC.addParticle(star); pairs.push({ o, star })
      }
    }
    // constellation guide lines are static once laid out → draw once here, not per frame
    for (const cat of cats) {
      if (!cat.ch || !cat._links || !cat._links.length) continue
      let any = false
      for (const [a, b] of cat._links) { const A = cat.ch[a], B = cat.ch[b]; if (!A || !B) continue; lineG.moveTo(A.x, A.y).lineTo(B.x, B.y); any = true }
      if (any) lineG.stroke({ width: 1, color: colNum(cat.color), alpha: 0.3 })
    }
    app.render()
  }

  function drawFxq(dt: number) {
    for (let i = fxq.length - 1; i >= 0; i--) {
      const e = fxq[i]; e.t += dt / e.dur; const t = e.t
      if (t >= 1) { fxq.splice(i, 1); continue }
      if (e.kind === 'beam') {
        // PLASMA LANCE: thick flickering plasma + white-hot core grows to a traveling head,
        // with a head bloom, source glow and an impact bloom+ring at the star.
        const fade = t < 0.12 ? t / 0.12 : t > 0.6 ? Math.max(0, 1 - (t - 0.6) / 0.4) : 1
        const pt = Math.min(1, t / 0.4), fl = 0.82 + 0.18 * Math.sin(t * 50 + i)
        const hx = e.px + (e.sx - e.px) * pt, hy = e.py + (e.sy - e.py) * pt
        beamG.moveTo(e.px, e.py).lineTo(hx, hy).stroke({ width: 18 * fade * fl, color: e.col, alpha: 0.16 * fade })
        beamG.moveTo(e.px, e.py).lineTo(hx, hy).stroke({ width: 9 * fade, color: e.col, alpha: 0.45 * fade })
        beamG.moveTo(e.px, e.py).lineTo(hx, hy).stroke({ width: 4 * fade, color: 0xffebff, alpha: 0.92 * fade })
        beamG.circle(hx, hy, 10 * fade).fill({ color: 0xffebff, alpha: 0.5 * fade })
        beamG.circle(hx, hy, 5 * fade).fill({ color: 0xffffff, alpha: 0.9 * fade })
        beamG.circle(e.px, e.py, 8 * fade).fill({ color: e.col, alpha: 0.4 * fade })
        const rt = t > 0.4 ? (t - 0.4) / 0.6 : 0
        if (rt > 0) {
          beamG.circle(e.sx, e.sy, e.sr + rt * 18).fill({ color: e.col, alpha: 0.5 * (1 - rt) })
          beamG.circle(e.sx, e.sy, e.sr + rt * 26).stroke({ width: 3, color: 0xffebff, alpha: Math.max(0, 1 - rt) })
        }
      } else { // flash ring
        beamG.circle(e.x, e.y, e.r + 30 * t).stroke({ width: 2, color: e.col, alpha: 1 - t })
      }
    }
  }

  return {
    get ready() { return ready },
    active() { return ready && ((!dense && pairs.length > 0) || fxq.length > 0) },
    setStars(c: any[], d: boolean) { cats = c || []; dense = d; rebuild() },
    resize(W: number, H: number, x0: number, y0: number, ws: number) {
      wx0 = x0; wy0 = y0; wsize = ws; lastW = Math.max(W, 1); lastH = Math.max(H, 1)
      if (!ready) return
      app.renderer.resize(lastW, lastH) // jeopLayer is identity (1 unit = 1 wrap px); positions come from layout()
    },
    beam(tx: number, ty: number, sx: number, sy: number, sr: number, col: string, big?: boolean) {
      if (document.hidden || fxq.length > 64) return // backgrounded → render() never drains; don't accumulate
      fxq.push({ kind: 'beam', px: wx0 + (tx / 1000) * wsize, py: wy0 + (ty / 1000) * wsize, sx, sy, sr, col: colNum(col), big: !!big, t: 0, dur: 0.64 })
    },
    flash(x: number, y: number, r: number, col: string) { if (document.hidden || fxq.length > 64) return; fxq.push({ kind: 'flash', x, y, r, col: colNum(col), t: 0, dur: 0.62 }) },
    destroy() {
      disposed = true
      try { if (ready) app.destroy({ removeView: true }, { children: true, texture: true }) } catch (e) {}
      try { canvas.remove() } catch (e) {}
    },
    render(now: number, frozen?: boolean) {
      if (!ready) return
      const dt = lastNow ? Math.min(Math.max((now - lastNow) / 1000, 0), 0.05) : 0.016
      lastNow = now // always advance (keeps drawFxq dt correct across skipped/frozen frames)
      if (frozen) return // the .fz-overlay (z96) fully covers the side-band stars; lasers are suppressed while frozen
      const hasStars = !dense && pairs.length > 0
      if (!hasStars && !fxq.length) return // nothing to draw (rebuild already committed a cleared frame)
      // twinkle period is 2.4-4.5s → 30fps is visually identical; lasers (fxq) keep full 60fps
      if (!fxq.length && now - lastDraw < 33) return
      lastDraw = now
      glowG.clear(); overG.clear(); beamG.clear()
      // Accessibility: under prefers-reduced-motion, pin the twinkle phase to its peak so
      // stars render at a steady full alpha instead of oscillating. Everything else
      // (positions, glows, crosshairs, solved rings, lasers) is unchanged — the board is
      // fully drawn, it just doesn't pulse.
      const reduceMotion = prefersReducedMotion()
      if (hasStars) {
        const sec = now / 1000
        for (const { o, star } of pairs) {
          const x = o.x, y = o.y, R = o.r
          const solved = o.solvers.length > 0, dim = solved ? 0.62 : 1
          const o1 = dim, o2 = dim * (solved ? 0.8 : 0.55)
          const dur = 2.4 + (o.i * 0.7) % 2.1, dly = -(o.i * 0.6)
          const a01 = reduceMotion ? 1 : 0.5 + 0.5 * Math.cos(6.2832 * (sec - dly) / dur) // 1 at peak (--o), 0 at trough (--o2)
          const g = o2 + (o1 - o2) * a01 // group opacity, matches the jtwk keyframe
          const col = colNum(o.catObj.color)
          star.alpha = g
          glowG.circle(x, y, R * 2.1).fill({ color: col, alpha: g * 0.1 * dim })
          glowG.circle(x, y, R * 1.25).fill({ color: col, alpha: g * 0.17 * dim })
          if (o.base >= 300) { const gg = R * 2.5; overG.moveTo(x - gg, y).lineTo(x + gg, y).moveTo(x, y - gg).lineTo(x, y + gg).stroke({ width: 0.7, color: 0xffffff, alpha: g * 0.55 * dim }) }
          overG.circle(x, y, R * 0.34).fill({ color: 0xffffff, alpha: g })
          if (solved) overG.circle(x, y, R + 4).stroke({ width: 1.4, color: colNum(o.solvers[0].color), alpha: 0.85 })
        }
      }
      if (fxq.length) drawFxq(dt)
      app.render()
    },
  }
}
