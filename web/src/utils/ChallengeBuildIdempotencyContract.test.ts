import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const api = readFileSync('src/Api.ts', 'utf8')
const detail = readFileSync('src/pages/admin/games/[id]/challenges/[chalId]/Index.tsx', 'utf8')
const audit = readFileSync('src/components/admin/ChallengeAuditModal.tsx', 'utf8')
const card = readFileSync('src/components/admin/ChallengeEditCard.tsx', 'utf8')

test('challenge image rebuilds require and forward an idempotency key', () => {
  const start = api.indexOf('editRebuildChallengeImage: (')
  const end = api.indexOf('editApproveChallenge: (', start)
  assert.notEqual(start, -1)
  assert.notEqual(end, -1)
  const contract = api.slice(start, end)

  assert.match(contract, /operationId: string/)
  assert.match(contract, /\.\.\.params\.headers/)
  assert.match(contract, /"Idempotency-Key": operationId/)
})

test('every challenge rebuild button retains its key across retryable failures', () => {
  for (const source of [detail, audit, card]) {
    assert.match(source, /new RetryableOperationKey\(/)
    assert.match(source, /const operationId = \w+OperationOwner\.claim\(\)/)
    assert.match(source, /editRebuildChallengeImage\([^\n]+, operationId\)/)
    assert.match(source, /\w+OperationOwner\.complete\(operationId\)/)
    assert.match(source, /!isRetryableHttpError\(e\)/)
    assert.match(source, /useEffect\(\(\) => \(\) => \w+OperationOwner\.release\(\)/)
  }
})
