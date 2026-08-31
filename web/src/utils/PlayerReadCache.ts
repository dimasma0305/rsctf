import type { Key, ScopedMutator } from 'swr'
import { swrRequestPath } from '@Utils/ViewerIdentity'

export const ACCOUNT_STATS_PATH = '/api/account/stats'
export const CHALLENGE_CATALOG_PATH = '/api/game/challenges'
export const TEAM_SELECTOR_PATH = '/api/team/selector'

export const isPlayerReadPath = (key: Key, paths: ReadonlySet<string>) => {
  const path = swrRequestPath(key)
  return path !== null && paths.has(path)
}

/** Invalidate every query variant within the active viewer-scoped SWR cache. */
export const invalidatePlayerReads = (mutate: ScopedMutator, paths: readonly string[], revalidate = true) => {
  const selected = new Set(paths)
  return mutate((key) => isPlayerReadPath(key, selected), undefined, { revalidate })
}
