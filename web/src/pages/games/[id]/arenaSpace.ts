/**
 * Deep-space backdrop behind the conquest globe. Two deterministic star layers
 * shift with the camera yaw at different depths, and a faint nebula gives the
 * scene distance. Everything here is decorative: it never carries game state,
 * repaints only on size/theme changes, and moves only by a cheap transform.
 */
export interface SpaceStar {
  x: number
  y: number
  r: number
  a: number
}

/** Deterministic star positions in the unit square (same seed → same sky). */
export function starField(seed: number, count: number): SpaceStar[] {
  let state = seed >>> 0 || 1
  const next = () => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0
    return state / 4294967296
  }
  const stars: SpaceStar[] = []
  for (let i = 0; i < count; i++) {
    const x = next()
    const y = next()
    const size = next()
    stars.push({ x, y, r: 0.6 + size * size * 1.6, a: 0.35 + next() * 0.65 })
  }
  return stars
}

/** Bounded parallax shift: the sky slides with the camera but never runs out. */
export function parallaxOffset(yaw: number, depth: number): { x: number; y: number } {
  const safeYaw = Number.isFinite(yaw) ? yaw : 0
  return { x: Math.round(Math.sin(safeYaw) * depth * 100) / 100, y: Math.round(Math.cos(safeYaw) * depth * 25) / 100 }
}

const LAYERS = [
  { className: 'space-far', seed: 7, count: 170, depth: 9, tint: '#c9dcf0' },
  { className: 'space-near', seed: 31, count: 55, depth: 24, tint: '#f3f7ff' },
] as const

export function createArenaSpace(container: HTMLElement) {
  const nebula = document.createElement('div')
  nebula.className = 'space-nebula'
  nebula.setAttribute('aria-hidden', 'true')
  const canvases = LAYERS.map((layer) => {
    const canvas = document.createElement('canvas')
    canvas.className = layer.className
    canvas.setAttribute('aria-hidden', 'true')
    return canvas
  })
  container.prepend(nebula, ...canvases)
  let key = ''
  let shift = ''

  return {
    /** Repaint the star layers for the current theme and container size. */
    paint(dark: boolean) {
      const width = Math.max(1, Math.round(container.clientWidth * 1.2))
      const height = Math.max(1, Math.round(container.clientHeight * 1.2))
      const next = `${dark}:${width}:${height}`
      if (next === key) return
      key = next
      canvases.forEach((canvas, index) => {
        canvas.width = width
        canvas.height = height
        const ctx = canvas.getContext('2d')
        if (!ctx) return
        ctx.clearRect(0, 0, width, height)
        if (!dark) return
        const layer = LAYERS[index]
        ctx.fillStyle = layer.tint
        for (const star of starField(layer.seed, layer.count)) {
          ctx.globalAlpha = star.a
          ctx.beginPath()
          ctx.arc(star.x * width, star.y * height, star.r, 0, Math.PI * 2)
          ctx.fill()
        }
        ctx.globalAlpha = 1
      })
    },
    /** Slide each layer by its depth; skipped when the camera has not moved. */
    parallax(yaw: number) {
      const next = canvases.map((_, index) => parallaxOffset(yaw, LAYERS[index].depth))
      const signature = next.map((o) => `${o.x},${o.y}`).join('|')
      if (signature === shift) return
      shift = signature
      canvases.forEach((canvas, index) => {
        canvas.style.transform = `translate3d(${next[index].x}px, ${next[index].y}px, 0)`
      })
    },
    destroy() {
      nebula.remove()
      for (const canvas of canvases) canvas.remove()
    },
  }
}
