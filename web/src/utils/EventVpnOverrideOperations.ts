export type EventVpnOverrideIntent =
  | {
      kind: 'create'
      reason: string
      durationMinutes: number
      expectedPolicyRevision: number
    }
  | {
      kind: 'revoke'
      overrideId: string
      expectedPolicyRevision: number
    }

export interface EventVpnOverrideOperation {
  operationId: string
  gameId: number
  intent: EventVpnOverrideIntent
  createdAt: number
}

interface StorageLike {
  getItem(key: string): string | null
  setItem(key: string, value: string): void
  removeItem(key: string): void
}

const MAX_AGE_MS = 2 * 60 * 60_000
const MAX_BYTES = 2_048
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i

const key = (gameId: number) => `rsctf:event-vpn-override-operation:${gameId}`

const validIntent = (value: unknown): value is EventVpnOverrideIntent => {
  if (!value || typeof value !== 'object') return false
  const intent = value as Partial<EventVpnOverrideIntent> & { overrideId?: unknown }
  if (!Number.isSafeInteger(intent.expectedPolicyRevision) || Number(intent.expectedPolicyRevision) < 1) {
    return false
  }
  if (intent.kind === 'create') {
    return (
      typeof intent.reason === 'string' &&
      intent.reason.length >= 8 &&
      intent.reason.length <= 512 &&
      Number.isSafeInteger(intent.durationMinutes) &&
      Number(intent.durationMinutes) >= 1 &&
      Number(intent.durationMinutes) <= 60
    )
  }
  return intent.kind === 'revoke' && typeof intent.overrideId === 'string' && UUID.test(intent.overrideId)
}

export const readEventVpnOverrideOperation = (
  storage: StorageLike,
  gameId: number,
  now: number = Date.now()
): EventVpnOverrideOperation | null => {
  const storageKey = key(gameId)
  let encoded: string | null
  try {
    encoded = storage.getItem(storageKey)
  } catch {
    return null
  }
  if (!encoded || encoded.length > MAX_BYTES) {
    if (encoded) {
      try {
        storage.removeItem(storageKey)
      } catch {
        // Privacy-restricted storage remains an in-memory-only workflow.
      }
    }
    return null
  }
  try {
    const value = JSON.parse(encoded) as Partial<EventVpnOverrideOperation> | null
    if (
      value &&
      value.gameId === gameId &&
      typeof value.operationId === 'string' &&
      UUID.test(value.operationId) &&
      typeof value.createdAt === 'number' &&
      Number.isFinite(value.createdAt) &&
      value.createdAt <= now &&
      now - value.createdAt <= MAX_AGE_MS &&
      validIntent(value.intent)
    ) {
      return value as EventVpnOverrideOperation
    }
  } catch {
    // Malformed tab-local state is never trusted as a mutation identity.
  }
  try {
    storage.removeItem(storageKey)
  } catch {
    // Invalid storage cannot be retained as an authoritative operation.
  }
  return null
}

export const retainEventVpnOverrideOperation = (
  storage: StorageLike,
  gameId: number,
  intent: EventVpnOverrideIntent,
  createId: () => string = () => crypto.randomUUID(),
  now: number = Date.now()
): EventVpnOverrideOperation => {
  const current = readEventVpnOverrideOperation(storage, gameId, now)
  if (current && JSON.stringify(current.intent) === JSON.stringify(intent)) return current
  const operation = { operationId: createId(), gameId, intent, createdAt: now }
  const encoded = JSON.stringify(operation)
  if (encoded.length > MAX_BYTES) throw new Error('VPN override operation is too large to retain safely')
  try {
    storage.setItem(key(gameId), encoded)
  } catch {
    // The returned owner still protects this mounted request lifetime.
  }
  return operation
}

export const clearEventVpnOverrideOperation = (storage: StorageLike, gameId: number, operationId: string): void => {
  if (readEventVpnOverrideOperation(storage, gameId)?.operationId === operationId) {
    try {
      storage.removeItem(key(gameId))
    } catch {
      // The server result is authoritative even when storage is unavailable.
    }
  }
}
