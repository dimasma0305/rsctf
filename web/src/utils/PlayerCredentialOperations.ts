import { createUuid } from './Uuid'

export interface PlayerCredentialOperation {
  operationId: string
  expectedRevision: number
  createdAt: number
  intent: string
}

export interface PlayerCredentialRevisionSignal {
  operationId: string
  revision: number
}

type CredentialStorage = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>

const fallbackValues = new Map<string, string>()
const fallbackStorage: CredentialStorage = {
  getItem: (key) => fallbackValues.get(key) ?? null,
  setItem: (key, value) => fallbackValues.set(key, value),
  removeItem: (key) => fallbackValues.delete(key),
}

export const playerCredentialStorage = (): CredentialStorage => {
  try {
    const storage = globalThis.localStorage
    return storage ?? fallbackStorage
  } catch {
    return fallbackStorage
  }
}

const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i
export const PLAYER_CREDENTIAL_RECOVERY_WINDOW_MS = 15 * 60_000

export const playerCredentialOperationStorageKey = (
  viewerScope: string | null,
  gameId: number,
  kind: 'ad-token' | 'ad-ssh' | 'koth-api',
  challengeId: number = 0
) => `player-credential-operation:${viewerScope ?? 'unscoped'}:${gameId}:${kind}:${challengeId}`

export const playerCredentialRevisionSignalKey = (
  gameId: number,
  kind: 'ad-token' | 'ad-ssh' | 'koth-api',
  challengeId: number = 0
) => `player-credential-revision:${gameId}:${kind}:${challengeId}`

export const publishPlayerCredentialRevision = (
  storage: Pick<Storage, 'setItem'>,
  key: string,
  signal: PlayerCredentialRevisionSignal
) => {
  try {
    storage.setItem(key, JSON.stringify(signal))
    return true
  } catch {
    // Cross-tab invalidation is best-effort. A storage quota/privacy failure
    // must never turn an already-owned one-time response into a client error.
    return false
  }
}

export const parsePlayerCredentialRevision = (value: string | null): PlayerCredentialRevisionSignal | null => {
  try {
    const parsed = JSON.parse(value ?? 'null') as Partial<PlayerCredentialRevisionSignal> | null
    if (
      !parsed ||
      typeof parsed.operationId !== 'string' ||
      !UUID_PATTERN.test(parsed.operationId) ||
      !Number.isSafeInteger(parsed.revision) ||
      (parsed.revision ?? -1) < 1
    ) {
      return null
    }
    return parsed as PlayerCredentialRevisionSignal
  } catch {
    return null
  }
}

export const readPlayerCredentialOperation = (
  storage: Pick<Storage, 'getItem'>,
  key: string
): PlayerCredentialOperation | null => {
  try {
    const parsed = JSON.parse(storage.getItem(key) ?? 'null') as Partial<PlayerCredentialOperation> | null
    if (
      !parsed ||
      typeof parsed.operationId !== 'string' ||
      !UUID_PATTERN.test(parsed.operationId) ||
      !Number.isSafeInteger(parsed.expectedRevision) ||
      (parsed.expectedRevision ?? -1) < 0 ||
      !Number.isSafeInteger(parsed.createdAt) ||
      (parsed.createdAt ?? -1) < 0 ||
      typeof parsed.intent !== 'string' ||
      parsed.intent.length < 1 ||
      parsed.intent.length > 128
    ) {
      return null
    }
    return parsed as PlayerCredentialOperation
  } catch {
    return null
  }
}

/**
 * Retain one non-secret mutation identity across ambiguous responses and reloads.
 * Client revision metadata may be absent or stale, so a known operation is
 * always recovered first. Local age is not authoritative either: server expiry
 * starts only after reservation. The backend's locked revision fence therefore
 * retires an expired or superseded operation with a definitive conflict before
 * a later activation may create another identity.
 */
export const claimPlayerCredentialOperation = (
  storage: CredentialStorage,
  key: string,
  currentRevision: number,
  intent: string = 'mutation',
  now: number = Date.now(),
  createId: () => string = createUuid
): PlayerCredentialOperation => {
  const pending = readPlayerCredentialOperation(storage, key)
  if (pending) {
    if (pending.intent !== intent) {
      throw new Error(`Recover the pending ${pending.intent} credential operation before starting ${intent}`)
    }
    return pending
  }

  storage.removeItem(key)
  const created: PlayerCredentialOperation = {
    operationId: createId(),
    expectedRevision: currentRevision,
    createdAt: now,
    intent,
  }
  storage.setItem(key, JSON.stringify(created))
  return created
}

/** Compact a public mutation payload into non-secret operation metadata. */
export const playerCredentialIntent = async (kind: string, payload: string = '') => {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(payload))
  const hash = Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join('')
  return `${kind}:${hash}`
}

export const ownsPlayerCredentialResult = (
  storage: Pick<Storage, 'getItem'>,
  key: string,
  pending: PlayerCredentialOperation,
  result: { operationId?: string | null; revision: number }
) => {
  const current = readPlayerCredentialOperation(storage, key)
  return (
    current?.operationId === pending.operationId &&
    result.operationId === pending.operationId &&
    result.revision === pending.expectedRevision + 1
  )
}

export const clearPlayerCredentialOperation = (
  storage: Pick<Storage, 'getItem' | 'removeItem'>,
  key: string,
  operationId: string
) => {
  try {
    if (readPlayerCredentialOperation(storage, key)?.operationId !== operationId) return false
    storage.removeItem(key)
    return true
  } catch {
    // Cleanup is best-effort after a response is already owned. Retaining the
    // metadata is safe: a later activation exact-retries the same operation.
    return false
  }
}

/** A definitive client rejection proves this operation did not ambiguously commit. */
export const playerCredentialOperationWasRejected = (error: unknown) => {
  const status = (error as { response?: { status?: number } })?.response?.status
  return (
    Number.isInteger(status) && (status ?? 0) >= 400 && (status ?? 0) < 500 && ![408, 425, 429].includes(status ?? 0)
  )
}

/** Serialize one browser profile's cross-tab intent where Web Locks is available. */
export const withPlayerCredentialLock = async <T>(key: string, task: () => Promise<T>): Promise<T> => {
  if (typeof navigator !== 'undefined' && navigator.locks) {
    return navigator.locks.request(`rsctf:${key}`, task)
  }
  return task()
}
