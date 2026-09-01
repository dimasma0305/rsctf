import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import type { TrafficFlowPage } from '@Api'
import {
  beginTrafficFlowLoad,
  failTrafficFlowLoad,
  TrafficFlowRequestOwner,
  trafficFlowRetryDelay,
  type TrafficFlowLoadState,
} from './useTrafficInspector'

const deferred = <Value>() => {
  let resolve!: (value: Value) => void
  let reject!: (error: unknown) => void
  const promise = new Promise<Value>((onResolve, onReject) => {
    resolve = onResolve
    reject = onReject
  })
  return { promise, resolve, reject }
}

const page: TrafficFlowPage = {
  items: [],
  page: 1,
  pageSize: 50,
  totalItems: 0,
  totalPages: 0,
  snapshotVersion: 'a'.repeat(32),
  indexedPayloadBytes: 0,
  payloadTruncated: false,
}

test('rapid filter changes abort the slow request and only publish the newest response', async () => {
  const owner = new TrafficFlowRequestOwner()
  const slow = deferred<string>()
  const current = deferred<string>()
  let slowSignal: AbortSignal | undefined
  const slowRun = owner.run((signal) => {
    slowSignal = signal
    return slow.promise
  })
  const currentRun = owner.run(() => current.promise)

  assert.equal(slowSignal?.aborted, true)
  current.resolve('new-filter')
  assert.equal(await currentRun, 'new-filter')
  slow.resolve('stale-filter')
  assert.equal(await slowRun, undefined)
})

test('close and unmount cancellation remove the sole retry timer', (context) => {
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  const owner = new TrafficFlowRequestOwner()
  let retries = 0
  owner.schedule(5_000, () => {
    retries += 1
  })
  assert.equal(owner.pendingRetryCount(), 1)
  owner.cancel()
  assert.equal(owner.pendingRetryCount(), 0)
  context.mock.timers.tick(60_000)
  assert.equal(retries, 0)
  context.mock.timers.reset()
})

test('inspection retries honor bounded Retry-After and stop permanent failures', () => {
  const busy = { response: { status: 503, headers: { 'retry-after': '2' } } }
  assert.equal(
    trafficFlowRetryDelay(busy, 1, () => 0.5, 0),
    2_000
  )
  assert.equal(trafficFlowRetryDelay({ response: { status: 400 } }, 1), null)
  assert.equal(trafficFlowRetryDelay(busy, 4), null)
  assert.equal(trafficFlowRetryDelay({ response: { status: 503, headers: { 'retry-after': '3600' } } }, 1), null)
})

test('transient refresh failure retains only the same file last-good page', () => {
  const current: TrafficFlowLoadState = {
    fileScope: 'file-a',
    page,
    loading: false,
    error: null,
    retryAfterMs: null,
  }
  assert.equal(beginTrafficFlowLoad(current, 'file-a').page, page)
  assert.equal(failTrafficFlowLoad(current, 'file-a', new Error('busy'), 2_000).page, page)
  assert.equal(beginTrafficFlowLoad(current, 'file-b').page, null)
  assert.equal(failTrafficFlowLoad(current, 'file-b', new Error('busy'), 2_000).page, null)
})

test('flow components forward abortable versioned requests and keep last-good UI', () => {
  const inspector = readFileSync('src/components/traffic/FlowInspector.tsx', 'utf8')
  const detail = readFileSync('src/components/traffic/FlowDetail.tsx', 'utf8')
  const hook = readFileSync('src/hooks/useTrafficInspector.ts', 'utf8')

  assert.match(hook, /gameGetTrafficFlows\([\s\S]*?requestQuery, \{ signal \}/)
  assert.match(hook, /gameGetTrafficFlowDetail\([\s\S]*?\{ snapshotVersion, flowId \},[\s\S]*?\{ signal \}/)
  assert.match(inspector, /flowPage && ` .*Showing the last successful result/)
  assert.match(inspector, /key=\{flow\.flowId\}/)
  assert.match(inspector, /flowId=\{selectedFlow\?\.flowId \?\? null\}/)
  assert.match(inspector, /snapshotVersion=\{flowPage\?\.snapshotVersion \?\? null\}/)
  assert.match(detail, /detail\.payloadTruncated/)
  assert.doesNotMatch(inspector, /setFlows\(\[\]\)/)
})
