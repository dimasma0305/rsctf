export type FlagVerdictKind = 'success' | 'wrong'

export interface FlagVerdictState {
  kind: FlagVerdictKind
  sequence: number
}

export type FlagVerdictAction =
  | { type: 'show'; result: string; sequence: number }
  | { type: 'dismiss'; sequence: number }
  | { type: 'reset' }

export function getFlagVerdictKind(result: string): FlagVerdictKind | null {
  if (result === 'Accepted') return 'success'
  if (result === 'WrongAnswer') return 'wrong'
  return null
}

export function flagVerdictReducer(state: FlagVerdictState | null, action: FlagVerdictAction): FlagVerdictState | null {
  if (action.type === 'reset') return null

  if (action.type === 'dismiss') {
    return state?.sequence === action.sequence ? null : state
  }

  const kind = getFlagVerdictKind(action.result)
  return kind ? { kind, sequence: action.sequence } : state
}
