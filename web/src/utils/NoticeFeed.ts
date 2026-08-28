import { prependUniqueBoundedRow } from '@Utils/FeedReconciliation'
import type { GameNotice } from '@Api'

export const MAX_GAME_NOTICE_ROWS = 100

export interface NoticePushResult {
  accepted: boolean
  rows: GameNotice[]
}

const noticeIdentity = (notice: GameNotice) => notice.id

/** Accept a pushed notice only once across the live buffer and authoritative
 * snapshot. The caller may safely gate toast side effects on `accepted`. */
export const receiveGameNotice = (
  notice: GameNotice,
  live: readonly GameNotice[],
  snapshot: readonly GameNotice[]
): NoticePushResult => {
  const known = live.some((row) => row.id === notice.id) || snapshot.some((row) => row.id === notice.id)
  if (known) return { accepted: false, rows: live.slice(0, MAX_GAME_NOTICE_ROWS) }

  return {
    accepted: true,
    rows: prependUniqueBoundedRow(notice, live, MAX_GAME_NOTICE_ROWS, noticeIdentity),
  }
}
