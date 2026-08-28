import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const utility = readFileSync('src/utils/FlagImport.ts', 'utf8')
const modal = readFileSync('src/components/admin/FlagCreateModal.tsx', 'utf8')
const remoteModal = readFileSync('src/components/admin/AttachmentRemoteEditModal.tsx', 'utf8')
const detail = readFileSync('src/pages/admin/games/[id]/challenges/[chalId]/Index.tsx', 'utf8')

test('flag imports bound rows and UTF-8 field bytes before sending', () => {
  assert.match(utility, /MAX_FLAG_IMPORT_ROWS = 100/)
  assert.match(utility, /MAX_FLAG_BYTES = 127/)
  assert.match(utility, /TextEncoder/)
  assert.match(modal, /submitting\.current/)
  assert.match(remoteModal, /operationId\.current \?\?= crypto\.randomUUID\(\)/)
})

test('active builds poll only the compact status owner', () => {
  assert.match(detail, /editGetChallengeBuildStatus/)
  assert.doesNotMatch(detail, /setInterval\(\(\) => \{\s*mutate\(\)/)
})
