import type { GameInfoModel } from '@Api'

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
  payload: GameInfoModel
}

export function buildGameInfoUpdatePayload(
  game: GameInfoModel,
  schedule: GameInfoScheduleDraft,
  vpnPolicyChanged: boolean
): GameInfoModel {
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
  payload: GameInfoModel,
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

export function gameInfoDraftChanged(current: GameInfoModel, saved: GameInfoModel): boolean {
  return JSON.stringify(current) !== JSON.stringify(saved)
}
