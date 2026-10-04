import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const read = (path: string) => readFileSync(path, 'utf8')

test('user imports expose bounded history with edit and individual mail actions', () => {
  const history = read('src/components/admin/UserImportHistoryModal.tsx')
  const users = read('src/pages/admin/Users.tsx')
  const api = read('src/Api.ts')

  assert.match(users, /<UserImportHistoryModal/)
  assert.match(history, /Non-secret import records are retained for 180 days/)
  assert.match(history, /onEditUser\(row\.userId\)/)
  assert.match(history, /Retry the original temporary credentials/)
  assert.match(history, /Send a fresh, single-use password setup link/)
  assert.match(history, /importOperationId: detail\.operationId/)
  assert.match(history, /const operationId = passwordEmailOperations\.current\.get/)
  assert.match(history, /'Idempotency-Key': operationId/)
  assert.match(api, /path: `\/api\/admin\/users\/imports`/)
  assert.match(api, /path: `\/api\/admin\/users\/\$\{userId\}\/password-email`/)
})

test('import history remains usable on narrow screens and names icon actions', () => {
  const history = read('src/components/admin/UserImportHistoryModal.tsx')

  assert.match(history, /visibleFrom="md"/)
  assert.match(history, /hiddenFrom="md"/)
  assert.match(history, /aria-label={`Edit \$\{row\.userName\}`}/)
  assert.match(history, /aria-label={`Retry original credentials for \$\{row\.email\}`}/)
  assert.match(history, /aria-label={`Send password setup link to \$\{row\.email\}`}/)
  assert.match(history, /aria-live="polite"/)
})

test('CSV import can enroll teams into bounded multi-event selections', () => {
  const importer = read('src/components/admin/UserImportModal.tsx')

  assert.match(importer, /MAX_IMPORT_EVENTS = 10/)
  assert.match(importer, /MAX_TEAM_EVENT_ASSIGNMENTS = 200/)
  assert.match(importer, /label="Events for imported teams"/)
  assert.match(importer, /eventAssignments: selectedEventIds/)
  assert.match(importer, /Division for \$\{eventName\}/)
  assert.match(importer, /Every row needs a team name before events can be assigned/)
  assert.match(importer, /Existing\s+suspended or withdrawn participation remains unchanged/)
  assert.match(importer, /Accepted enrollment follows the platform's\s+optional team-lock policy/)
})
