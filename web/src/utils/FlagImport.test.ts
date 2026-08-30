import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { parsePlainFlagRows, validateFlagRows } from './FlagImport'

const utility = readFileSync('src/utils/FlagImport.ts', 'utf8')
const modal = readFileSync('src/components/admin/FlagCreateModal.tsx', 'utf8')
const remoteModal = readFileSync('src/components/admin/AttachmentRemoteEditModal.tsx', 'utf8')
const localModal = readFileSync('src/components/admin/AttachmentUploadModal.tsx', 'utf8')
const detail = readFileSync('src/pages/admin/games/[id]/challenges/[chalId]/Index.tsx', 'utf8')

test('flag imports bound rows and UTF-8 field bytes before sending', () => {
  assert.match(utility, /MAX_FLAG_IMPORT_ROWS = 100/)
  assert.match(utility, /MAX_FLAG_BYTES = 127/)
  assert.match(utility, /TextEncoder/)
  assert.match(utility, /row\.flag\.trim\(\) !== row\.flag/)
  assert.match(modal, /submitting\.current/)
  assert.match(remoteModal, /operationId\.current \?\?= crypto\.randomUUID\(\)/)
  assert.match(localModal, /operationId\.current \?\?= crypto\.randomUUID\(\)/)
  assert.match(localModal, /validateFlagRows\(files\.map/)
  assert.match(localModal, /closeOnClickOutside=\{!disabled\}/)
})

test('active builds poll only the compact status owner', () => {
  assert.match(detail, /useEditGetChallengeBuildStatus/)
  assert.doesNotMatch(detail, /setInterval\(\(\) => \{\s*mutate\(\)/)
})

test('flag row validation uses UTF-8 bytes and canonical surrounding whitespace', () => {
  assert.equal(validateFlagRows([{ flag: 'x'.repeat(127) }]), null)
  assert.match(validateFlagRows([{ flag: 'x'.repeat(128) }]) ?? '', /127 UTF-8 bytes/)
  assert.equal(validateFlagRows([{ flag: '界'.repeat(42) }]), null)
  assert.match(validateFlagRows([{ flag: '界'.repeat(43) }]) ?? '', /127 UTF-8 bytes/)
  assert.match(validateFlagRows([{ flag: ' flag{answer}' }]) ?? '', /whitespace/)
  assert.equal(parsePlainFlagRows('flag{one}\n\nflag{two}').length, 2)
})
