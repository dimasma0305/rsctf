import assert from 'node:assert/strict'
import test from 'node:test'
import { drawGlobeSurface } from '../components/competition/globeSurface'

test('globe surface reuses its bounded bitmap and batches geometry without accumulating transforms', () => {
  let width = 0,
    height = 0,
    resizes = 0,
    fills = 0,
    strokes = 0
  const transforms: number[][] = []
  const context = {
    setTransform: (...values: number[]) => transforms.push(values),
    clearRect() {},
    createRadialGradient: () => ({ addColorStop() {} }),
    beginPath() {},
    arc() {},
    moveTo() {},
    lineTo() {},
    fill() {
      fills++
    },
    stroke() {
      strokes++
    },
  }
  const canvas = {
    get width() {
      return width
    },
    set width(value: number) {
      width = value
      resizes++
    },
    get height() {
      return height
    },
    set height(value: number) {
      height = value
      resizes++
    },
    getContext: () => context,
  } as unknown as HTMLCanvasElement
  for (let i = 0; i < 120; i++) drawGlobeSurface(canvas, i / 60, 0.1, i < 60)
  assert.equal(resizes, 2, 'bitmap storage is allocated once, not on every frame')
  assert.equal(width, 1200)
  assert.equal(height, 1200)
  assert.equal(fills, 240)
  assert.equal(strokes, 120)
  assert.equal(transforms.length, 120)
  assert.ok(transforms.every((t) => JSON.stringify(t) === '[1.5,0,0,1.5,0,0]'))
  drawGlobeSurface(canvas, 0, 0, false, 200)
  assert.equal(width, 400)
  drawGlobeSurface(canvas, 0, 0, false, 4000)
  assert.equal(width, 1200)
  drawGlobeSurface(canvas, 0, 0, false, Number.NaN)
  assert.equal(width, 1200)
})
