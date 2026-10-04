import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const source = readFileSync('src/components/TeamEditModal.tsx', 'utf8')

test('invite rotation has a synchronous owner and stable operation identity', () => {
  assert.match(source, /inviteMutationOwner = useRef\(false\)/)
  assert.match(source, /if \(!team\?\.id \|\| inviteRevision == null \|\| inviteMutationOwner\.current\) return/)
  assert.match(source, /inviteOperationId\.current \?\? crypto\.randomUUID\(\)/)
  assert.match(source, /expectedRevision: observedRevision/)
  assert.match(source, /generation === inviteRequestGeneration\.current/)
  assert.match(source, /await loadInviteCode\(\)/)
})

test('invite secret reads run only for an open modal and expose retry', () => {
  assert.match(source, /!props\.opened/)
  assert.match(source, /inviteRequestGeneration\.current/)
  assert.match(source, /role="alert"/)
  assert.match(source, /loadInviteCode\(\)/)
})
