import type { AdminKothObserverModel } from '@Hooks/useGame'

export type KothObserverOperationKind = 'Rotate' | 'Revoke'

export interface KothObserverOperationOwner {
  gameId: number
  challengeId: number
  expectedRevision: number
  generation: number
  operationId: string
  kind: KothObserverOperationKind
  viewGeneration: number
}

export const newKothObserverOperationId = (): string => {
  if (typeof crypto.randomUUID === 'function') return crypto.randomUUID()
  const bytes = crypto.getRandomValues(new Uint8Array(16))
  bytes[6] = (bytes[6] & 0x0f) | 0x40
  bytes[8] = (bytes[8] & 0x3f) | 0x80
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('')
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`
}

export const ownsKothObserverResult = (
  owner: KothObserverOperationOwner | null,
  result: AdminKothObserverModel,
  gameId: number,
  challengeId: number | null,
  viewGeneration: number
): boolean =>
  owner !== null &&
  owner.gameId === gameId &&
  owner.challengeId === challengeId &&
  owner.viewGeneration === viewGeneration &&
  result.operationId === owner.operationId &&
  result.challengeId === owner.challengeId &&
  result.revision === owner.expectedRevision + 1
