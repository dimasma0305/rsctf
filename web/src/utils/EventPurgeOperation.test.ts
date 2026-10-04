import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import test from 'node:test'

test('event purge retains one retry identity across reloads until acknowledgement', () => {
  const source = readFileSync(resolve(process.cwd(), 'src/pages/admin/games/[id]/Info.tsx'), 'utf8')

  assert.match(source, /new RetryableOperationKey\([\s\S]*`rsctf:event-purge:/)
  assert.match(source, /const operationId = operationOwner\.owner\.claim\(\)/)
  assert.match(source, /await api\.edit\.editPurgeGame\([\s\S]*operationId/)
  assert.match(source, /operationOwner\.owner\.complete\(operationId\)/)
  assert.match(source, /catch \(error\) \{\s*operationOwner\.owner\.release\(\)/)
})
