import assert from 'node:assert/strict'
import test from 'node:test'
import { createDeferredTimerOwner } from './DeferredTimer'

test('deferred timer owner cancels every pending lifecycle callback', (context) => {
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  const owner = createDeferredTimerOwner()
  const calls: string[] = []

  try {
    const canceled = owner.schedule(() => calls.push('single'), 100)
    owner.schedule(() => calls.push('late'), 200)
    assert.equal(owner.pending(), 2)

    owner.cancel(canceled)
    assert.equal(owner.pending(), 1)
    context.mock.timers.tick(100)
    assert.deepEqual(calls, [])

    owner.cancelAll()
    context.mock.timers.tick(1_000)
    assert.deepEqual(calls, [])
    assert.equal(owner.pending(), 0)
    assert.equal(
      owner.schedule(() => calls.push('after stop'), 0),
      null
    )
  } finally {
    owner.cancelAll()
    context.mock.timers.reset()
  }
})

test('deferred timer owner releases completed callbacks', (context) => {
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  const owner = createDeferredTimerOwner()
  let calls = 0

  try {
    owner.schedule(() => {
      calls += 1
    }, 25)
    context.mock.timers.tick(25)
    assert.equal(calls, 1)
    assert.equal(owner.pending(), 0)
  } finally {
    owner.cancelAll()
    context.mock.timers.reset()
  }
})
