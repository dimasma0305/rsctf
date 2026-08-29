import type { GameInfoModel } from '@Api'

/** Optional fields exposed by newer settings endpoints while older RSCTF servers ignore them. */
export type CompatibleGameInfoModel = GameInfoModel & {
  configurationRevision?: number
  operationId?: string | null
}

export interface GameInfoScheduleDraft {
  start: number
  end: number
  freeze: number | null
  writeupDeadline: number
}

export interface GameInfoSaveOperation {
  digest: string
  id: string
}

export interface PreparedGameInfoSave {
  operation: GameInfoSaveOperation
  payload: CompatibleGameInfoModel
}

export function buildGameInfoUpdatePayload(
  game: CompatibleGameInfoModel,
  schedule: GameInfoScheduleDraft,
  vpnPolicyChanged: boolean
): CompatibleGameInfoModel {
  const { operationId: _operationId, serverTime: _serverTime, ...editableGame } = game
  return {
    ...editableGame,
    inviteCode: (game.inviteCode?.length ?? 0) > 6 ? game.inviteCode : null,
    vpnPolicyChangeReason: vpnPolicyChanged ? game.vpnPolicyChangeReason : undefined,
    ...schedule,
  }
}

/** Keep one idempotency key for retries of the same draft and rotate it after any edit. */
export function prepareGameInfoSave(
  payload: CompatibleGameInfoModel,
  previous: GameInfoSaveOperation | null,
  createId: () => string = () => crypto.randomUUID()
): PreparedGameInfoSave {
  const digest = JSON.stringify(payload)
  const operation = previous?.digest === digest ? previous : { digest, id: createId() }
  return {
    operation,
    payload: { ...payload, operationId: operation.id },
  }
}

export function gameInfoDraftChanged(current: CompatibleGameInfoModel, saved: CompatibleGameInfoModel): boolean {
  return JSON.stringify(current) !== JSON.stringify(saved)
}
