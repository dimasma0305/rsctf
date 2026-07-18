import assert from 'node:assert/strict'
import { afterEach, test } from 'node:test'
import { createSoundEngine } from './audio'

class FakeAudioParam {
  value = 0

  cancelScheduledValues() {}
  exponentialRampToValueAtTime(value: number) { this.value = value }
  setTargetAtTime(value: number) { this.value = value }
  setValueAtTime(value: number) { this.value = value }
}

class FakeNode {
  disconnected = false

  connect<T>(node: T): T { return node }
  disconnect() { this.disconnected = true }
}

class FakeGain extends FakeNode {
  gain = new FakeAudioParam()
}

class FakeSource extends FakeNode {
  buffer: unknown = null
  frequency = new FakeAudioParam()
  startCalls: number[] = []
  stopCalls: number[] = []
  type = 'sine'

  start(when = 0) { this.startCalls.push(when) }
  stop(when = 0) { this.stopCalls.push(when) }
}

class FakeAudioContext {
  static latest: FakeAudioContext | null = null

  currentTime = 0
  destination = new FakeNode()
  gains: FakeGain[] = []
  sampleRate = 100
  sources: FakeSource[] = []
  state: 'closed' | 'running' | 'suspended' = 'suspended'

  constructor() { FakeAudioContext.latest = this }

  close() { this.state = 'closed'; return Promise.resolve() }
  createBiquadFilter() {
    return Object.assign(new FakeNode(), {
      frequency: new FakeAudioParam(),
      Q: new FakeAudioParam(),
      type: 'lowpass',
    })
  }
  createBuffer(_channels: number, length: number) {
    const data = new Float32Array(length)
    return { getChannelData: () => data }
  }
  createBufferSource() {
    const source = new FakeSource()
    this.sources.push(source)
    return source
  }
  createConvolver() { return Object.assign(new FakeNode(), { buffer: null as unknown }) }
  createDynamicsCompressor() {
    return Object.assign(new FakeNode(), {
      attack: new FakeAudioParam(),
      ratio: new FakeAudioParam(),
      release: new FakeAudioParam(),
      threshold: new FakeAudioParam(),
    })
  }
  createGain() {
    const gain = new FakeGain()
    this.gains.push(gain)
    return gain
  }
  createOscillator() {
    const source = new FakeSource()
    this.sources.push(source)
    return source
  }
  resume() { this.state = 'running'; return Promise.resolve() }
}

function installAudioWindow() {
  FakeAudioContext.latest = null
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: {
      AudioContext: FakeAudioContext,
      // Cleanup timers do not need to run in these graph-lifecycle tests.
      setTimeout: () => 0,
    },
  })
}

afterEach(() => {
  delete (globalThis as { window?: unknown }).window
  FakeAudioContext.latest = null
})

test('cinematic sounds are dropped until Web Audio is explicitly unlocked', () => {
  installAudioWindow()
  const sound = createSoundEngine()

  sound.sfxIncoming(5)()
  sound.sfxFirstBlood()()

  assert.equal(FakeAudioContext.latest, null)
  sound.close()
})

test('unlock permits cinematic synthesis and the returned stop is idempotent', async () => {
  installAudioWindow()
  const sound = createSoundEngine()
  sound.unlock()
  await Promise.resolve()
  const context = FakeAudioContext.latest
  assert.ok(context)
  assert.equal(context.state, 'running')

  const stop = sound.sfxFirstBlood()
  assert.ok(context.sources.length > 0)
  const scheduledStops = context.sources.map((source) => source.stopCalls.length)

  stop()
  context.sources.forEach((source, index) => {
    assert.equal(source.stopCalls.length, scheduledStops[index] + 1)
  })
  const stoppedOnce = context.sources.map((source) => source.stopCalls.length)
  stop()
  assert.deepEqual(context.sources.map((source) => source.stopCalls.length), stoppedOnce)
  sound.close()
})

test('muting cancels active alarm and stinger graphs and blocks new sounds', async () => {
  installAudioWindow()
  const sound = createSoundEngine()
  sound.unlock()
  await Promise.resolve()
  const context = FakeAudioContext.latest
  assert.ok(context)

  sound.sfxIncoming(5)
  sound.sfxFirstBlood()
  const activeSources = [...context.sources]
  const scheduledStops = activeSources.map((source) => source.stopCalls.length)

  sound.setEnabled(false)
  activeSources.forEach((source, index) => {
    assert.equal(source.stopCalls.length, scheduledStops[index] + 1)
  })
  assert.equal(context.gains[0].gain.value, 0)

  const sourceCount = context.sources.length
  sound.sfxIncoming(5)
  sound.sfxFirstBlood()
  assert.equal(context.sources.length, sourceCount)
  sound.close()
})
