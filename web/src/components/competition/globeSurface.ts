import { projectSpherePoint } from './model'

// Deterministic geometry, independent of challenge data and viewer identity.
const particles = Array.from({ length: 700 }, (_, index) => {
  const y = 1 - (index / 699) * 2
  const r = Math.sqrt(1 - y * y)
  const angle = index * Math.PI * (3 - Math.sqrt(5))
  return [Math.cos(angle) * r, y, Math.sin(angle) * r] as const
})

export function drawGlobeSurface(element: HTMLCanvasElement, yaw: number, pitch: number, dark: boolean) {
  const context = element.getContext('2d')
  if (!context) return
  // Fixed, bounded bitmap. No perpetual animation, timers, network or WebGL context.
  const size = 800
  const radius = 348
  // Keep the larger stage crisp with one bounded, 1200px bitmap at every viewport.
  element.width = size * 1.5
  element.height = size * 1.5
  context.scale(1.5, 1.5)
  context.clearRect(0, 0, size, size)
  const color = dark ? '115,166,225' : '40,87,147'
  const gradient = context.createRadialGradient(325, 280, 0, 400, 400, radius)
  gradient.addColorStop(0, dark ? '#15273d' : '#eaf2fc')
  gradient.addColorStop(0.85, dark ? '#081522' : '#dce8f6')
  gradient.addColorStop(0.985, dark ? '#142b45' : '#b3cbe9')
  gradient.addColorStop(1, dark ? '#6cafd0' : '#85add4')
  context.fillStyle = gradient
  context.beginPath()
  context.arc(400, 400, radius, 0, Math.PI * 2)
  context.fill()
  const drawLine = (points: readonly (readonly [number, number, number])[]) => {
    context.beginPath()
    let connected = false
    points.forEach(([x, y, z]) => {
      const point = projectSpherePoint(x, y, z, yaw, pitch)
      if (point.z < 0) {
        connected = false
        return
      }
      const px = 400 + point.x * radius
      const py = 400 + point.y * radius
      if (!connected) context.moveTo(px, py)
      else context.lineTo(px, py)
      connected = true
    })
    context.stroke()
  }
  context.strokeStyle = `rgba(${color},${dark ? 0.2 : 0.26})`
  context.lineWidth = 0.8
  for (let ring = 0; ring < 18; ring++) {
    const longitude = (ring * Math.PI) / 18
    drawLine(
      Array.from({ length: 97 }, (_, i) => {
        const a = (i * Math.PI * 2) / 96
        return [Math.cos(a) * Math.cos(longitude), Math.sin(a), Math.cos(a) * Math.sin(longitude)] as const
      })
    )
  }
  for (let ring = -4; ring <= 4; ring++) {
    const y = ring / 5
    const r = Math.sqrt(1 - y * y)
    drawLine(
      Array.from({ length: 97 }, (_, i) => {
        const a = (i * Math.PI * 2) / 96
        return [Math.cos(a) * r, y, Math.sin(a) * r] as const
      })
    )
  }
  for (const [x, y, z] of particles) {
    const point = projectSpherePoint(x, y, z, yaw, pitch)
    if (point.z < 0) continue
    context.fillStyle = `rgba(${color},0.72)`
    context.beginPath()
    context.arc(400 + point.x * radius, 400 + point.y * radius, 1.5, 0, Math.PI * 2)
    context.fill()
  }
}
