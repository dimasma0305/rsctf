import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import test from 'node:test'

test('clone requests carry both source revisions observed by the organizer', () => {
  const clone = readFileSync(resolve(process.cwd(), 'src/components/admin/CloneGameModal.tsx'), 'utf8')

  assert.match(clone, /expectedSourceRevision: game\.configurationRevision/)
  assert.match(clone, /expectedChallengeRevision: game\.challengeConfigurationRevision/)
  assert.match(clone, /sessionStorage\.setItem\(CLONE_OPERATION_KEY/)
  assert.match(clone, /sourceRevision: game\.configurationRevision/)
  assert.match(clone, /clearCloneOperation\(operationOwner, operationId\)/)
})
