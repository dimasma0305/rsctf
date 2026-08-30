interface StorageLike {
  getItem(key: string): string | null
  setItem(key: string, value: string): void
  removeItem(key: string): void
}

interface StoredOperation {
  operationId: string
  createdAt: number
}

const PREFIX = 'rsctf:admin:password-reset:'
const MAX_AGE_MS = 15 * 60_000
const MAX_BYTES = 128
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i

const storageKey = (adminId: string, userId: string) => `${PREFIX}${adminId}:${userId}`

export const readAdminPasswordResetOperation = (
  storage: StorageLike,
  adminId: string,
  userId: string,
  now: number = Date.now()
): string | null => {
  if (!UUID.test(adminId) || !UUID.test(userId)) return null
  const key = storageKey(adminId, userId)
  let encoded: string | null
  try {
    encoded = storage.getItem(key)
  } catch {
    return null
  }
  if (encoded && encoded.length <= MAX_BYTES) {
    try {
      const value = JSON.parse(encoded) as Partial<StoredOperation>
      if (
        typeof value.operationId === 'string' &&
        UUID.test(value.operationId) &&
        typeof value.createdAt === 'number' &&
        Number.isFinite(value.createdAt) &&
        value.createdAt <= now &&
        now - value.createdAt <= MAX_AGE_MS
      ) {
        return value.operationId
      }
    } catch {
      // Malformed state is discarded below.
    }
  }
  try {
    storage.removeItem(key)
  } catch {
    // Privacy-restricted storage remains in-memory only.
  }
  return null
}

export const retainAdminPasswordResetOperation = (
  storage: StorageLike,
  adminId: string,
  userId: string,
  current: string | null,
  createId: () => string = () => crypto.randomUUID(),
  now: number = Date.now()
): string => {
  if (current && UUID.test(current)) return current
  const retained = readAdminPasswordResetOperation(storage, adminId, userId, now)
  if (retained) return retained
  const operationId = createId()
  try {
    storage.setItem(storageKey(adminId, userId), JSON.stringify({ operationId, createdAt: now }))
  } catch {
    // The caller's in-memory owner still prevents duplicate mounted submits.
  }
  return operationId
}

export const clearAdminPasswordResetOperation = (
  storage: StorageLike,
  adminId: string,
  userId: string,
  operationId: string
): void => {
  if (readAdminPasswordResetOperation(storage, adminId, userId) !== operationId) return
  try {
    storage.removeItem(storageKey(adminId, userId))
  } catch {
    // The authoritative terminal response is already known.
  }
}
