import assert from 'node:assert/strict'
import test from 'node:test'
import { arenaHome, createArenaCamera } from '../pages/games/[id]/arenaCamera'

const distance = (a: number, b: number) => Math.abs(Math.atan2(Math.sin(a - b), Math.cos(a - b)))
const settle = (camera: ReturnType<typeof createArenaCamera>) => {
  for (let i = 0; i < 30; i++) camera.step(1 / 60, false)
}
test('arena camera focus is gradual, takes the shortest route and finishes precisely', () => {
  const camera = createArenaCamera()
  camera.set({ yaw: Math.PI * 2 - 0.1, pitch: 0 })
  camera.focus({ yaw: 0.1, pitch: 0.2 }, true)
  assert.ok(camera.moving)
  assert.ok(distance(camera.current.yaw, Math.PI * 2 - 0.1) < 1e-9, 'selection does not snap')
  camera.step(1 / 60, false)
  assert.ok(distance(camera.current.yaw, Math.PI * 2 - 0.1) < 0.03, 'first frame follows the short arc')
  settle(camera)
  assert.equal(camera.moving, false)
  assert.ok(distance(camera.current.yaw, 0.1) < 1e-9)
  assert.ok(distance(camera.current.pitch, 0.2) < 1e-9)
})
test('latest camera selection wins and repeated arrow inputs accumulate', () => {
  const camera = createArenaCamera()
  camera.focus({ yaw: 3, pitch: 1 }, true)
  camera.step(0.05, false)
  const intermediate = camera.current
  camera.focus({ yaw: 1, pitch: 0 }, true)
  assert.deepEqual(camera.current, intermediate)
  settle(camera)
  assert.ok(distance(camera.current.yaw, 1) < 1e-9)
  camera.nudge(0.25, 0, true)
  camera.nudge(0.25, 0, true)
  settle(camera)
  assert.ok(distance(camera.current.yaw, 1.5) < 1e-9)
})
test('manual drag interrupts focus; reduced motion settles without residual movement', () => {
  const camera = createArenaCamera()
  camera.focus({ yaw: 3, pitch: 1 }, true)
  camera.set(arenaHome)
  assert.equal(camera.step(0.02, false), false)
  assert.ok(distance(camera.current.yaw, arenaHome.yaw) < 1e-9)
  camera.focus({ yaw: 2, pitch: 1 }, false)
  assert.equal(camera.moving, false)
  assert.equal(camera.current.yaw, 2)
  camera.focus({ yaw: 1, pitch: 2 }, true)
  camera.settle()
  assert.deepEqual(camera.current, { yaw: 1, pitch: 2 })
  assert.equal(camera.step(1, false), false)
})
test('arena camera advances at display cadence and bounds suspended-tab catch-up', () => {
  const camera = createArenaCamera()
  for (let i = 0; i < 60; i++) assert.equal(camera.step(1 / 60, true), true)
  assert.ok(distance(camera.current.yaw, arenaHome.yaw + 0.075) < 1e-9)
  const before = camera.current.yaw
  camera.step(60, true)
  assert.ok(distance(camera.current.yaw, before) <= 0.003751)
})
