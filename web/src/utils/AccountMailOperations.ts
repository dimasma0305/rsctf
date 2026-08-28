export type AccountMailPurpose = 'registration' | 'email-change'

export interface AccountMailOperation {
  purpose: AccountMailPurpose
  scope: string
  operationId: string
  createdAt: number
}

interface StorageLike {
  getItem(key: string): string | null
  setItem(key: string, value: string): void
  removeItem(key: string): void
}

const MAX_AGE_MS = 60 * 60_000
const MAX_BYTES = 2_048
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i
const key = (purpose: AccountMailPurpose) => `rsctf:account-mail-operation:${purpose}`

export const readAccountMailOperation = (
  storage: StorageLike,
  purpose: AccountMailPurpose,
  scope: string,
  now: number = Date.now()
): AccountMailOperation | null => {
  let encoded: string | null
  try {
    encoded = storage.getItem(key(purpose))
  } catch {
    return null
  }
  if (!encoded || encoded.length > MAX_BYTES) return null
  try {
    const value = JSON.parse(encoded) as Partial<AccountMailOperation> | null
    if (
      (value?.purpose === 'registration' || value?.purpose === 'email-change') &&
      typeof value.scope === 'string' &&
      value.scope.length <= 1_024 &&
      typeof value.operationId === 'string' &&
      UUID.test(value.operationId) &&
      typeof value.createdAt === 'number' &&
      Number.isFinite(value.createdAt) &&
      value.createdAt <= now &&
      now - value.createdAt <= MAX_AGE_MS
    ) {
      return value.purpose === purpose && value.scope === scope ? (value as AccountMailOperation) : null
    }
  } catch {
    // Malformed tab state is discarded below.
  }
  try {
    storage.removeItem(key(purpose))
  } catch {
    // Storage can be unavailable in privacy-restricted browsers.
  }
  return null
}

export const retainAccountMailOperation = (
  storage: StorageLike,
  purpose: AccountMailPurpose,
  scope: string,
  current: AccountMailOperation | null,
  createId: () => string = () => crypto.randomUUID(),
  now: number = Date.now()
): AccountMailOperation => {
  if (scope.length > 1_024) throw new Error('Account mail operation scope is too large')
  if (current?.purpose === purpose && current.scope === scope) return current
  const retained = readAccountMailOperation(storage, purpose, scope, now)
  if (retained) return retained
  const operation = { purpose, scope, operationId: createId(), createdAt: now }
  const encoded = JSON.stringify(operation)
  if (encoded.length > MAX_BYTES) throw new Error('Account mail operation is too large to retain safely')
  try {
    storage.setItem(key(purpose), encoded)
  } catch {
    // The in-memory owner still protects this mounted request lifetime.
  }
  return operation
}

export const clearAccountMailOperation = (storage: StorageLike, owner: AccountMailOperation): void => {
  if (readAccountMailOperation(storage, owner.purpose, owner.scope)?.operationId !== owner.operationId) return
  try {
    storage.removeItem(key(owner.purpose))
  } catch {
    // The authoritative server result is already known.
  }
}
