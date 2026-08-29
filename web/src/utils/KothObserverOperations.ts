import type { AdminKothObserverModel } from '@Hooks/useGame'
import { createUuid } from './Uuid'

export type KothObserverOperationKind = 'Rotate' | 'Revoke'

export interface KothObserverOperationOwner {
  challengeId: number
  expectedRevision: number
  generation: number
  operationId: string
  kind: KothObserverOperationKind
  viewGeneration: number
}

export const newKothObserverOperationId = createUuid

export const ownsKothObserverResult = (
  owner: KothObserverOperationOwner | null,
  result: AdminKothObserverModel,
  challengeId: number | null,
  viewGeneration: number
): boolean =>
  owner !== null &&
  owner.challengeId === challengeId &&
  owner.viewGeneration === viewGeneration &&
  result.operationId === owner.operationId &&
  result.challengeId === owner.challengeId &&
  result.revision === owner.expectedRevision + 1
