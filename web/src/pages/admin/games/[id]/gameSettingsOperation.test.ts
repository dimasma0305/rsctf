import assert from 'node:assert/strict'
import test from 'node:test'
import type { GameInfoModel } from '@Api'
import {
  clearGameSettingsOperation,
  readGameSettingsOperation,
  reconcileGameSettingsOperation,
  retainGameSettingsOperation,
  type GameSettingsOperationOwner,
  type StorageLike,
} from './gameSettingsOperation'

class MemoryStorage implements StorageLike {
  readonly values = new Map<string, string>()
  getItem(key: string) {
    return this.values.get(key) ?? null
  }
  setItem(key: string, value: string) {
    this.values.set(key, value)
  }
  removeItem(key: string) {
    this.values.delete(key)
  }
}

const operation = (createdAt = Date.now()): GameSettingsOperationOwner => {
  const payload = { id: 7, title: 'Recovered' } as GameInfoModel
  return {
    gameId: 7,
    operationId: '00000000-0000-4000-8000-000000000007',
    digest: JSON.stringify(payload),
    payload,
    createdAt,
  }
}

test('the exact settings payload and operation identity survive a reload', () => {
  const storage = new MemoryStorage()
  const owner = operation()
  retainGameSettingsOperation(storage, owner)
  assert.deepEqual(readGameSettingsOperation(storage, 7, owner.createdAt + 1_000), owner)

  clearGameSettingsOperation(storage, 7, '00000000-0000-4000-8000-000000000008')
  assert.deepEqual(readGameSettingsOperation(storage, 7, owner.createdAt + 1_000), owner)
  clearGameSettingsOperation(storage, 7, owner.operationId)
  assert.equal(readGameSettingsOperation(storage, 7, owner.createdAt + 1_000), null)
})

test('expired or tampered operations are discarded instead of replayed', () => {
  const storage = new MemoryStorage()
  retainGameSettingsOperation(storage, operation())
  assert.equal(readGameSettingsOperation(storage, 7, 60 * 60 * 1000 + 1_001), null)

  const tampered = operation(2_000)
  retainGameSettingsOperation(storage, tampered)
  tampered.payload.title = 'Different request'
  storage.setItem('rsctf:admin:game-settings:7', JSON.stringify(tampered))
  assert.equal(readGameSettingsOperation(storage, 7, 3_000), null)
})

test('duplicate mount effects share one recovery and retry only after a missing result', async () => {
  const owner = operation()
  let recoveries = 0
  let retries = 0
  let release!: () => void
  const waiting = new Promise<void>((resolve) => {
    release = resolve
  })
  const recover = async () => {
    recoveries += 1
    await waiting
    throw { response: { status: 404 } }
  }
  const retry = async () => {
    retries += 1
    return owner.payload
  }
  const first = reconcileGameSettingsOperation(owner, recover, retry)
  const second = reconcileGameSettingsOperation(owner, recover, retry)
  release()
  assert.equal(await first, owner.payload)
  assert.equal(await second, owner.payload)
  assert.equal(recoveries, 1)
  assert.equal(retries, 1)
})
