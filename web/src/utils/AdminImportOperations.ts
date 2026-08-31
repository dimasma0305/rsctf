export interface AdminImportOperation {
  operationId: string
  requestSignature: string
  createdAt: number
}

interface StorageLike {
  getItem(key: string): string | null
  setItem(key: string, value: string): void
  removeItem(key: string): void
}

const STORAGE_KEY = 'rsctf:admin:user-import-operation'
const MAX_AGE_MS = 60 * 60_000
const MAX_BYTES = 512
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i
const DIGEST = /^[0-9a-f]{64}$/

export const adminImportRequestSignature = async (request: unknown): Promise<string> => {
  const bytes = new TextEncoder().encode(JSON.stringify(request))
  const digest = await crypto.subtle.digest('SHA-256', bytes)
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join('')
}

export const readAdminImportOperation = (
  storage: StorageLike,
  now: number = Date.now()
): AdminImportOperation | null => {
  let encoded: string | null
  try {
    encoded = storage.getItem(STORAGE_KEY)
  } catch {
    return null
  }
  if (!encoded || encoded.length > MAX_BYTES) return null
  try {
    const value = JSON.parse(encoded) as Partial<AdminImportOperation> | null
    if (
      value &&
      typeof value.operationId === 'string' &&
      UUID.test(value.operationId) &&
      typeof value.requestSignature === 'string' &&
      DIGEST.test(value.requestSignature) &&
      typeof value.createdAt === 'number' &&
      Number.isFinite(value.createdAt) &&
      value.createdAt <= now &&
      now - value.createdAt <= MAX_AGE_MS
    ) {
      return value as AdminImportOperation
    }
  } catch {
    // Malformed tab state is discarded below.
  }
  try {
    storage.removeItem(STORAGE_KEY)
  } catch {
    // Privacy-restricted storage remains an in-memory-only workflow.
  }
  return null
}

export const retainAdminImportOperation = (
  storage: StorageLike,
  requestSignature: string,
  current: AdminImportOperation | null,
  createId: () => string = () => crypto.randomUUID(),
  now: number = Date.now()
): AdminImportOperation => {
  if (!DIGEST.test(requestSignature)) throw new Error('Invalid import request signature')
  if (current?.requestSignature === requestSignature) return current
  const retained = readAdminImportOperation(storage, now)
  if (retained?.requestSignature === requestSignature) return retained
  const operation = { operationId: createId(), requestSignature, createdAt: now }
  try {
    storage.setItem(STORAGE_KEY, JSON.stringify(operation))
  } catch {
    // The in-memory owner still protects this mounted request lifetime.
  }
  return operation
}

export const clearAdminImportOperation = (storage: StorageLike, operationId: string): void => {
  if (readAdminImportOperation(storage)?.operationId !== operationId) return
  try {
    storage.removeItem(STORAGE_KEY)
  } catch {
    // The authoritative server result is already known.
  }
}
