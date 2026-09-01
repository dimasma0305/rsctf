import assert from 'node:assert/strict'
import test from 'node:test'
import {
  claimPlayerCredentialOperation,
  clearPlayerCredentialOperation,
  ownsPlayerCredentialResult,
  PLAYER_CREDENTIAL_RECOVERY_WINDOW_MS,
  parsePlayerCredentialRevision,
  playerCredentialOperationWasRejected,
  playerCredentialOperationStorageKey,
  playerCredentialRevisionSignalKey,
  publishPlayerCredentialRevision,
  readPlayerCredentialOperation,
} from './PlayerCredentialOperations'

const memoryStorage = () => {
  const values = new Map<string, string>()
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  }
}

const firstId = '018f0000-0000-4000-8000-000000000001'
const secondId = '018f0000-0000-4000-8000-000000000002'

test('an ambiguous operation survives reload and the committed revision', () => {
  const storage = memoryStorage()
  const key = playerCredentialOperationStorageKey('user:7:User', 13, 'ad-token')
  claimPlayerCredentialOperation(storage, key, 4, 'rotate', 1_000, () => firstId)

  assert.equal(claimPlayerCredentialOperation(storage, key, 4, 'rotate', 2_000, () => secondId).operationId, firstId)
  assert.equal(claimPlayerCredentialOperation(storage, key, 5, 'rotate', 3_000, () => secondId).operationId, firstId)
  assert.equal(readPlayerCredentialOperation(storage, key)?.expectedRevision, 4)
})

test('only the active operation and next revision may disclose one-time material', () => {
  const storage = memoryStorage()
  const key = playerCredentialOperationStorageKey('user:7:User', 13, 'ad-ssh')
  const pending = claimPlayerCredentialOperation(storage, key, 8, 'generate', 1_000, () => firstId)

  assert.equal(ownsPlayerCredentialResult(storage, key, pending, { operationId: firstId, revision: 9 }), true)
  assert.equal(ownsPlayerCredentialResult(storage, key, pending, { operationId: firstId, revision: 10 }), false)
  assert.equal(ownsPlayerCredentialResult(storage, key, pending, { operationId: secondId, revision: 9 }), false)

  clearPlayerCredentialOperation(storage, key, secondId)
  assert.equal(readPlayerCredentialOperation(storage, key)?.operationId, firstId)
  clearPlayerCredentialOperation(storage, key, firstId)
  assert.equal(readPlayerCredentialOperation(storage, key), null)
})

test('SSH recovery identities cannot cross authenticated viewers in one browser profile', () => {
  const storage = memoryStorage()
  const firstViewerKey = playerCredentialOperationStorageKey('user:7:User', 13, 'ad-ssh')
  const secondViewerKey = playerCredentialOperationStorageKey('user:8:User', 13, 'ad-ssh')
  const firstViewer = claimPlayerCredentialOperation(storage, firstViewerKey, 8, 'generate', 1_000, () => firstId)
  const secondViewer = claimPlayerCredentialOperation(storage, secondViewerKey, 0, 'generate', 2_000, () => secondId)

  assert.notEqual(firstViewerKey, secondViewerKey)
  assert.equal(firstViewer.operationId, firstId)
  assert.equal(secondViewer.operationId, secondId)
  assert.equal(
    ownsPlayerCredentialResult(storage, secondViewerKey, firstViewer, { operationId: firstId, revision: 9 }),
    false
  )
})

test('malformed metadata is rejected while stale UI revisions preserve recovery', () => {
  const storage = memoryStorage()
  const key = playerCredentialOperationStorageKey('user:7:User', 13, 'koth-api', 42)
  storage.setItem(key, '{"operationId":"bad","expectedRevision":4,"createdAt":1000,"intent":"rotate"}')
  assert.equal(readPlayerCredentialOperation(storage, key), null)

  claimPlayerCredentialOperation(storage, key, 4, 'rotate', 1_000, () => firstId)
  const recoveredWithoutAuthoritativeRevision = claimPlayerCredentialOperation(
    storage,
    key,
    0,
    'rotate',
    2_000,
    () => secondId
  )
  const recoveredWithAdvancedRevision = claimPlayerCredentialOperation(storage, key, 6, 'rotate', 3_000, () => secondId)
  assert.equal(recoveredWithoutAuthoritativeRevision.operationId, firstId)
  assert.equal(recoveredWithAdvancedRevision.operationId, firstId)
})

test('local timeout retries the known operation before a definitive rejection permits another', () => {
  const storage = memoryStorage()
  const key = playerCredentialOperationStorageKey('user:7:User', 13, 'ad-token')
  const pending = claimPlayerCredentialOperation(storage, key, 2, 'rotate', 1_000, () => firstId)
  const timedOutRetry = claimPlayerCredentialOperation(
    storage,
    key,
    2,
    'rotate',
    1_000 + PLAYER_CREDENTIAL_RECOVERY_WINDOW_MS,
    () => secondId
  )

  assert.equal(timedOutRetry.operationId, firstId)
  clearPlayerCredentialOperation(storage, key, firstId)
  const current = claimPlayerCredentialOperation(
    storage,
    key,
    2,
    'rotate',
    1_000 + PLAYER_CREDENTIAL_RECOVERY_WINDOW_MS + 1,
    () => secondId
  )
  assert.equal(current.operationId, secondId)
  assert.equal(ownsPlayerCredentialResult(storage, key, pending, { operationId: firstId, revision: 3 }), false)
})

test('a reversed superseded response cannot replace the current operation result', () => {
  const storage = memoryStorage()
  const key = playerCredentialOperationStorageKey('user:7:User', 13, 'ad-ssh')
  const older = claimPlayerCredentialOperation(storage, key, 4, 'generate', 1_000, () => firstId)
  clearPlayerCredentialOperation(storage, key, firstId)
  const newer = claimPlayerCredentialOperation(storage, key, 6, 'generate', 2_000, () => secondId)

  assert.equal(ownsPlayerCredentialResult(storage, key, older, { operationId: firstId, revision: 5 }), false)
  assert.equal(ownsPlayerCredentialResult(storage, key, newer, { operationId: secondId, revision: 7 }), true)
})

test('a different intent cannot replace an ambiguous same-revision operation', () => {
  const storage = memoryStorage()
  const key = playerCredentialOperationStorageKey('user:7:User', 13, 'ad-ssh')
  claimPlayerCredentialOperation(storage, key, 2, 'generate', 1_000, () => firstId)

  assert.throws(
    () => claimPlayerCredentialOperation(storage, key, 2, 'revoke', 2_000, () => secondId),
    /Recover the pending generate/
  )
  assert.equal(readPlayerCredentialOperation(storage, key)?.operationId, firstId)
})

test('cross-tab revision signals contain ordering metadata but no secret', () => {
  const storage = memoryStorage()
  const key = playerCredentialRevisionSignalKey(13, 'koth-api', 42)
  publishPlayerCredentialRevision(storage, key, { operationId: firstId, revision: 5 })

  assert.deepEqual(parsePlayerCredentialRevision(storage.getItem(key)), {
    operationId: firstId,
    revision: 5,
  })
  assert.equal(storage.getItem(key)?.includes('token'), false)
  assert.equal(parsePlayerCredentialRevision('{"operationId":"bad","revision":6}'), null)
})

test('cross-tab signal storage failure cannot discard an owned one-time response', () => {
  const throwingStorage = {
    setItem: () => {
      throw new DOMException('Storage unavailable', 'QuotaExceededError')
    },
  }

  assert.equal(
    publishPlayerCredentialRevision(throwingStorage, playerCredentialRevisionSignalKey(13, 'ad-token'), {
      operationId: firstId,
      revision: 5,
    }),
    false
  )
})

test('operation cleanup storage failure cannot discard an owned one-time response', () => {
  const key = playerCredentialOperationStorageKey('user:7:User', 13, 'ad-ssh')
  const stored = JSON.stringify({
    operationId: firstId,
    expectedRevision: 4,
    createdAt: 1_000,
    intent: 'generate',
  })
  const throwingStorage = {
    getItem: () => stored,
    removeItem: () => {
      throw new DOMException('Storage unavailable', 'SecurityError')
    },
  }

  assert.equal(clearPlayerCredentialOperation(throwingStorage, key, firstId), false)
  assert.equal(readPlayerCredentialOperation(throwingStorage, key)?.operationId, firstId)
})

test('only definitive client rejections retire an operation identity', () => {
  const error = (status: number) => ({ response: { status } })
  for (const status of [400, 401, 403, 404, 409, 413, 422]) {
    assert.equal(playerCredentialOperationWasRejected(error(status)), true, String(status))
  }
  for (const status of [408, 425, 429, 500, 503]) {
    assert.equal(playerCredentialOperationWasRejected(error(status)), false, String(status))
  }
  assert.equal(playerCredentialOperationWasRejected(new TypeError('network error')), false)
})
