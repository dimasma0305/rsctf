import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const source = readFileSync('src/pages/admin/games/[id]/challenges/Index.tsx', 'utf8')

test('1 and 100 selected challenges share one revisioned request owner', () => {
  assert.match(source, /MAX_BULK_SELECTION = 100/)
  assert.match(source, /bulkMutationOwner = useRef\(false\)/)
  assert.match(source, /operationId: crypto\.randomUUID\(\)/)
  assert.match(source, /editMutateGameChallengesBulk/)
  assert.doesNotMatch(source, /Promise\.allSettled/)
})

test('destructive jobs recover the same operation while pending', () => {
  assert.match(source, /response\.data\.state === 'Pending' && pollCount < 150/)
  assert.match(source, /bulkOperation\.current!/)
})
