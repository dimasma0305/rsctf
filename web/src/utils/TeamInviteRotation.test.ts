import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const source = readFileSync('src/components/TeamEditModal.tsx', 'utf8')

test('invite rotation has a synchronous owner and stable operation identity', () => {
  assert.match(source, /inviteMutationOwner = useRef<\{/)
  assert.match(source, /if \(!team\?\.id \|\| inviteRevision == null \|\| inviteMutationOwner\.current\) return/)
  assert.match(source, /operationId: crypto\.randomUUID\(\)/)
  assert.match(source, /expectedRevision: operation\.expectedRevision/)
  assert.match(source, /response\.data\.revision !== expectedRevision \+ 1/)
  assert.match(source, /inviteMutationOwner\.current !== owner/)
  assert.match(source, /response\.data\.revision !== recoverableOperation\.expectedRevision/)
})

test('invite reads and mutations abort on lifecycle changes and expose retry', () => {
  assert.match(source, /!props\.opened/)
  assert.match(source, /inviteRequestGeneration\.current/)
  assert.match(source, /inviteReadOwner\.current\?\.abort\(\)/)
  assert.match(source, /inviteMutationOwner\.current\?\.controller\.abort\(\)/)
  assert.match(source, /teamInviteCode\(teamId, \{ signal: owner\.signal \}\)/)
  assert.match(source, /\{ signal: controller\.signal \}/)
  assert.match(source, /role="alert"/)
  assert.match(source, /loadInviteCode\(\)/)
})
