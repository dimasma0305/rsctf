import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { beginMailOperation, finishMailOperation } from './MailOperation'

test('rapid activation joins one synchronous mail operation', () => {
  const first = beginMailOperation(null, 'same-form')
  const duplicate = beginMailOperation(first.owner, 'same-form')
  assert.equal(first.started, true)
  assert.equal(duplicate.started, false)
  assert.equal(duplicate.owner.operationId, first.owner.operationId)
})

test('an ambiguous retry keeps its operation ID but an edited form rotates it', () => {
  const first = beginMailOperation(null, 'first-form')
  const retained = finishMailOperation(first.owner, false)
  const retry = beginMailOperation(retained, 'first-form')
  assert.equal(retry.owner.operationId, first.owner.operationId)

  retry.owner.running = false
  const edited = beginMailOperation(retry.owner, 'edited-form')
  assert.notEqual(edited.owner.operationId, first.owner.operationId)
  assert.equal(retry.owner.controller.signal.aborted, true)
})

test('a known response clears the retry owner', () => {
  const started = beginMailOperation(null, 'form')
  assert.equal(finishMailOperation(started.owner, true), null)
})

test('account mail forms acquire one owner and send its stable operation ID', () => {
  const register = readFileSync('src/pages/account/Register.tsx', 'utf8')
  const recovery = readFileSync('src/pages/account/Recovery.tsx', 'utf8')
  const profile = readFileSync('src/pages/account/Profile.tsx', 'utf8')
  for (const source of [register, recovery, profile]) {
    assert.match(source, /beginMailOperation\(/)
    assert.match(source, /operationId: operation\.operationId/)
    assert.match(source, /finishMailOperation\(operation, completed\)/)
  }
  assert.match(recovery, /<Button type="submit" disabled=\{disabled\} fullWidth>/)
  assert.doesNotMatch(recovery, /<Button[^>]+onClick=\{onRecovery\}/)
})
