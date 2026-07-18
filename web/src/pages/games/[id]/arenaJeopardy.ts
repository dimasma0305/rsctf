/**
 * Jeopardy constellation overlay for the A&D/KotH battle arena.
 *
 * Ported from lawbyte's CyberADArena reference and adapted to the platform map:
 * it renders the game's JEOPARDY challenges as star constellations in the empty
 * side bands beside the square battle wheel (one constellation per category,
 * star size ∝ point value), draws a laser beam from a team node to the actual
 * challenge star on a live solve, and shows a who-solved-it tooltip (gold/
 * silver/bronze) on hover/tap.
 *
 * It owns ONLY its own `#jeop` SVG overlay + `#jtip` tooltip inside the arena
 * panel — the main `#svg` battle wheel is untouched. Data (categories, points,
 * solve state) is injected via setData(); the host drives layout() on resize
 * and solveByTitle() from the live attack feed.
 */

const NS = 'http://www.w3.org/2000/svg'

export interface JeopChallenge {
  id: number
  name: string
  base: number // current (dynamic) point value from the scoreboard
  solvers: { name: string; color: string }[] // ordered (blood order); first = gold
  solveCount: number
}
export interface JeopCategory {
  id: string
  name: string
  color: string
  challenges: JeopChallenge[]
}
export interface JeopDeps {
  root: ShadowRoot
  arena: HTMLElement
  isFrozen: () => boolean
  isTouch: boolean
  // Pixi/WebGL star layer (jeopRenderer). When pixiReady() is true the SVG drops the
  // animated star visuals (emits only the invisible .chhit hit circle + text) and the
  // stars/lasers are drawn on the GPU instead. All optional → SVG-only fallback if absent.
  pixiReady?: () => boolean
  onStars?: (cats: any[], dense: boolean) => void // hand the laid-out categories to the Pixi layer (full rebuild)
  onBeam?: (tx: number, ty: number, sx: number, sy: number, sr: number, col: string) => void
  onFlash?: (x: number, y: number, r: number, col: string) => void
}

// deterministic 0..1 so layouts are stable across re-renders
const jrand = (s: number) => { const x = Math.sin(s * 12.9898) * 43758.5453; return x - Math.floor(x) }

// named asterism shapes for the common 5-challenge case; other counts get a procedural one
const ASTERISM = [
  { pts: [[0.06, 0.32], [0.30, 0.80], [0.52, 0.40], [0.74, 0.84], [0.96, 0.30]], links: [[0, 1], [1, 2], [2, 3], [3, 4]] },
  { pts: [[0.50, 0.05], [0.50, 0.45], [0.16, 0.52], [0.84, 0.55], [0.52, 0.97]], links: [[0, 1], [2, 1], [1, 3], [1, 4]] },
  { pts: [[0.08, 0.66], [0.32, 0.74], [0.55, 0.68], [0.74, 0.50], [0.84, 0.18]], links: [[0, 1], [1, 2], [2, 3], [3, 4]] },
  { pts: [[0.24, 0.16], [0.80, 0.30], [0.50, 0.54], [0.58, 0.82], [0.40, 0.98]], links: [[0, 1], [1, 2], [2, 0], [2, 3], [3, 4]] },
]
function procAster(ci: number, M: number) {
  const pts: number[][] = []
  for (let k = 0; k < M; k++) pts.push([0.08 + jrand(ci * 61.7 + k * 12.3 + 2.1) * 0.84, 0.06 + jrand(ci * 29.3 + k * 7.9 + 5.7) * 0.88])
  const order = pts.map((_p, i) => i).sort((a, b) => pts[a][1] - pts[b][1])
  const links: number[][] = []
  for (let k = 0; k < order.length - 1; k++) links.push([order[k], order[k + 1]])
  return { pts, links }
}
function starPath(x: number, y: number, R: number) {
  const r = R * 0.3
  return `M${x} ${(y - R).toFixed(1)} L${(x + r).toFixed(1)} ${(y - r).toFixed(1)} L${(x + R).toFixed(1)} ${y} L${(x + r).toFixed(1)} ${(y + r).toFixed(1)} L${x} ${(y + R).toFixed(1)} L${(x - r).toFixed(1)} ${(y + r).toFixed(1)} L${(x - R).toFixed(1)} ${y} L${(x - r).toFixed(1)} ${(y - r).toFixed(1)} Z`
}

export function createJeopardy(deps: JeopDeps) {
  const { root, arena } = deps
  const $ = (id: string): any => root.getElementById(id)
  const qs = (sel: string): any => root.querySelector(sel)

  let CATEGORIES: any[] = []
  const CHALLENGES: any[] = []
  let jeopReady = false, jDense = false, _jeopKilled = false
  let jWheelX = 0, jWheelY = 0, jWheelSize = 0
  // Pixi star layer takes over the animated visuals only on the non-dense desktop path
  // (dense/mobile already draws static, twinkle-free stars cheaply in SVG).
  const usePixi = () => !!(deps.pixiReady && deps.pixiReady()) && !jDense

  const esc = (s: any) => String(s == null ? '' : s).replace(/[<>&"]/g, (c) => ({ '<': '&lt;', '>': '&gt;', '&': '&amp;', '"': '&quot;' } as any)[c])

  function layout() {
    CHALLENGES.length = 0; jeopReady = false
    const wrap = qs('.arena-wrap'), js = $('jeop'), space = $('jeopSpace')
    if (!wrap || !js || !arena || !CATEGORIES.length) { if (js) js.innerHTML = ''; deps.onStars?.([], true); return }
    let r = wrap.getBoundingClientRect(); if (r.width < 2) return // transient (mid-mount/hidden) — leave the Pixi layer as-is
    let ar = arena.getBoundingClientRect()
    const wsize = ar.width
    const leftBand = ar.left - r.left, rightBand = r.right - ar.right
    const mobile = Math.min(leftBand, rightBand) < 80
    const N = CATEGORIES.length
    const total = CATEGORIES.reduce((a, c) => a + c.challenges.length, 0)
    jDense = total > 40 || N > 8 || mobile
    let gCols = 0, gRows = 0
    const cardH = 118
    if (mobile) {
      gCols = r.width < 360 ? 1 : 2
      gRows = Math.ceil(N / gCols)
      if (space) space.style.height = gRows * cardH + 12 + 'px'
    } else if (space) space.style.height = ''
    r = wrap.getBoundingClientRect(); ar = arena.getBoundingClientRect()
    const W = r.width, H = r.height, wx0 = ar.left - r.left, wy0 = ar.top - r.top
    jWheelX = wx0; jWheelY = wy0; jWheelSize = wsize
    js.setAttribute('viewBox', `0 0 ${W.toFixed(0)} ${H.toFixed(0)}`)
    // Pixi path (desktop) stretches with the canvas → 'none' keeps SVG text aligned; when the SVG
    // itself draws the stars (mobile/dense) 'xMidYMid meet' avoids non-uniform stretch on a
    // transient resize. (Bare 'meet' is not a valid enum value — browsers log an error and fall
    // back to the default, which happens to be xMidYMid meet.)
    js.setAttribute('preserveAspectRatio', usePixi() ? 'none' : 'xMidYMid meet')
    if (!mobile && Math.min(leftBand, rightBand) < 70) { CATEGORIES.forEach((c) => (c.ch = [])); jeopReady = false; js.innerHTML = ''; deps.onStars?.([], true); return }

    if (mobile) {
      const pad = 10, top0 = wy0 + wsize + 10, colW = (W - pad * 2) / gCols
      CATEGORIES.forEach((cat: any, ci: number) => {
        const col = ci % gCols, row = Math.floor(ci / gCols)
        const cellX0 = pad + col * colW, cellY0 = top0 + row * cardH
        const M = cat.challenges.length
        cat._side = cellX0 + colW / 2 < W / 2 ? 'L' : 'R'
        cat._title = { x: cellX0 + 2, y: cellY0 + 14, anchor: 'start', grid: true }
        const aster = procAster(ci, M)
        const mx = colW * 0.14, bx0 = cellX0 + mx, bw2 = colW - mx * 2, top = cellY0 + 24, boxH = cardH - 40
        cat.ch = cat.challenges.map((c: any, k: number) => {
          const p = aster.pts[k], xx = bx0 + p[0] * bw2, yy = top + p[1] * boxH
          const R = Math.min(4 + (c.base / 100) * 1.1, 7, (boxH / M) * 0.5, colW * 0.06)
          const o = { ...c, cat: cat.id, catObj: cat, i: k, x: xx, y: yy, r: R, side: cat._side, _lx: xx, _ly: yy + R + 12, _anchor: 'middle' }
          CHALLENGES.push(o); return o
        })
        cat._links = aster.links.slice(); cat._labels = false
      })
      jeopReady = true; deps.onStars?.(CATEGORIES, jDense); return
    }

    // desktop: side bands left/right of the wheel
    const pad = 12, wide = Math.min(leftBand, rightBand) >= 300, half = Math.ceil(N / 2)
    const fsMode = (document as any).fullscreenElement === wrap
    CATEGORIES.forEach((cat: any, ci: number) => {
      const side = ci < half ? 'L' : 'R', left = side === 'L'
      const idx = left ? ci : ci - half
      const per = left ? half : N - half
      const cols = per > 4 ? 2 : 1
      const rows = Math.ceil(per / cols)
      const col = Math.floor(idx / rows), row = idx % rows
      const thisBand = left ? leftBand : rightBand
      const bandX0 = left ? pad : wx0 + wsize + pad, bandW = thisBand - pad * 2
      const cellW = bandW / cols, cellX0 = bandX0 + col * cellW
      const topPad = 8, cellH = (H - topPad * 2) / rows, cellY0 = topPad + row * cellH
      const M = cat.challenges.length
      cat._side = side
      cat._title = { x: cellX0 + 2, y: cellY0 + 15, anchor: 'start', grid: cols > 1 }
      const single = cols === 1
      if (single && wide) {
        const aster = M === 5 ? ASTERISM[ci % ASTERISM.length] : procAster(ci, M)
        const mx = cellW * 0.13, bx0 = cellX0 + mx, bw2 = cellW - mx * 2, top = cellY0 + 44, boxH = Math.max(cellH - 104, 60)
        cat.ch = cat.challenges.map((c: any, k: number) => {
          const p = aster.pts[k], xx = bx0 + p[0] * bw2, yy = top + p[1] * boxH
          const R = Math.min(5 + (c.base / 100) * 1.6, 12, (boxH / M) * 0.5)
          const o = { ...c, cat: cat.id, catObj: cat, i: k, x: xx, y: yy, r: R, side, _lx: xx, _ly: yy + R + 14, _anchor: 'middle' }
          CHALLENGES.push(o); return o
        })
        cat._links = aster.links.slice(); cat._labels = fsMode && M <= 8
      } else if (single) {
        const itop = cellY0 + 46, ih = Math.max(cellH - 60, 40), slot = ih / Math.max(M, 1)
        const labels = fsMode && slot >= 26 && cellW >= 104
        const gaps: number[] = []; let tot = 0
        for (let k = 0; k < M; k++) { const gp = 0.6 + jrand(ci * 31.7 + k * 9.3 + 1.7) * 0.8; gaps.push(gp); tot += gp }
        let acc = 0
        cat.ch = cat.challenges.map((c: any, k: number) => {
          acc += gaps[k]; const yy = itop + (acc - gaps[k] * 0.5) / tot * ih
          const rx = jrand(ci * 53.3 + k * 7.7 + 4.1)
          const xf = labels ? (left ? 0.06 + rx * 0.46 : 0.48 + rx * 0.46) : 0.12 + rx * 0.76
          const xx = cellX0 + xf * cellW, R = Math.min(5 + (c.base / 100) * 1.4, cellW * 0.07 + 5, slot * 0.42)
          const lx = left ? xx + R + 7 : xx - R - 7
          const o = { ...c, cat: cat.id, catObj: cat, i: k, x: xx, y: yy, r: R, side, _lx: lx, _ly: yy + 1, _anchor: left ? 'start' : 'end' }
          CHALLENGES.push(o); return o
        })
        cat._links = []; for (let k = 0; k < M - 1; k++) cat._links.push([k, k + 1]); cat._labels = labels
      } else {
        const aster = procAster(ci, M)
        const mx = cellW * 0.16, bx0 = cellX0 + mx, bw2 = cellW - mx * 2, top = cellY0 + 26, boxH = Math.max(cellH - 44, 40)
        cat.ch = cat.challenges.map((c: any, k: number) => {
          const p = aster.pts[k], xx = bx0 + p[0] * bw2, yy = top + p[1] * boxH
          const R = Math.min(3.5 + (c.base / 100) * 1.1, 7, (boxH / M) * 0.42, cellW * 0.06)
          const o = { ...c, cat: cat.id, catObj: cat, i: k, x: xx, y: yy, r: R, side, _lx: xx, _ly: yy + R + 12, _anchor: 'middle' }
          CHALLENGES.push(o); return o
        })
        cat._links = aster.links.slice(); cat._labels = false
      }
    })
    jeopReady = true; deps.onStars?.(CATEGORIES, jDense)
  }

  // invisible hover/click hit target (the ONLY interactive element); always emitted on both paths
  const hitCircle = (c: any) => `<circle class="chhit" data-cat="${c.cat}" data-i="${c.i}" cx="${c.x}" cy="${c.y}" r="${Math.max(c.r + 11, 14).toFixed(1)}" fill="#fff" opacity="0"/>`

  function drawChallenge(c: any) {
    // Pixi-active: the GPU draws the star/glow/crosshair/solved-ring; SVG keeps only the hit target.
    if (usePixi()) return hitCircle(c)
    const cat = c.catObj, R = c.r, x = c.x, y = c.y
    const solved = c.solvers.length > 0
    const dim = solved ? 0.62 : 1
    let core = ''
    core += `<circle cx="${x}" cy="${y}" r="${(R * 2.1).toFixed(1)}" fill="${cat.color}" opacity="${(0.1 * dim).toFixed(3)}"/>`
    if (!jDense) core += `<circle cx="${x}" cy="${y}" r="${(R * 1.25).toFixed(1)}" fill="${cat.color}" opacity="${(0.17 * dim).toFixed(3)}"/>`
    if (c.base >= 300 && !jDense) { const g = (R * 2.5).toFixed(1); core += `<g opacity="${(0.55 * dim).toFixed(2)}" stroke="#fff" stroke-width="0.7"><line x1="${x - +g}" y1="${y}" x2="${+x + +g}" y2="${y}"/><line x1="${x}" y1="${y - +g}" x2="${x}" y2="${+y + +g}"/></g>` }
    core += `<path d="${starPath(x, y, R)}" fill="${cat.color}" stroke="#06050f" stroke-width="0.5"/>`
    core += `<circle cx="${x}" cy="${y}" r="${(R * 0.34).toFixed(2)}" fill="#fff"/>`
    let s
    if (jDense) s = `<g opacity="${dim.toFixed(2)}">${core}</g>`
    else {
      const dur = (2.4 + (c.i * 0.7) % 2.1).toFixed(2), dly = (-(c.i * 0.6)).toFixed(2)
      s = `<g class="twk" style="--o:${dim.toFixed(2)};--o2:${(dim * (solved ? 0.8 : 0.55)).toFixed(2)};--d:${dur}s;--dl:${dly}s">${core}</g>`
    }
    if (solved) s += `<circle cx="${x}" cy="${y}" r="${(R + 4).toFixed(1)}" fill="none" stroke="${c.solvers[0].color}" stroke-width="1.4" opacity="0.85"/>`
    s += hitCircle(c)
    return s
  }

  function render() {
    const host = $('jeop'); if (!host) return
    if (!jeopReady) { host.innerHTML = ''; return }
    let out = ''
    CATEGORIES.forEach((cat: any) => {
      if (!cat.ch || !cat.ch.length) return
      let lines = ''
      cat._links.forEach(([a, b]: number[]) => { const A = cat.ch[a], B = cat.ch[b]; if (!A || !B) return; lines += `<line x1="${A.x.toFixed(1)}" y1="${A.y.toFixed(1)}" x2="${B.x.toFixed(1)}" y2="${B.y.toFixed(1)}" stroke="${cat.color}" stroke-width="1" stroke-opacity="0.30"/>` })
      let stars = '', labels = ''
      cat.ch.forEach((c: any) => {
        stars += `<g id="ch-${cat.id}-${c.i}">${drawChallenge(c)}</g>`
        if (cat._labels) {
          const lx = c._lx.toFixed(1), anc = c._anchor
          labels += `<text x="${lx}" y="${c._ly.toFixed(1)}" text-anchor="${anc}" fill="#d4dcef" font-family="'VT323'" font-size="15" paint-order="stroke" stroke="#06050f" stroke-width="3">${esc(c.name)}</text>`
          labels += `<text id="chv-${cat.id}-${c.i}" x="${lx}" y="${(c._ly + 14).toFixed(1)}" text-anchor="${anc}" fill="${cat.color}" font-family="'VT323'" font-size="13" paint-order="stroke" stroke="#06050f" stroke-width="3">${c.base}pt</text>`
        }
      })
      const t = cat._title, tf = t.grid ? 9 : CATEGORIES.length > 6 ? 10 : 12, cf = t.grid ? 13 : 15
      const hint = cat._labels ? '' : deps.isTouch ? ' · tap' : ' · hover'
      const done = cat.ch.filter((x: any) => x.solvers.length > 0).length
      const title = `<text x="${t.x.toFixed(1)}" y="${t.y.toFixed(1)}" text-anchor="${t.anchor}" fill="${cat.color}" font-family="'Press Start 2P'" font-size="${tf}" paint-order="stroke" stroke="#06050f" stroke-width="4">${esc(cat.name)}</text>`
        + `<text id="jc-${cat.id}" x="${t.x.toFixed(1)}" y="${(t.y + (t.grid ? 14 : 17)).toFixed(1)}" text-anchor="${t.anchor}" fill="#8b88a8" font-family="'VT323'" font-size="${cf}" paint-order="stroke" stroke="#06050f" stroke-width="3">${done}/${cat.ch.length} SOLVED${hint}</text>`
      out += `<g id="jeop-${cat.id}">${lines}${stars}${labels}${title}</g>`
    })
    host.innerHTML = out
  }

  function renderChallenge(c: any) {
    const g = $('ch-' + c.cat + '-' + c.i); if (g) g.innerHTML = drawChallenge(c)
    const v = $('chv-' + c.cat + '-' + c.i); if (v) v.textContent = c.base + 'pt'
    const done = c.catObj.ch.filter((x: any) => x.solvers.length > 0).length
    const jc = $('jc-' + c.cat); if (jc) jc.textContent = done + '/' + c.catObj.ch.length + ' SOLVED'
  }

  function flashChallenge(c: any) {
    if (usePixi() && deps.onFlash) { deps.onFlash(c.x, c.y, c.r, c.catObj.color); return } // GPU draws the flash ring
    const g = $('ch-' + c.cat + '-' + c.i); if (!g) return
    const ring = document.createElementNS(NS, 'circle')
    ring.setAttribute('cx', c.x); ring.setAttribute('cy', c.y); ring.setAttribute('r', c.r)
    ring.setAttribute('fill', 'none'); ring.setAttribute('stroke', c.catObj.color); ring.setAttribute('stroke-width', '2')
    g.appendChild(ring)
    const t0 = performance.now(), DUR = 620
    const step = (now: number) => {
      const t = (now - t0) / DUR
      if (_jeopKilled || t >= 1 || !ring.parentNode) { if (ring.parentNode) ring.parentNode.removeChild(ring); return }
      ring.setAttribute('r', (c.r + 30 * t).toFixed(1)); ring.setAttribute('opacity', (1 - t).toFixed(2))
      requestAnimationFrame(step)
    }
    requestAnimationFrame(step)
  }

  // laser from a team node (wheel coords 0..1000) to the challenge star (panel px)
  function jeopBeam(tx: number, ty: number, c: any, col: string) {
    if (!jeopReady || deps.isFrozen()) return
    if (usePixi() && deps.onBeam) { deps.onBeam(tx, ty, c.x, c.y, c.r, col); return } // GPU draws the laser
    const host = $('jeop'); if (!host) return
    const px = jWheelX + (tx / 1000) * jWheelSize, py = jWheelY + (ty / 1000) * jWheelSize
    const g = document.createElementNS(NS, 'g'); host.appendChild(g)
    const ln = (w: number, stroke: string) => {
      const l = document.createElementNS(NS, 'line')
      l.setAttribute('x1', px.toFixed(1)); l.setAttribute('y1', py.toFixed(1))
      l.setAttribute('x2', c.x.toFixed(1)); l.setAttribute('y2', c.y.toFixed(1))
      l.setAttribute('stroke', stroke); l.setAttribute('stroke-width', String(w)); l.setAttribute('stroke-linecap', 'round')
      g.appendChild(l); return l
    }
    const glow = ln(9, col), core = ln(4, col), hot = ln(1.7, '#ffffff')
    const muzzle = document.createElementNS(NS, 'circle')
    muzzle.setAttribute('cx', px.toFixed(1)); muzzle.setAttribute('cy', py.toFixed(1)); muzzle.setAttribute('fill', '#fff'); g.appendChild(muzzle)
    const pulse = document.createElementNS(NS, 'circle')
    pulse.setAttribute('r', '4.5'); pulse.setAttribute('fill', '#fff'); g.appendChild(pulse)
    const ring = document.createElementNS(NS, 'circle')
    ring.setAttribute('cx', c.x.toFixed(1)); ring.setAttribute('cy', c.y.toFixed(1))
    ring.setAttribute('fill', 'none'); ring.setAttribute('stroke', col); ring.setAttribute('stroke-width', '2.6'); g.appendChild(ring)
    const t0 = performance.now(), DUR = 640
    const step = (now: number) => {
      const t = (now - t0) / DUR
      if (_jeopKilled || t >= 1 || !g.parentNode) { if (g.parentNode) g.parentNode.removeChild(g); return }
      const fade = t < 0.12 ? t / 0.12 : t > 0.6 ? Math.max(0, 1 - (t - 0.6) / 0.4) : 1
      glow.setAttribute('opacity', (0.32 * fade).toFixed(3))
      core.setAttribute('opacity', (0.95 * fade).toFixed(3))
      hot.setAttribute('opacity', fade.toFixed(3))
      const mz = t < 0.3 ? t / 0.3 : 1
      muzzle.setAttribute('r', (3 + 11 * Math.sin(mz * Math.PI)).toFixed(1)); muzzle.setAttribute('opacity', (0.9 * (1 - mz)).toFixed(2))
      const pt = Math.min(1, t / 0.4)
      pulse.setAttribute('cx', (px + (c.x - px) * pt).toFixed(1)); pulse.setAttribute('cy', (py + (c.y - py) * pt).toFixed(1)); pulse.setAttribute('opacity', pt < 1 ? '0.95' : '0')
      const rt = t > 0.34 ? (t - 0.34) / 0.66 : 0
      ring.setAttribute('r', (c.r + rt * 26).toFixed(1)); ring.setAttribute('opacity', (rt > 0 ? Math.max(0, 1 - rt) : 0).toFixed(2))
      requestAnimationFrame(step)
    }
    requestAnimationFrame(step)
  }

  function jtipHTML(c: any) {
    const RANK = ['1ST', '2ND', '3RD'], RC = ['#ffd54a', '#cfd8e6', '#cd8b54']
    let rows = ''
    if (c.solvers.length) c.solvers.forEach((tm: any, k: number) => {
      rows += `<div class="jt-row"><span class="jt-dot" style="background:${tm.color};color:${tm.color}"></span>${esc(tm.name) || '—'}` + (RANK[k] ? `<span class="jt-rank" style="color:${RC[k]}">${RANK[k]}</span>` : '') + `</div>`
    })
    else rows = `<div class="jt-none">// unsolved</div>`
    return `<div class="jt-name" style="color:${c.catObj.color}">${esc(c.name)}</div>`
      + `<div class="jt-meta">${esc(c.catObj.name)} · ${c.base}pt · ${c.solveCount} solve${c.solveCount === 1 ? '' : 's'}</div>` + rows
  }

  function markSolved(c: any, team: { name: string; color: string }) {
    if (!c.solvers.find((s: any) => s.color === team.color)) { c.solvers.push({ name: team.name || '', color: team.color }); c.solveCount = Math.max(c.solveCount, c.solvers.length) }
    renderChallenge(c)
  }

  let _docClick: any = null
  function initHover() {
    const host = $('jeop'), tip = $('jtip'), wrap = qs('.arena-wrap')
    if (!host || !tip || !wrap || host._hoverInit) return
    host._hoverInit = true
    let pinned = false
    const find = (t: any) => { if (!t || !t.getAttribute) return null; const cc = t.getAttribute('data-cat'); if (cc == null) return null; return CHALLENGES.find((c) => c.cat === cc && c.i === +t.getAttribute('data-i')) }
    const showTip = (c: any) => {
      tip.innerHTML = jtipHTML(c); tip.classList.add('show')
      const tw = tip.offsetWidth, th = tip.offsetHeight, W = wrap.clientWidth, H = wrap.clientHeight
      let lx = c.side === 'L' ? c.x + 18 : c.x - 18 - tw
      let ty = c.y - th - 12; if (ty < 6) ty = c.y + 16
      lx = Math.max(6, Math.min(lx, W - tw - 6)); ty = Math.max(6, Math.min(ty, H - th - 6))
      tip.style.left = lx.toFixed(0) + 'px'; tip.style.top = ty.toFixed(0) + 'px'
    }
    host.addEventListener('mouseover', (e: any) => { if (pinned) return; const c = find(e.target); if (c) showTip(c) })
    host.addEventListener('mouseout', (e: any) => { if (!pinned && find(e.target)) tip.classList.remove('show') })
    host.addEventListener('click', (e: any) => { const c = find(e.target); if (c) { showTip(c); pinned = true; e.stopPropagation() } else { pinned = false; tip.classList.remove('show') } })
    _docClick = () => { if (pinned) { pinned = false; tip.classList.remove('show') } }
    document.addEventListener('click', _docClick)
  }

  // signature of the challenge structure (ids) — when unchanged, a poll only needs to
  // refresh point values + solvers in place (no full relayout/redraw → no 15s flicker,
  // no wiped in-flight beams).
  let _sig = ''
  const sigOf = (cats: any[]) => cats.map((c) => c.id + ':' + c.challenges.map((x: any) => x.id).join(',')).join('|')

  return {
    hasData: () => CATEGORIES.length > 0,
    setData(cats: JeopCategory[]) {
      cats = cats || []
      const sig = sigOf(cats)
      if (sig === _sig && jeopReady && sig !== '') {
        // same challenges — update value + solve state in place (laid-out stars for the
        // immediate redraw, and the source CATEGORIES so a later relayout stays fresh)
        const byId: any = {}; CHALLENGES.forEach((c) => (byId[c.id] = c))
        const srcById: any = {}; CATEGORIES.forEach((cat) => cat.challenges.forEach((x: any) => (srcById[x.id] = x)))
        const solversSig = (arr: any[]) => (arr ? arr.map((s: any) => s.color).join(',') : '')
        cats.forEach((cat) => cat.challenges.forEach((nc) => {
          const c = byId[nc.id]
          if (c) {
            // only re-render the SVG when something visible actually changed (most 15s polls change nothing);
            // ALWAYS update the in-place values so a later relayout stays fresh. signature BEFORE overwrite.
            const changed = c.base !== nc.base || c.solveCount !== nc.solveCount || solversSig(c.solvers) !== solversSig(nc.solvers)
            c.base = nc.base; c.solveCount = nc.solveCount; c.solvers = nc.solvers
            if (changed) renderChallenge(c)
          }
          const sc = srcById[nc.id]
          if (sc) { sc.base = nc.base; sc.solveCount = nc.solveCount; sc.solvers = nc.solvers }
        }))
        return
      }
      CATEGORIES = cats; _sig = sig; layout(); render()
    },
    layout() { layout(); render() },
    /** Pixi star layer just became ready: hand it the laid-out stars and re-render the SVG to
     *  hit-only (drawChallenge now drops the animated visuals). No-op until the first layout. */
    syncPixi() { if (!jeopReady) return; deps.onStars?.(CATEGORIES, jDense); render() },
    /** fire a solve beam + flash from a team node to a challenge by title (live feed) */
    solveByTitle(wheelX: number, wheelY: number, title: string, team: { name: string; color: string }): boolean {
      if (!jeopReady) return false
      const t = String(title || '').toLowerCase()
      const c = CHALLENGES.find((x) => String(x.name).toLowerCase() === t); if (!c) return false
      jeopBeam(wheelX, wheelY, c, team.color); flashChallenge(c); markSolved(c, team)
      return true
    },
    /** preview/sim: solve a random not-yet-solved-by-this-team challenge; returns it */
    solveRandom(wheelX: number, wheelY: number, team: { name: string; color: string }): { name: string; base: number } | null {
      if (!jeopReady) return null
      const open = CHALLENGES.filter((c) => c.solveCount < 8 && !c.solvers.find((s: any) => s.color === team.color))
      if (!open.length) return null
      const c = open[Math.floor(Math.random() * open.length)]
      jeopBeam(wheelX, wheelY, c, team.color); flashChallenge(c); markSolved(c, team)
      return { name: c.name, base: c.base }
    },
    initHover,
    destroy() { _jeopKilled = true; if (_docClick) { document.removeEventListener('click', _docClick); _docClick = null } },
  }
}
