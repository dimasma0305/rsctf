import assert from 'node:assert/strict'
import test from 'node:test'
import {
  RETRYABLE_OPERATION_STORAGE_KEY,
  RETRYABLE_OPERATION_STORAGE_LIMIT,
  RetryableOperationKey,
} from './RetryableOperationKey'

class MemoryStorage {
  readonly values = new Map<string, string>()

  getItem(key: string): string | null {
    return this.values.get(key) ?? null
  }

  setItem(key: string, value: string): void {
    this.values.set(key, value)
  }

  removeItem(key: string): void {
    this.values.delete(key)
  }
}

const operationId = (value: number) => `00000000-0000-4000-8000-${value.toString().padStart(12, '0')}`

const manifestEntries = (storage: MemoryStorage) => {
  const stored = storage.getItem(RETRYABLE_OPERATION_STORAGE_KEY)
  assert.ok(stored)
  return (JSON.parse(stored) as { entries: Record<string, { operationId: string; touchedAt: number }> }).entries
}

test('failed durable operations retain their key until an acknowledged completion', () => {
  const ids = ['operation-1', 'operation-2']
  const key = new RetryableOperationKey(() => ids.shift()!)

  const first = key.claim()
  assert.equal(first, 'operation-1')
  assert.equal(key.claim(), first)

  key.complete('another-operation')
  assert.equal(key.claim(), first)

  key.complete(first)
  assert.equal(key.claim(), 'operation-2')
})

test('a component remount restores its pending operation from one bounded namespace', () => {
  const storage = new MemoryStorage()
  const firstId = operationId(1)
  const nextId = operationId(2)
  const firstMount = new RetryableOperationKey(
    () => firstId,
    'game-13',
    storage,
    () => 1_000
  )

  assert.equal(firstMount.claim(), firstId)
  firstMount.release()
  assert.deepEqual(manifestEntries(storage)['game-13'], { operationId: firstId, touchedAt: 1_000 })
  assert.deepEqual([...storage.values.keys()], [RETRYABLE_OPERATION_STORAGE_KEY])
  assert.equal(new RetryableOperationKey(() => nextId, 'game-13', storage).claim(), firstId)

  const remount = new RetryableOperationKey(() => nextId, 'game-13', storage)
  remount.complete(firstId)
  assert.equal(new RetryableOperationKey(() => nextId, 'game-13', storage).claim(), nextId)
})

test('the legacy per-scope key migrates only after the bounded manifest is durable', () => {
  const storage = new MemoryStorage()
  const firstId = operationId(3)
  storage.setItem('game-13', firstId)
  const owner = new RetryableOperationKey(
    () => operationId(4),
    'game-13',
    storage,
    () => 2_000
  )

  assert.equal(owner.claim(), firstId)
  assert.equal(storage.getItem('game-13'), null)
  assert.equal(manifestEntries(storage)['game-13'].operationId, firstId)
})

test('the previous container-operation object migrates without losing a reload-safe retry', () => {
  const storage = new MemoryStorage()
  const firstId = operationId(4)
  const scope = 'rsctf:container-operation:create:user:13:50'
  storage.setItem(scope, JSON.stringify({ scope: 'user:13:50', id: firstId }))
  const owner = new RetryableOperationKey(() => operationId(5), scope, storage, () => 3_000)

  assert.equal(owner.claim(), firstId)
  assert.equal(storage.getItem(scope), null)
  assert.equal(manifestEntries(storage)[scope].operationId, firstId)
})

test('malformed session state cannot poison a later operation', () => {
  const storage = new MemoryStorage()
  storage.setItem(RETRYABLE_OPERATION_STORAGE_KEY, '{bad-json')
  storage.setItem('game-13', 'not-a-uuid')
  const expected = operationId(5)

  assert.equal(new RetryableOperationKey(() => expected, 'game-13', storage).claim(), expected)
  assert.equal(storage.getItem('game-13'), null)
})

test('the LRU namespace stays bounded without evicting an identity active in this page', () => {
  const storage = new MemoryStorage()
  const active = new RetryableOperationKey(
    () => operationId(1),
    'active-game',
    storage,
    () => 1
  )
  assert.equal(active.claim(), operationId(1))

  for (let index = 2; index <= RETRYABLE_OPERATION_STORAGE_LIMIT + 6; index += 1) {
    const owner = new RetryableOperationKey(
      () => operationId(index),
      `game-${index}`,
      storage,
      () => index
    )
    owner.claim()
    owner.release()
  }

  const entries = manifestEntries(storage)
  assert.equal(Object.keys(entries).length, RETRYABLE_OPERATION_STORAGE_LIMIT)
  assert.equal(entries['active-game'].operationId, operationId(1))
  assert.equal(entries['game-2'], undefined)
  assert.equal(entries[`game-${RETRYABLE_OPERATION_STORAGE_LIMIT + 6}`].operationId, operationId(38))
  active.release()
})

test('quota pressure retries with the active identity only', () => {
  const storage = new MemoryStorage()
  storage.values.set(
    RETRYABLE_OPERATION_STORAGE_KEY,
    JSON.stringify({
      version: 1,
      entries: {
        old1: { operationId: operationId(1), touchedAt: 1 },
        old2: { operationId: operationId(2), touchedAt: 2 },
      },
    })
  )
  let rejectedWrites = 0
  const quotaStorage = {
    getItem: storage.getItem.bind(storage),
    removeItem: storage.removeItem.bind(storage),
    setItem: (key: string, value: string) => {
      const entryCount = Object.keys((JSON.parse(value) as { entries: object }).entries).length
      if (entryCount > 1) {
        rejectedWrites += 1
        throw new DOMException('full', 'QuotaExceededError')
      }
      storage.setItem(key, value)
    },
  }
  const currentId = operationId(9)
  const owner = new RetryableOperationKey(
    () => currentId,
    'current',
    quotaStorage,
    () => 9
  )

  assert.equal(owner.claim(), currentId)
  assert.equal(rejectedWrites, 1)
  assert.deepEqual(Object.keys(manifestEntries(storage)), ['current'])
  assert.equal(new RetryableOperationKey(() => operationId(10), 'current', quotaStorage).claim(), currentId)
})

test('unavailable storage never breaks in-memory retry or authoritative completion', () => {
  const ids = [operationId(11), operationId(12)]
  const unavailableStorage = {
    getItem: () => {
      throw new DOMException('blocked', 'SecurityError')
    },
    setItem: () => {
      throw new DOMException('full', 'QuotaExceededError')
    },
    removeItem: () => {
      throw new DOMException('blocked', 'SecurityError')
    },
  }
  const owner = new RetryableOperationKey(() => ids.shift()!, 'game-13', unavailableStorage)

  const first = owner.claim()
  assert.equal(owner.claim(), first)
  owner.release()
  assert.equal(owner.claim(), first)
  owner.complete(first)
  assert.equal(owner.claim(), operationId(12))
})
