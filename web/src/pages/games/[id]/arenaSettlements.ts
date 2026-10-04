import type { CountryGeometry } from './arenaGeography'
import { projectGlobe, type GlobePoint } from './arenaGlobeModel'

const NS = 'http://www.w3.org/2000/svg'
const raised = (p: GlobePoint, height: number) => ({
  x: p.x * (1 + height),
  y: p.y * (1 + height),
  z: p.z * (1 + height),
})
const middle = (a: GlobePoint, b: GlobePoint) => {
  const n = Math.hypot(a.x + b.x, a.y + b.y, a.z + b.z)
  return { x: (a.x + b.x) / n, y: (a.y + b.y) / n, z: (a.z + b.z) / n }
}

/** Bounded decorative scene: real spherical foundations and radial extrusion,
 * rather than screen-facing city stickers. No listeners, animation loop or IO.
 */
export function createArenaSettlements(surface: SVGSVGElement) {
  const layer = document.createElementNS(NS, 'g')
  layer.classList.add('settlements')
  layer.setAttribute('aria-hidden', 'true')
  surface.append(layer)
  const slots = Array.from({ length: 24 }, () => {
    const group = document.createElementNS(NS, 'g')
    group.classList.add('settlement')
    group.style.display = 'none'
    const paths = Array.from({ length: 5 }, () =>
      ['wall', 'shade', 'roof', 'window'].map((kind) => {
        const path = document.createElementNS(NS, 'path')
        path.classList.add(`building-${kind}`)
        group.append(path)
        return path
      })
    )
    layer.append(group)
    return { group, paths }
  })
  return (
    countries: readonly CountryGeometry[],
    yaw: number,
    pitch: number,
    width: number,
    selected: number | null
  ) => {
    const visible = countries
      .map((country) => ({ country, p: projectGlobe(country.location, yaw, pitch) }))
      .filter(({ country, p }) => p.z > 0.18 && country.settlement.buildings[0]?.height * width > 1.8)
      .sort((a, b) => Number(b.country.id === selected) - Number(a.country.id === selected) || b.p.z - a.p.z)
      .slice(0, slots.length)
      .sort((a, b) => a.p.z - b.p.z)
    layer.dataset.visibleSettlements = String(visible.length)
    slots.forEach(({ group, paths }, i) => {
      const item = visible[i]
      group.style.display = item ? '' : 'none'
      if (!item) return
      const { country } = item
      group.dataset.style = country.settlement.style
      group.dataset.country = String(country.id)
      // Paint complete buildings from far to near, so a rear building's windows
      // cannot appear through a nearer wall just because they share a material.
      const buildings = country.settlement.buildings.toSorted(
        (a, b) =>
          projectGlobe(middle(a.base[0], a.base[2]), yaw, pitch).z -
          projectGlobe(middle(b.base[0], b.base[2]), yaw, pitch).z
      )
      paths.forEach((materials, index) => {
        const building = buildings[index]
        if (!building) {
          materials.forEach((p) => p.setAttribute('d', ''))
          return
        }
        const faces: { points: GlobePoint[]; kind: number }[] = []
        const base = building.base,
          top = base.map((p) => raised(p, building.height))
        for (let side = 0; side < 4; side++) {
          const next = (side + 1) % 4
          faces.push({ points: [base[side], base[next], top[next], top[side]], kind: side % 2 })
          // A short window band is inset from each wall's edges.
          const a = middle(base[side], base[next])
          const left = middle(base[side], a),
            right = middle(a, base[next])
          faces.push({
            points: [
              raised(left, building.height * 0.5),
              raised(right, building.height * 0.5),
              raised(right, building.height * 0.64),
              raised(left, building.height * 0.64),
            ],
            kind: 3,
          })
        }
        if (building.pitched) {
          const left = raised(middle(base[0], base[3]), building.height * 1.6)
          const right = raised(middle(base[1], base[2]), building.height * 1.6)
          faces.push(
            { points: [top[0], top[1], right, left], kind: 2 },
            { points: [top[2], top[3], left, right], kind: 2 },
            { points: [top[1], top[2], right], kind: 0 },
            { points: [top[3], top[0], left], kind: 1 }
          )
        } else faces.push({ points: top, kind: 2 })
        const painted = faces
          .map((face) => ({ ...face, projected: face.points.map((p) => projectGlobe(p, yaw, pitch)) }))
          .filter(
            ({ projected: p }) =>
              p.every((v) => v.z > 0) &&
              (p[1].x - p[0].x) * (p[2].y - p[0].y) - (p[1].y - p[0].y) * (p[2].x - p[0].x) > 0
          )
        // Four paths per building; at most 24 settlements / 120 buildings.
        const commands = ['', '', '', '']
        for (const face of painted)
          commands[face.kind] +=
            face.projected.map((p, j) => `${j ? 'L' : 'M'}${p.x.toFixed(2)},${p.y.toFixed(2)}`).join('') + 'Z'
        materials.forEach((path, j) => path.setAttribute('d', commands[j]))
      })
    })
  }
}
