/** Static ocean/sky backdrop. Repaint only on size or theme changes, not rotation. */
export function createArenaOcean(canvas: HTMLCanvasElement) {
  let key = ''
  return (dark: boolean, pixels: number) => {
    const bitmap = Math.max(400, Math.min(1200, Math.ceil(pixels)))
    const next = `${dark}:${bitmap}`
    if (next === key) return
    key = next
    canvas.width = canvas.height = bitmap
    const ctx = canvas.getContext('2d')
    if (!ctx) return
    ctx.setTransform(bitmap / 1000, 0, 0, bitmap / 1000, 0, 0)
    ctx.clearRect(0, 0, 1000, 1000)
    // Quiet static stars stay outside the world and disappear in the light theme.
    if (dark)
      for (let i = 0; i < 100; i++) {
        const x = (i * 619 + 71) % 997,
          y = (i * i * 137 + 113) % 991
        if (Math.hypot(x - 500, y - 500) < 460) continue
        ctx.fillStyle = i % 3 ? '#aecce94d' : '#d9e8f080'
        ctx.beginPath()
        ctx.arc(x, y, i % 3 ? 0.8 : 1.2, 0, Math.PI * 2)
        ctx.fill()
      }
    const atmosphere = ctx.createRadialGradient(500, 500, 425, 500, 500, 472)
    atmosphere.addColorStop(0, '#8cdbea00')
    atmosphere.addColorStop(0.25, dark ? '#76bed851' : '#4a99b12e')
    atmosphere.addColorStop(1, '#5aaaca00')
    ctx.fillStyle = atmosphere
    ctx.beginPath()
    ctx.arc(500, 500, 472, 0, Math.PI * 2)
    ctx.fill()
    const sea = ctx.createRadialGradient(340, 280, 30, 500, 500, 435)
    sea.addColorStop(0, dark ? '#386c7c' : '#709aa1')
    sea.addColorStop(0.55, dark ? '#245460' : '#4e7c8b')
    sea.addColorStop(0.88, '#163947')
    sea.addColorStop(0.98, '#102b39')
    sea.addColorStop(1, '#7eaebb')
    ctx.fillStyle = sea
    ctx.beginPath()
    ctx.arc(500, 500, 435, 0, Math.PI * 2)
    ctx.fill()
  }
}

/** Lighting is under labels/borders, so neither their contrast nor hit targets change. */
export function addArenaLighting(surface: SVGSVGElement) {
  const ns = 'http://www.w3.org/2000/svg'
  const defs = document.createElementNS(ns, 'defs')
  const gradient = document.createElementNS(ns, 'radialGradient')
  gradient.id = 'arenaSunlight'
  for (const [name, value] of Object.entries({ cx: '34%', cy: '27%', r: '76%' })) gradient.setAttribute(name, value)
  for (const [offset, color] of [
    ['0%', '#fff3c52b'],
    ['48%', '#172f2800'],
    ['84%', '#0717214d'],
    ['100%', '#030e19c9'],
  ]) {
    const stop = document.createElementNS(ns, 'stop')
    stop.setAttribute('offset', offset)
    stop.setAttribute('stop-color', color)
    gradient.append(stop)
  }
  const clip = document.createElementNS(ns, 'clipPath')
  clip.id = 'arenaOceanClip'
  const globe = document.createElementNS(ns, 'circle')
  for (const [name, value] of Object.entries({ cx: '500', cy: '500', r: '435' })) globe.setAttribute(name, value)
  clip.append(globe.cloneNode())
  defs.append(gradient, clip)
  globe.classList.add('world-lighting')
  globe.setAttribute('fill', 'url(#arenaSunlight)')
  surface.append(defs, globe)
}
