import assert from 'node:assert/strict'
import { afterEach, test } from 'node:test'
import { createSoundEngine } from '../pages/games/[id]/audio'

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
  offsets: number[] = []
  stopCalls: number[] = []
  type = 'sine'

  start(when = 0, offset = 0) { this.startCalls.push(when); this.offsets.push(offset) }
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
  async decodeAudioData() { return { duration: 2.2 } }
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

function installAudioWindow(fetch = async () => new Response(new Uint8Array(1))) {
  FakeAudioContext.latest = null
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: {
      AudioContext: FakeAudioContext,
      fetch,
      // Cleanup timers do not need to run in these graph-lifecycle tests.
      setTimeout: () => 0,
    },
  })
}

afterEach(() => {
  delete (globalThis as { window?: unknown }).window
  FakeAudioContext.latest = null
})

test('First Blood loads on opt-in and keeps only the current replay in sync', async () => {
  let requests = 0
  let finish!: (response: Response) => void
  installAudioWindow(() => {
    requests++
    return new Promise<Response>(resolve => { finish = resolve })
  })
  const sound = createSoundEngine('/glass.mp3')
  sound.setEnabled(false)
  sound.unlock()
  await Promise.resolve()
  sound.sfxFirstBlood()
  assert.equal(requests, 0)

  sound.setEnabled(true)
  sound.unlock()
  sound.sfxFirstBlood()()
  sound.sfxFirstBlood()
  sound.setEnabled(false)
  sound.setEnabled(true)
  sound.sfxFirstBlood()
  const ac = FakeAudioContext.latest!
  ac.currentTime = 0.3
  finish(new Response(new Uint8Array(1)))
  await new Promise(resolve => setImmediate(resolve))
  assert.deepEqual(ac.sources.map(s => s.offsets), [[0.3]])
  sound.sfxFirstBlood()
  await Promise.resolve()
  assert.equal(requests, 1)
  assert.deepEqual(ac.sources.map(s => s.offsets), [[0.3], [0]])
  sound.close()
  assert.ok(ac.sources.every(s => s.disconnected && s.stopCalls.length === 1))
})

test('First Blood drops an expired cue and retries a failed download on the next alert', async () => {
  let requests = 0
  installAudioWindow(async () => new Response(new Uint8Array(1), { status: ++requests === 1 ? 404 : 200 }))
  const sound = createSoundEngine('/glass.mp3')
  sound.unlock()
  await new Promise(resolve => setImmediate(resolve))
  const ac = FakeAudioContext.latest!
  sound.sfxFirstBlood()
  ac.currentTime = 4
  await new Promise(resolve => setImmediate(resolve))
  assert.equal(ac.sources.length, 0)
  sound.sfxFirstBlood()
  await Promise.resolve()
  assert.deepEqual(ac.sources.map(s => s.offsets), [[0]])
  assert.equal(requests, 2)
  sound.close()
})

test('cinematic sounds are dropped until Web Audio is explicitly unlocked', () => {
  installAudioWindow()
  const sound = createSoundEngine('/glass.mp3')

  sound.sfxIncoming(5)()
  sound.sfxFirstBlood()()
  sound.sfxFirstCrown()()

  assert.equal(FakeAudioContext.latest, null)
  sound.close()
})

test('unlock permits cinematic synthesis and the returned stop is idempotent', async () => {
  installAudioWindow()
  const sound = createSoundEngine('/glass.mp3')
  sound.unlock()
  await Promise.resolve()
  const context = FakeAudioContext.latest
  assert.ok(context)
  assert.equal(context.state, 'running')

  const stop = sound.sfxFirstCrown()
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
  const sound = createSoundEngine('/glass.mp3')
  sound.unlock()
  await Promise.resolve()
  const context = FakeAudioContext.latest
  assert.ok(context)

  sound.sfxIncoming(5)
  sound.sfxFirstCrown()
  const activeSources = [...context.sources]
  const scheduledStops = activeSources.map((source) => source.stopCalls.length)

  sound.setEnabled(false)
  activeSources.forEach((source, index) => {
    assert.equal(source.stopCalls.length, scheduledStops[index] + 1)
  })
  assert.equal(context.gains[0].gain.value, 0)

  const sourceCount = context.sources.length
  sound.sfxIncoming(5)
  sound.sfxFirstCrown()
  assert.equal(context.sources.length, sourceCount)
  sound.close()
})

test('the ambient bed waits for an unlocked context, fades in once, and stops with mute', async () => {
  installAudioWindow()
  const sound = createSoundEngine('/glass.mp3')
  sound.startAmbient()
  assert.equal(FakeAudioContext.latest, null, 'no graph before the context is unlocked')

  sound.unlock()
  await Promise.resolve()
  const context = FakeAudioContext.latest
  assert.ok(context)
  assert.equal(context.sources.length, 3, 'wind loop, filter LFO, and drone start once the context runs')
  const [wind] = context.sources
  assert.equal((wind as { loop?: boolean }).loop, true)
  sound.startAmbient()
  assert.equal(context.sources.length, 3, 'a second start never doubles the bed')

  sound.setEnabled(false)
  assert.deepEqual(context.sources.map((source) => source.stopCalls.length), [1, 1, 1])
  sound.setEnabled(true)
  sound.startAmbient()
  assert.equal(context.sources.length, 6, 'the bed can restart after mute')
  sound.stopAmbient()
  assert.deepEqual(context.sources.slice(3).map((source) => source.stopCalls.length), [1, 1, 1])
  sound.close()
})
