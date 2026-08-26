import assert from 'node:assert/strict'
import test from 'node:test'
import { LatestRequest } from './LatestRequest'

type Deferred<T> = {
  promise: Promise<T>
  resolve: (value: T) => void
}

const deferred = <T>(): Deferred<T> => {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((done) => {
    resolve = done
  })
  return { promise, resolve }
}

test('rapid query changes abort and suppress stale responses', async () => {
  const latest = new LatestRequest()
  const oldResponse = deferred<string>()
  const newResponse = deferred<string>()
  let oldSignal: AbortSignal | undefined

  const oldRequest = latest.run((signal) => {
    oldSignal = signal
    return oldResponse.promise
  })
  const newRequest = latest.run(() => newResponse.promise)

  assert.equal(oldSignal?.aborted, true)
  oldResponse.resolve('stale')
  newResponse.resolve('current')
  assert.equal(await oldRequest, undefined)
  assert.equal(await newRequest, 'current')
})

test('unmount cancellation suppresses abort errors but preserves real failures', async () => {
  const latest = new LatestRequest()
  const canceled = latest.run(
    (signal) =>
      new Promise<string>((_resolve, reject) => {
        signal.addEventListener('abort', () => reject(new Error('transport canceled')), { once: true })
      })
  )
  latest.cancel()
  assert.equal(await canceled, undefined)

  await assert.rejects(
    latest.run(async () => Promise.reject(new Error('database unavailable'))),
    /database unavailable/
  )
})
