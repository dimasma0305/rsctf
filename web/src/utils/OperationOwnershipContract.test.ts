import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const read = (path: string) => readFileSync(`src/${path}`, 'utf8')

test('credential and clone mutations have synchronous owners and stable operation ids', () => {
  const password = read('components/PasswordChangeModal.tsx')
  const reset = read('pages/account/Reset.tsx')
  const profile = read('pages/account/Profile.tsx')
  const importer = read('components/admin/UserImportModal.tsx')
  const clone = read('components/admin/CloneGameModal.tsx')

  assert.match(password, /if \(inFlight\.current\) return/)
  assert.match(password, /component="form" onSubmit=\{onChangePwd\}/)
  assert.doesNotMatch(password, /onClick=\{onChangePwd\}/)
  assert.match(password, /await api\.account\.accountLogOut\(\)/)
  assert.match(reset, /operationId: operationId\.current/)
  assert.match(reset, /component="form"/)
  assert.match(profile, /emailChangeInFlight = useRef\(false\)/)
  assert.match(profile, /emailChangeOperation = useRef<AccountMailOperation \| null>\(null\)/)
  assert.match(profile, /retainAccountMailOperation\([\s\S]*'email-change'[\s\S]*emailChangeOperation\.current/)
  assert.match(profile, /operationId: owner\.operationId/)
  assert.match(profile, /clearAccountMailOperation\(sessionStorage, owner\)/)
  assert.match(importer, /MAX_IMPORT_ROWS = 200/)
  assert.match(importer, /retainAdminImportOperation\(sessionStorage, signature, importOperation\.current\)/)
  assert.match(importer, /operationId: operation\.operationId/)
  assert.match(importer, /adminRecoverUserImport\(retained\.operationId\)/)
  assert.match(clone, /api\.edit\.editCloneGame/)
  assert.match(clone, /if \(!game\?\.id \|\| !canSubmit \|\| inFlight\.current\) return/)
})

test('wsrx reconnects through one generation owner with bounded backoff', () => {
  const provider = read('components/WsrxProvider.tsx')
  assert.match(provider, /connectGeneration/)
  assert.match(provider, /connectActive/)
  assert.match(provider, /Math\.min\(8_000, 500 \* 2 \*\* attempt\)/)
  assert.match(provider, /clearTimeout\(retryTimer\.current\)/)
})
