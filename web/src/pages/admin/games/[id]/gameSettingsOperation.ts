import { httpErrorStatus } from '@Utils/HttpError'
import type { GameInfoModel } from '@Api'

const STORAGE_PREFIX = 'rsctf:admin:game-settings:'
const MAX_AGE_MS = 60 * 60 * 1000
const MAX_SERIALIZED_BYTES = 512 * 1024

export interface StorageLike {
  getItem(key: string): string | null
  setItem(key: string, value: string): void
  removeItem(key: string): void
}

export interface GameSettingsOperationOwner {
  gameId: number
  operationId: string
  digest: string
  payload: GameInfoModel
  createdAt: number
}

const operationKey = (gameId: number) => `${STORAGE_PREFIX}${gameId}`

const isOperationId = (value: unknown): value is string =>
  typeof value === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(value)

const validOwner = (value: unknown, gameId: number, now: number): value is GameSettingsOperationOwner => {
  if (!value || typeof value !== 'object') return false
  const owner = value as Partial<GameSettingsOperationOwner>
  return (
    owner.gameId === gameId &&
    isOperationId(owner.operationId) &&
    typeof owner.digest === 'string' &&
    owner.payload !== null &&
    typeof owner.payload === 'object' &&
    typeof owner.createdAt === 'number' &&
    Number.isFinite(owner.createdAt) &&
    owner.createdAt <= now &&
    now - owner.createdAt <= MAX_AGE_MS &&
    owner.digest === JSON.stringify(owner.payload)
  )
}

export const readGameSettingsOperation = (
  storage: StorageLike,
  gameId: number,
  now: number = Date.now()
): GameSettingsOperationOwner | null => {
  const key = operationKey(gameId)
  const encoded = storage.getItem(key)
  if (!encoded || encoded.length > MAX_SERIALIZED_BYTES) {
    if (encoded) storage.removeItem(key)
    return null
  }
  try {
    const parsed: unknown = JSON.parse(encoded)
    if (validOwner(parsed, gameId, now)) return parsed
  } catch {
    // A malformed tab-local value is not an operation and must not be retried.
  }
  storage.removeItem(key)
  return null
}

export const retainGameSettingsOperation = (storage: StorageLike, owner: GameSettingsOperationOwner) => {
  const encoded = JSON.stringify(owner)
  if (encoded.length > MAX_SERIALIZED_BYTES) throw new Error('Event settings draft is too large to retain safely')
  storage.setItem(operationKey(owner.gameId), encoded)
}

export const clearGameSettingsOperation = (storage: StorageLike, gameId: number, operationId?: string) => {
  if (!operationId) {
    storage.removeItem(operationKey(gameId))
    return
  }
  const current = readGameSettingsOperation(storage, gameId)
  if (current?.operationId === operationId) storage.removeItem(operationKey(gameId))
}

const reconciliationFlights = new Map<string, Promise<GameInfoModel>>()

/**
 * One tab may mount the editor effect more than once (including React strict
 * mode). Share one recovery/retry flight for the same durable operation.
 */
export const reconcileGameSettingsOperation = (
  owner: GameSettingsOperationOwner,
  recover: () => Promise<GameInfoModel>,
  retry: () => Promise<GameInfoModel>
): Promise<GameInfoModel> => {
  const key = `${owner.gameId}:${owner.operationId}`
  const existing = reconciliationFlights.get(key)
  if (existing) return existing
  const flight = recover()
    .catch((error: unknown) => {
      if (httpErrorStatus(error) !== 404) throw error
      return retry()
    })
    .finally(() => reconciliationFlights.delete(key))
  reconciliationFlights.set(key, flight)
  return flight
}
