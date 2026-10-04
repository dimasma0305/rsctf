export interface EventVpnOverrideCreateOperation {
  kind: 'create'
  gameId: number
  operationId: string
  signature: string
  reason: string
  durationMinutes: number
  expectedPolicyRevision: number
  createdAt: number
}

export interface EventVpnOverrideRevokeOperation {
  kind: 'revoke'
  gameId: number
  overrideId: string
  operationId: string
  expectedPolicyRevision: number
  createdAt: number
}

export type EventVpnOverrideOperation = EventVpnOverrideCreateOperation | EventVpnOverrideRevokeOperation

interface StorageLike {
  getItem(key: string): string | null
  setItem(key: string, value: string): void
  removeItem(key: string): void
}

const STORAGE_KEY = 'rsctf:event-vpn-override-operations'
const MAX_AGE_MS = 24 * 60 * 60_000
const MAX_RECORDS = 32
const MAX_STORAGE_CHARS = 32 * 1024
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i

const characterCount = (value: string): number => Array.from(value).length

const validRevision = (value: unknown): value is number =>
  typeof value === 'number' && Number.isSafeInteger(value) && value >= 1

const validCreatedAt = (value: unknown, now: number): value is number =>
  typeof value === 'number' && Number.isFinite(value) && value <= now && now - value <= MAX_AGE_MS

const validBase = (
  value: Partial<EventVpnOverrideOperation>,
  now: number
): value is Partial<EventVpnOverrideOperation> & {
  gameId: number
  operationId: string
  expectedPolicyRevision: number
  createdAt: number
} =>
  Number.isSafeInteger(value.gameId) &&
  (value.gameId ?? 0) > 0 &&
  typeof value.operationId === 'string' &&
  UUID.test(value.operationId) &&
  validRevision(value.expectedPolicyRevision) &&
  validCreatedAt(value.createdAt, now)

const isOperation = (value: unknown, now: number): value is EventVpnOverrideOperation => {
  if (!value || typeof value !== 'object') return false
  const candidate = value as Partial<EventVpnOverrideOperation>
  if (!validBase(candidate, now)) return false
  if (candidate.kind === 'create') {
    return (
      typeof candidate.signature === 'string' &&
      candidate.signature.length <= 2_100 &&
      typeof candidate.reason === 'string' &&
      candidate.reason.trim() === candidate.reason &&
      characterCount(candidate.reason) >= 8 &&
      characterCount(candidate.reason) <= 512 &&
      Number.isInteger(candidate.durationMinutes) &&
      (candidate.durationMinutes ?? 0) >= 1 &&
      (candidate.durationMinutes ?? 0) <= 60 &&
      candidate.signature === `${candidate.reason}\0${candidate.durationMinutes}`
    )
  }
  return candidate.kind === 'revoke' && typeof candidate.overrideId === 'string' && UUID.test(candidate.overrideId)
}

const readAll = (storage: StorageLike, now: number): EventVpnOverrideOperation[] => {
  let encoded: string | null
  try {
    encoded = storage.getItem(STORAGE_KEY)
  } catch {
    return []
  }
  if (!encoded) return []
  if (encoded.length > MAX_STORAGE_CHARS) {
    try {
      storage.removeItem(STORAGE_KEY)
    } catch {
      // Privacy-restricted storage remains unavailable.
    }
    return []
  }
  try {
    const parsed = JSON.parse(encoded)
    if (!Array.isArray(parsed)) throw new Error('invalid operation collection')
    return parsed.filter((operation) => isOperation(operation, now)).slice(0, MAX_RECORDS)
  } catch {
    try {
      storage.removeItem(STORAGE_KEY)
    } catch {
      // Privacy-restricted storage remains unavailable.
    }
    return []
  }
}

const writeAll = (storage: StorageLike, operations: EventVpnOverrideOperation[]): void => {
  const bounded = operations.sort((a, b) => b.createdAt - a.createdAt).slice(0, MAX_RECORDS)
  try {
    if (bounded.length === 0) storage.removeItem(STORAGE_KEY)
    else {
      let encoded = JSON.stringify(bounded)
      while (encoded.length > MAX_STORAGE_CHARS && bounded.length > 1) {
        bounded.pop()
        encoded = JSON.stringify(bounded)
      }
      if (encoded.length <= MAX_STORAGE_CHARS) storage.setItem(STORAGE_KEY, encoded)
      else storage.removeItem(STORAGE_KEY)
    }
  } catch {
    // The caller retains the same operation in memory for this mounted request.
  }
}

export const readEventVpnOverrideOperations = (
  storage: StorageLike,
  gameId: number,
  now: number = Date.now()
): {
  create: EventVpnOverrideCreateOperation | null
  revokes: EventVpnOverrideRevokeOperation[]
} => {
  const operations = readAll(storage, now).filter((operation) => operation.gameId === gameId)
  return {
    create:
      operations.find((operation): operation is EventVpnOverrideCreateOperation => operation.kind === 'create') ?? null,
    revokes: operations.filter(
      (operation): operation is EventVpnOverrideRevokeOperation => operation.kind === 'revoke'
    ),
  }
}

export const retainEventVpnOverrideCreateOperation = (
  storage: StorageLike,
  input: Omit<EventVpnOverrideCreateOperation, 'kind' | 'operationId' | 'createdAt'>,
  current: EventVpnOverrideCreateOperation | null,
  createId: () => string = () => crypto.randomUUID(),
  now: number = Date.now()
): EventVpnOverrideCreateOperation => {
  const stored = readEventVpnOverrideOperations(storage, input.gameId, now).create
  const reusable = [current, stored].find(
    (operation) => operation?.gameId === input.gameId && operation.signature === input.signature
  )
  if (reusable) return reusable
  const operation: EventVpnOverrideCreateOperation = {
    kind: 'create',
    operationId: createId(),
    createdAt: now,
    ...input,
  }
  const remaining = readAll(storage, now).filter(
    (candidate) => candidate.kind !== 'create' || candidate.gameId !== input.gameId
  )
  writeAll(storage, [operation, ...remaining])
  return operation
}

export const retainEventVpnOverrideRevokeOperation = (
  storage: StorageLike,
  input: Omit<EventVpnOverrideRevokeOperation, 'kind' | 'operationId' | 'createdAt'>,
  current: EventVpnOverrideRevokeOperation | null,
  createId: () => string = () => crypto.randomUUID(),
  now: number = Date.now()
): EventVpnOverrideRevokeOperation => {
  const stored = readEventVpnOverrideOperations(storage, input.gameId, now).revokes.find(
    (operation) => operation.overrideId === input.overrideId
  )
  const reusable = current?.overrideId === input.overrideId && current.gameId === input.gameId ? current : stored
  if (reusable) return reusable
  const operation: EventVpnOverrideRevokeOperation = {
    kind: 'revoke',
    operationId: createId(),
    createdAt: now,
    ...input,
  }
  const remaining = readAll(storage, now).filter(
    (candidate) =>
      candidate.kind !== 'revoke' || candidate.gameId !== input.gameId || candidate.overrideId !== input.overrideId
  )
  writeAll(storage, [operation, ...remaining])
  return operation
}

export const clearEventVpnOverrideCreateOperation = (
  storage: StorageLike,
  gameId: number,
  operationId: string,
  now: number = Date.now()
): void => {
  writeAll(
    storage,
    readAll(storage, now).filter(
      (operation) => operation.kind !== 'create' || operation.gameId !== gameId || operation.operationId !== operationId
    )
  )
}

export const clearEventVpnOverrideRevokeOperation = (
  storage: StorageLike,
  gameId: number,
  overrideId: string,
  operationId: string,
  now: number = Date.now()
): void => {
  writeAll(
    storage,
    readAll(storage, now).filter(
      (operation) =>
        operation.kind !== 'revoke' ||
        operation.gameId !== gameId ||
        operation.overrideId !== overrideId ||
        operation.operationId !== operationId
    )
  )
}
