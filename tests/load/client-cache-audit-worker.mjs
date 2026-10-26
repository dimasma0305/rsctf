// Run in a fresh process: evicted values must not remain alive in a corpus array.
import { pathToFileURL } from 'node:url'
import { performance } from 'node:perf_hooks'
import { setTimeout as pause } from 'node:timers/promises'

const { localCacheProvider } = await import(pathToFileURL(process.argv[2]))
const cache = localCacheProvider()
cache.clear()
const payloadBytes = 256 * 1024
cache.set('hot-profile', { data: { name: 'Profile working set' } })
const before = process.cpuUsage()
const started = performance.now()
const writes = [],
  reads = [],
  lateness = []
let hits = 0
for (let tick = 0; tick < 1000; tick++) {
  const due = started + tick * 10
  if (due > performance.now()) await pause(due - performance.now())
  lateness.push(Math.max(0, performance.now() - due))
  for (let read = 0; read < 10; read++) {
    const now = performance.now()
    if (cache.get('hot-profile')?.data) hits++
    reads.push(performance.now() - now)
  }
  // Buffer conversion materializes a distinct, flat string rather than a rope
  // sharing a tiny repeated prefix. The allocation is identical in both modes.
  const data = {
    body: Buffer.alloc(payloadBytes, 65 + (tick % 26)).toString('latin1'),
    response: tick
  }
  const now = performance.now()
  cache.set('response-' + tick, {
    data,
    _k: 'response-' + tick,
    isValidating: false
  })
  // Exercise SWR metadata writes that retain the same immutable data model.
  cache.set('response-' + tick, {
    data,
    _k: 'response-' + tick,
    isValidating: true
  })
  cache.set('response-' + tick, {
    data,
    _k: 'response-' + tick,
    isValidating: false
  })
  writes.push(performance.now() - now)
}
global.gc()
const heapUsed = process.memoryUsage().heapUsed
const keys = [...cache.keys()]
const retainedPayloadBytes = keys.reduce(
  (sum, key) => sum + (cache.get(key)?.data?.body?.length || 0),
  0
)
const cpu = process.cpuUsage(before)
console.log(
  JSON.stringify({
    responses: 1000,
    responsesPerSecond: 100,
    reads: 10000,
    hits,
    payloadBytes,
    elapsedMs: performance.now() - started,
    cpuMs: (cpu.user + cpu.system) / 1000,
    postGcHeapBytes: heapUsed,
    retainedPayloadBytes,
    retainedEntries: keys.length,
    estimatedCacheBytes: cache.estimatedBytes ?? null,
    writeLatencyMs: writes,
    readLatencyMs: reads,
    scheduleLatenessMs: lateness
  })
)
