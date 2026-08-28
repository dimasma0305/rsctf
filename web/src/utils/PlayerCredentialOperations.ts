export interface PlayerCredentialOperation {
  operationId: string
  expectedRevision: number
  createdAt: number
}

const RECOVERY_WINDOW_MS = 15 * 60_000

export const newPlayerCredentialOperationId = (): string => {
  if (typeof crypto.randomUUID === 'function') return crypto.randomUUID()
  const bytes = crypto.getRandomValues(new Uint8Array(16))
  bytes[6] = (bytes[6] & 0x0f) | 0x40
  bytes[8] = (bytes[8] & 0x3f) | 0x80
  const value = Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('')
  return `${value.slice(0, 8)}-${value.slice(8, 12)}-${value.slice(12, 16)}-${value.slice(16, 20)}-${value.slice(20)}`
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
      typeof parsed.expectedRevision !== 'number' ||
      typeof parsed.createdAt !== 'number'
    ) {
      return null
    }
    return parsed as PlayerCredentialOperation
  } catch {
    return null
  }
}

export const claimPlayerCredentialOperation = (
  storage: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>,
  key: string,
  currentRevision: number,
  now: number = Date.now(),
  createId: () => string = newPlayerCredentialOperationId
): PlayerCredentialOperation => {
  const pending = readPlayerCredentialOperation(storage, key)
  const recoverableRevision =
    pending && (pending.expectedRevision === currentRevision || pending.expectedRevision + 1 === currentRevision)
  if (pending && recoverableRevision && now - pending.createdAt < RECOVERY_WINDOW_MS) return pending

  storage.removeItem(key)
  const created: PlayerCredentialOperation = {
    operationId: createId(),
    expectedRevision: currentRevision,
    createdAt: now,
  }
  storage.setItem(key, JSON.stringify(created))
  return created
}

export const ownsPlayerCredentialResult = (
  storage: Pick<Storage, 'getItem'>,
  key: string,
  pending: PlayerCredentialOperation,
  result: { operationId?: string | null; revision: number }
): boolean => {
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
  if (readPlayerCredentialOperation(storage, key)?.operationId === operationId) storage.removeItem(key)
}

export const playerCredentialOperationStorageKey = (
  gameId: number,
  kind: 'ad-token' | 'ad-ssh' | 'koth-api',
  challengeId?: number
) => `player-credential-operation:${gameId}:${kind}:${challengeId ?? 0}`
