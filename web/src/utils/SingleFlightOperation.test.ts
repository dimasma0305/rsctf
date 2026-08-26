import assert from 'node:assert/strict'
import test from 'node:test'
import { createConsentSingleFlightOperation, createSingleFlightOperation } from './SingleFlightOperation'

const nextTurn = () => new Promise<void>((resolve) => queueMicrotask(resolve))

test('single click and Enter each start exactly one semantic operation', async () => {
  let calls = 0
  const owner = createSingleFlightOperation(async () => {
    calls += 1
  })

  await owner.run() // pointer activation reaches the native form submit
  assert.equal(calls, 1)
  await owner.run() // Enter reaches the same native form submit
  assert.equal(calls, 2)
})

test('click plus native submit synchronously join the same owner', async () => {
  let calls = 0
  let release: (() => void) | undefined
  const owner = createSingleFlightOperation(async () => {
    calls += 1
    await new Promise<void>((resolve) => {
      release = resolve
    })
  })

  const click = owner.run()
  const submit = owner.run()
  assert.equal(click, submit)
  await nextTurn()
  assert.equal(calls, 1)
  release?.()
  await click
})

test('one Terms acceptance resumes the current operation once without reopening', async () => {
  let prompts = 0
  let requests = 0
  const owner = createConsentSingleFlightOperation({
    requiresConsent: () => true,
    requestConsent: () => {
      prompts += 1
    },
    operation: async (_signal, granted) => {
      assert.equal(granted, true)
      requests += 1
    },
  })

  const pending = owner.run()
  const duplicate = owner.run()
  assert.equal(pending, duplicate)
  await nextTurn()
  assert.equal(prompts, 1)
  assert.equal(requests, 0)
  owner.acceptConsent()
  await pending
  assert.equal(requests, 1)

  await owner.run()
  assert.equal(prompts, 1, 'accepted consent must not be read from a stale render')
  assert.equal(requests, 2)
})

test('rejecting Terms cancels that activation and a later explicit retry can succeed', async () => {
  let prompts = 0
  let requests = 0
  const owner = createConsentSingleFlightOperation({
    requiresConsent: () => true,
    requestConsent: () => {
      prompts += 1
    },
    operation: async () => {
      requests += 1
    },
  })

  const rejected = owner.run()
  await nextTurn()
  owner.rejectConsent()
  await rejected
  assert.equal(requests, 0)

  const retried = owner.run()
  await nextTurn()
  owner.acceptConsent()
  await retried
  assert.equal(prompts, 2)
  assert.equal(requests, 1)
})

test('an operation while consent is disabled cannot pre-grant a later fingerprint attempt', async () => {
  let required = false
  const grants: boolean[] = []
  let prompts = 0
  const owner = createConsentSingleFlightOperation({
    requiresConsent: () => required,
    requestConsent: () => {
      prompts += 1
    },
    operation: async (_signal, granted) => {
      grants.push(granted)
    },
  })

  await owner.run()
  assert.deepEqual(grants, [false])
  required = true
  const pending = owner.run()
  await nextTurn()
  assert.equal(prompts, 1)
  owner.acceptConsent()
  await pending
  assert.deepEqual(grants, [false, true])
})

test('unmount disposal aborts a hanging operation and blocks late retries', async () => {
  const owner = createSingleFlightOperation(
    (signal) =>
      new Promise<void>((resolve, reject) => {
        signal.addEventListener('abort', () => reject(new DOMException('Unmounted', 'AbortError')), { once: true })
      })
  )
  const pending = owner.run()
  await nextTurn()
  owner.dispose()
  await assert.rejects(pending, (error: unknown) => error instanceof Error && error.name === 'AbortError')
  await assert.rejects(owner.run(), (error: unknown) => error instanceof Error && error.name === 'AbortError')
})

test('failed operations release their owner only for an explicit retry', async () => {
  let calls = 0
  const owner = createSingleFlightOperation(async () => {
    calls += 1
    if (calls === 1) throw new Error('temporary failure')
    return 'ok'
  })

  await assert.rejects(owner.run(), /temporary failure/)
  assert.equal(calls, 1)
  assert.equal(await owner.run(), 'ok')
  assert.equal(calls, 2)
})
