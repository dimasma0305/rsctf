import type { GameInfoModel } from '@Api'

export interface GameInfoScheduleDraft {
  start: number
  end: number
  freeze: number | null
  writeupDeadline: number
}

export function buildGameInfoUpdatePayload(
  game: GameInfoModel,
  schedule: GameInfoScheduleDraft,
  vpnPolicyChanged: boolean
): GameInfoModel {
  return {
    ...game,
    inviteCode: (game.inviteCode?.length ?? 0) > 6 ? game.inviteCode : null,
    vpnPolicyChangeReason: vpnPolicyChanged ? game.vpnPolicyChangeReason : undefined,
    ...schedule,
  }
}

export function gameInfoDraftChanged(current: GameInfoModel, saved: GameInfoModel): boolean {
  return JSON.stringify(current) !== JSON.stringify(saved)
}
