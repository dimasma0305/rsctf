export interface PasswordResetOperation {
  operationId: string
  requestSignature: string
  createdAt: number
}

interface StorageLike {
  getItem(key: string): string | null
  setItem(key: string, value: string): void
  removeItem(key: string): void
}

const STORAGE_KEY = 'rsctf:account:password-reset-operation'
const MAX_AGE_MS = 15 * 60_000
const MAX_BYTES = 512
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i
const DIGEST = /^[0-9a-f]{64}$/

/** Scope persisted recovery to the link without retaining password-derived data. */
export const passwordResetRequestSignature = async (token: string, email: string): Promise<string> => {
  const input = new TextEncoder().encode(JSON.stringify([token, email]))
  const digest = await crypto.subtle.digest('SHA-256', input)
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join('')
}

export const readPasswordResetOperation = (
  storage: StorageLike,
  now: number = Date.now()
): PasswordResetOperation | null => {
  let encoded: string | null
  try {
    encoded = storage.getItem(STORAGE_KEY)
  } catch {
    return null
  }
  if (encoded && encoded.length <= MAX_BYTES) {
    try {
      const value = JSON.parse(encoded) as Partial<PasswordResetOperation>
      if (
        typeof value.operationId === 'string' &&
        UUID.test(value.operationId) &&
        typeof value.requestSignature === 'string' &&
        DIGEST.test(value.requestSignature) &&
        typeof value.createdAt === 'number' &&
        Number.isFinite(value.createdAt) &&
        value.createdAt <= now &&
        now - value.createdAt <= MAX_AGE_MS
      ) {
        return value as PasswordResetOperation
      }
    } catch {
      // Malformed state is discarded below.
    }
  }
  try {
    storage.removeItem(STORAGE_KEY)
  } catch {
    // Privacy-restricted storage remains in-memory only.
  }
  return null
}

export const retainPasswordResetOperation = (
  storage: StorageLike,
  requestSignature: string,
  current: PasswordResetOperation | null,
  createId: () => string = () => crypto.randomUUID(),
  now: number = Date.now()
): PasswordResetOperation => {
  if (!DIGEST.test(requestSignature)) throw new Error('Invalid password-reset request signature')
  if (current?.requestSignature === requestSignature) return current
  const retained = readPasswordResetOperation(storage, now)
  if (retained?.requestSignature === requestSignature) return retained
  const operation = { operationId: createId(), requestSignature, createdAt: now }
  try {
    storage.setItem(STORAGE_KEY, JSON.stringify(operation))
  } catch {
    // The mounted page still owns the operation in memory.
  }
  return operation
}

export const clearPasswordResetOperation = (storage: StorageLike, operationId: string): void => {
  if (readPasswordResetOperation(storage)?.operationId !== operationId) return
  try {
    storage.removeItem(STORAGE_KEY)
  } catch {
    // The terminal server result is authoritative.
  }
}
