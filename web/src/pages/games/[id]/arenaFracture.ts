type Point = readonly [number, number]

// Each spoke runs from the impact site to an edge of the virtual pane.
const spokes: Point[][] = [
  [[566, 264], [550, 247], [526, 219], [502, 197], [451, 146], [416, 119], [340, 54], [262, -12]],
  [[568, 260], [561, 233], [565, 217], [550, 163], [553, 127], [529, 72], [542, -15]],
  [[573, 262], [579, 241], [585, 219], [596, 192], [612, 155], [636, 104], [652, 76], [697, -12]],
  [[577, 265], [604, 249], [621, 235], [647, 230], [689, 193], [722, 187], [782, 145], [836, 136], [937, 59], [1020, 38]],
  [[578, 268], [605, 266], [630, 263], [655, 264], [708, 260], [752, 253], [805, 250], [877, 241], [932, 241], [1010, 237]],
  [[577, 272], [603, 285], [621, 286], [647, 310], [686, 324], [732, 361], [776, 369], [853, 425], [901, 435], [1015, 527]],
  [[574, 275], [582, 301], [599, 324], [602, 352], [633, 402], [631, 433], [651, 468], [661, 529], [706, 612], [709, 667]],
  [[570, 276], [562, 306], [544, 324], [538, 355], [509, 391], [491, 451], [448, 494], [412, 587], [357, 663]],
  [[565, 273], [538, 290], [519, 292], [484, 317], [445, 323], [396, 354], [354, 358], [286, 395], [245, 397], [134, 452], [-12, 495]],
  [[563, 268], [532, 267], [512, 262], [481, 258], [438, 247], [396, 242], [342, 231], [279, 224], [208, 210], [123, 202], [-10, 180]],
]
const impact: Point = [570, 268]
const cornerAfter: Partial<Record<number, Point>> = {
  2: [1020, -20], 5: [1020, 670], 7: [-20, 670], 9: [-20, -20],
}
const outline = (points: Point[]) => 'M' + points.map(([x, y]) => `${x} ${y}`).join(' ')

// Each cross crack stops at the adjacent radial fracture. Different cut points
// on the other side of a spoke produce T junctions and long asymmetric slivers.
const crossCuts: Point[][] = [
  [[1, 2], [4, 3]], [[2, 1], [3, 3], [5, 4]], [[1, 2], [3, 5]],
  [[2, 1], [4, 3], [7, 5]], [[1, 2], [3, 4]], [[2, 1], [5, 3], [7, 6]],
  [[1, 2], [3, 4]], [[2, 1], [4, 3], [6, 7]], [[1, 2], [3, 5]], [[2, 1], [5, 4]],
]

// Orthographic projection of a thin rotating sheet. Its face reflects more
// light at grazing angles and briefly narrows to a bright edge as it turns.
// Sample a few reusable orientations once; CSS plays them without a frame loop.
const rotations = [
  [1, .42, .22, 215], [-.55, 1, .12, 155], [.22, 1, -.31, -193],
  [1, -.7, .35, -128], [-.8, .34, .18, 274], [.4, .72, -.54, 112],
]
const round = (value: number) => Number(value.toFixed(3))
export const fractureMotionCss = rotations.map(([x, y, z, degrees], index) => {
  const length = Math.hypot(x, y, z)
  const [a, b, c] = [x / length, y / length, z / length]
  const frames = Array.from({ length: 17 }, (_, step) => {
    const t = step / 16
    const angle = degrees * t * Math.PI / 180
    const cos = Math.cos(angle), sin = Math.sin(angle), k = 1 - cos
    const matrix = [cos + a*a*k, a*b*k + c*sin, a*b*k - c*sin, cos + b*b*k].map(round)
    const grazing = 1 - Math.abs(cos + c*c*k)
    const reflection = .09 + .86 * grazing ** 2
    return `${t * 100}%{transform:matrix(${matrix.join(',')},0,0);fill-opacity:${round(reflection)}}`
  })
  return `@keyframes glass-turn-${index}{${frames.join('')}}`
}).join('')

const seams: string[] = []
const forkedSectors = new Set([0, 3, 5, 8])

function shard(points: Point[], edge: Point[], index: number, release: number) {
  const centerX = points.reduce((sum, [x]) => sum + x, 0) / points.length
  const drift = (centerX - impact[0]) * .31 + (index * 17 % 43) - 21
  const style = `--release:${release}ms;--drift:${round(drift)}px;--kick:${-70 - index * 11 % 100}px;--turn:glass-turn-${index % rotations.length}`
  return `<g class="glass-shard" style="${style}"><g class="glass-face">
    <path class="glass-body" d="${outline(points)}Z"/><path class="glass-bevel" d="${outline(edge)}"/>
  </g></g>`
}

const shards = spokes.map((left, sector) => {
  const right = spokes[(sector + 1) % spokes.length]
  const corner = cornerAfter[sector]
  const cuts: Point[] = [[0, 0], ...crossCuts[sector]]
  let startLeft = 0, startRight = 0
  return [...cuts, [left.length - 1, right.length - 1]].map(([endLeft, endRight], ring) => {
    const outside = ring === cuts.length
    const edge = left.slice(startLeft, endLeft + 1)
    const opposite = right.slice(startRight, endRight + 1).reverse()
    if (!outside) seams.push(outline([left[endLeft], right[endRight]]))
    startLeft = endLeft
    startRight = endRight
    const index = sector * 5 + ring
    const release = 590 + ring * 94 + (sector * 67 % 235)
    if (outside && corner) opposite.unshift(corner)
    if (outside && forkedSectors.has(sector)) {
      // A secondary radial crack forks towards the frame. Its two pieces
      // share the exact same broken edge when they peel apart.
      const split = Math.floor((edge.length - 1) / 2)
      const pivot = edge[split]
      const tip = edge[edge.length - 1]
      const end: Point = [tip[0] * .6 + opposite[0][0] * .4, tip[1] * .6 + opposite[0][1] * .4]
      const bend: Point = [(pivot[0] + end[0]) / 2 + 4, (pivot[1] + end[1]) / 2 - 3]
      seams.push(outline([pivot, bend, end]))
      return shard([...edge.slice(0, split + 1), bend, end, ...opposite], [pivot, bend, end], index, release)
        + shard([...edge.slice(split), end, bend], edge.slice(split), index + 11, release + 73)
    }
    return shard([...(ring ? [] : [impact]), ...edge, ...opposite], edge, index, release)
  }).join('')
}).join('')

export const fractureGlass = `<svg class="fracture-glass" viewBox="0 0 1000 650" preserveAspectRatio="xMidYMid slice" aria-hidden="true" focusable="false">
  <defs>
    <linearGradient id="shard-face" x1="0" y1=".9" x2=".6" y2="0">
      <stop stop-color="#1c3039"/><stop offset=".48" stop-color="#88a5ac"/>
      <stop offset=".77" stop-color="#f3f8f5"/><stop offset="1" stop-color="#c0d9d8"/>
    </linearGradient>
  </defs>
  <g class="glass-pieces">${shards}</g>
  <g class="intact-cracks">
    <g class="fracture-rays" fill="none">${spokes.map(points => `<path pathLength="1" d="${outline(points)}"/>`).join('')}</g>
    <path class="fracture-branches" fill="none" d="${seams.join(' ')}"/>
    <path class="fracture-spall" fill="none" d="m550 247 3 9 6 2 4 9 m-29 8 14 -4 7 2 12 -3 m-22 20 11 -9 5 -10 m-1 29 6 -17 5 -10 m2 8 6 9 1 11 m-3 -28 13 9 13 1 m-26 -15 14 -5 9 1 14 -6 m-42 5 7 -10 4 -17 m-19 24 -6 -9 -3 -12"/>
    <path class="spall-glint" d="m562 264 3 -5 -1 4z m5 -1 4 -6 -2 5z m7 3 6 -2 -3 2z m-1 6 -4 5 2 -5z m-7 -3 -7 3 4 -3z"/>
  </g>
</svg>`
