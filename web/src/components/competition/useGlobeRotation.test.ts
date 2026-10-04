import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { installTestDom } from '../../test/installDom'
import { focusHorizonNode, projectHorizonNode } from './model'
import { useGlobeRotation } from './useGlobeRotation'

test('globe focus is finite, interruptible, latest-target-owned and respects reduced motion', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/' })
  const restoreDom = installTestDom(browser)
  const frames = new Map<number, FrameRequestCallback>()
  const media = { matches: false }
  let nextFrame = 0
  let now = 0
  context.mock.method(globalThis, 'requestAnimationFrame', (callback: FrameRequestCallback) => {
    frames.set(++nextFrame, callback)
    return nextFrame
  })
  context.mock.method(globalThis, 'cancelAnimationFrame', (id: number) => frames.delete(id))
  context.mock.method(performance, 'now', () => now)
  context.mock.method(browser, 'matchMedia', () => media as ReturnType<typeof browser.matchMedia>)
  let controls!: ReturnType<typeof useGlobeRotation>
  const Probe = ({ scope }: { scope: string }) => {
    controls = useGlobeRotation(scope)
    return createElement('div', controls.stageProps)
  }
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const { createRoot } = await import('react-dom/client')
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  const advance = async (milliseconds: number) => {
    now += milliseconds
    const queued = [...frames.values()]
    frames.clear()
    await act(async () => queued.forEach((callback) => callback(now)))
  }
  const centered = (index: number) => {
    const point = projectHorizonNode(index, controls.yaw, controls.pitch)
    assert.equal(point.visible, true)
    assert.ok(Math.abs(point.x - 50) < 1e-8)
    assert.ok(Math.abs(point.y - 29.36) < 1e-8)
  }

  try {
    await act(async () => root.render(createElement(Probe, { scope: 'event-a' })))
    await advance(0)
    controls.focus(0)
    await advance(80)
    assert.ok(frames.size === 1, 'one focus frame is scheduled')
    assert.notEqual(controls.yaw, focusHorizonNode(0).yaw, 'normal motion has intermediate frames')
    controls.focus(7)
    await advance(320)
    centered(7)
    assert.equal(frames.size, 0, 'finished focus has no idle loop')

    controls.focus(1)
    await advance(80)
    controls.rotate(0.2, -0.1)
    await advance(16)
    const interrupted = { yaw: controls.yaw, pitch: controls.pitch }
    await advance(1000)
    assert.deepEqual({ yaw: controls.yaw, pitch: controls.pitch }, interrupted, 'manual input cancels focus')

    media.matches = true
    controls.focus(3)
    await advance(16)
    centered(3)
    assert.equal(frames.size, 0, 'reduced motion snaps in one frame')
    media.matches = false
    controls.focus(4)
    await advance(80)
    media.matches = true
    await advance(16)
    centered(4)
    assert.equal(frames.size, 0, 'a live reduced-motion change also stops the transition')

    media.matches = false
    controls.focus(2)
    await advance(80)
    await act(async () => root.render(createElement(Probe, { scope: 'event-b' })))
    await advance(1000)
    assert.equal(controls.yaw, 0)
    assert.equal(controls.pitch, 0)
    assert.equal(frames.size, 0, 'scope changes cannot publish a previous target')

    controls.focus(5)
    await act(async () => root.unmount())
    assert.equal(frames.size, 0, 'unmount cancels the focus frame')
  } finally {
    await act(async () => root.unmount())
    context.mock.restoreAll()
    restoreDom()
    await browser.happyDOM.close()
  }
})
