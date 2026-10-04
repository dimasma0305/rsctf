import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const read = (path: string) => readFileSync(path, 'utf8')

test('credential and clone mutations have synchronous owners and stable operation ids', () => {
  const password = read('src/components/PasswordChangeModal.tsx')
  const reset = read('src/pages/account/Reset.tsx')
  const importer = read('src/components/admin/UserImportModal.tsx')
  const clone = read('src/components/admin/CloneGameModal.tsx')

  assert.match(password, /if \(inFlight\.current\) return/)
  assert.match(password, /await api\.account\.accountLogOut\(\)/)
  assert.match(reset, /retainPasswordResetOperation\(sessionStorage, signature, operation\.current\)/)
  assert.match(reset, /component="form"/)
  assert.match(reset, /disabled=\{disabled \|\| intentLocked\}/)
  assert.match(reset, /status === 409/)
  assert.match(importer, /MAX_IMPORT_ROWS = 200/)
  assert.match(importer, /retainAdminImportOperation\(sessionStorage, signature, importOperation\.current\)/)
  assert.match(importer, /adminRecoverUserImport\(retained\.operationId\)/)
  assert.match(clone, /api\.edit\.editCloneGame/)
  assert.match(clone, /if \(!game\?\.id \|\| !canSubmit \|\| inFlight\.current\) return/)
})
