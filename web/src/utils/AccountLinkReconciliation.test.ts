import assert from 'node:assert/strict'
import test from 'node:test'
import { reconcileAccountLink } from './AccountLinkReconciliation'

test('an ambiguous account-link response is reconciled with one exact replay', async () => {
  const controller = new AbortController()
  let calls = 0
  const result = await reconcileAccountLink(async () => {
    calls += 1
    if (calls === 1) throw new Error('connection reset after commit')
    return 'terminal-success'
  }, controller.signal)
  assert.equal(result, 'terminal-success')
  assert.equal(calls, 2)
})

test('a definitive invalid link is not replayed', async () => {
  const controller = new AbortController()
  let calls = 0
  await assert.rejects(
    reconcileAccountLink(async () => {
      calls += 1
      throw { response: { status: 400 } }
    }, controller.signal)
  )
  assert.equal(calls, 1)
})

test('navigation cancellation prevents reconciliation', async () => {
  const controller = new AbortController()
  let calls = 0
  await assert.rejects(
    reconcileAccountLink(async () => {
      calls += 1
      controller.abort()
      throw new Error('cancelled')
    }, controller.signal)
  )
  assert.equal(calls, 1)
})
