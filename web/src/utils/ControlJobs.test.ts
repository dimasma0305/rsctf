import assert from 'node:assert/strict'
import test from 'node:test'

import { controlJobResultCount } from './ControlJobs'

test('control-job result counts reject malformed server values', () => {
  const base = {
    id: crypto.randomUUID(),
    kind: 'test',
    scopeKey: 'game:1',
    gameId: 1,
    operationId: crypto.randomUUID(),
    fingerprint: 'a'.repeat(64),
    status: 'Succeeded' as const,
    progressCurrent: 1,
    progressTotal: 1,
    requestedGeneration: 1,
    createdAtUtc: 1,
    updatedAtUtc: 1,
  }
  assert.equal(controlJobResultCount({ ...base, result: { generated: 4 } }, 'generated'), 4)
  assert.equal(controlJobResultCount({ ...base, result: { generated: '4' } }, 'generated'), 0)
  assert.equal(controlJobResultCount({ ...base, result: null }, 'generated'), 0)
})
