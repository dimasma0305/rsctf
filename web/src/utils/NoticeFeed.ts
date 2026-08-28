import { prependUniqueBoundedRow } from '@Utils/FeedReconciliation'
import { NoticeType, type GameNotice } from '@Api'

export const MAX_GAME_NOTICE_ROWS = 100

export interface NoticePushResult {
  accepted: boolean
  rows: GameNotice[]
}

const noticeIdentity = (notice: GameNotice) => notice.id

const compareGameNotices = (left: GameNotice, right: GameNotice) => {
  if (left.type !== right.type && (left.type === NoticeType.Normal || right.type === NoticeType.Normal)) {
    return left.type === NoticeType.Normal ? -1 : 1
  }
  return right.time - left.time || right.id - left.id
}

/** Merge the bounded live and snapshot inputs before sorting so lower-priority
 * socket traffic cannot evict an organizer notice ahead of the final cap. */
export const mergeGameNotices = (
  live: readonly GameNotice[],
  snapshot: readonly GameNotice[],
  limit = MAX_GAME_NOTICE_ROWS
) => {
  const bounded = Number.isFinite(limit) ? Math.max(0, Math.floor(limit)) : 0
  if (bounded === 0) return []

  const seen = new Set<number>()
  return [...live, ...snapshot]
    .filter((notice) => {
      if (seen.has(notice.id)) return false
      seen.add(notice.id)
      return true
    })
    .sort(compareGameNotices)
    .slice(0, bounded)
}

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
