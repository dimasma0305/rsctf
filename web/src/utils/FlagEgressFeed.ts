import dayjs from 'dayjs'
import relativeTime from 'dayjs/plugin/relativeTime'
import type { FlagEgressEventModel, FlagEgressPage } from '@Api'

dayjs.extend(relativeTime)

const finiteLimit = (limit: number) => (Number.isFinite(limit) ? Math.max(0, Math.floor(limit)) : 0)
const MAX_FLAG_EGRESS_SEARCH_CHARS = 128
const MAX_FLAG_EGRESS_SEARCH_INSPECT_CHARS = 512

/** Mirror the server's whitespace, case, inspection, and scalar bounds so an
 * HTTP row and a live update always make the same search decision. */
export const normalizeFlagEgressSearch = (search: string) => {
  let normalized = ''
  let scalarCount = 0
  let pendingSpace = false
  const inspected = [...search].slice(0, MAX_FLAG_EGRESS_SEARCH_INSPECT_CHARS)
  for (const character of inspected) {
    if (/\s/u.test(character)) {
      pendingSpace = scalarCount > 0
      continue
    }
    if (pendingSpace) {
      if (scalarCount === MAX_FLAG_EGRESS_SEARCH_CHARS) break
      normalized += ' '
      scalarCount += 1
      pendingSpace = false
    }
    for (const lower of character.toLowerCase()) {
      if (scalarCount === MAX_FLAG_EGRESS_SEARCH_CHARS) return normalized
      normalized += lower
      scalarCount += 1
    }
  }
  return normalized
}

/** Merge aggregate updates by stable row id, keeping the newest committed
 * cursor even when an older HTTP response completes after a live push. */
export const mergeFlagEgressRows = (
  incoming: readonly FlagEgressEventModel[],
  current: readonly FlagEgressEventModel[],
  limit: number
) => {
  const latest = new Map<number, FlagEgressEventModel>()
  for (const event of [...current, ...incoming]) {
    const prior = latest.get(event.id)
    if (!prior || event.cursor >= prior.cursor) latest.set(event.id, event)
  }
  return [...latest.values()]
    .sort((left, right) => right.lastSeenUtc - left.lastSeenUtc || right.cursor - left.cursor || right.id - left.id)
    .slice(0, finiteLimit(limit))
}

/** Drop buffered states covered by an authoritative cursor checkpoint. */
export const rebaseFlagEgressRows = (current: readonly FlagEgressEventModel[], checkpoint: number) =>
  current.filter((event) => event.cursor > checkpoint)

export interface ScopedFlagEgressPage {
  scope: string
  page: FlagEgressPage
}

/** Hide a previous viewer/game/query page before effect cleanup runs. */
export const currentFlagEgressPage = (scope: string, snapshot?: ScopedFlagEgressPage) =>
  snapshot?.scope === scope ? snapshot.page : undefined

/** Hide a previous viewer/game buffer synchronously on scope change. */
export const currentFlagEgressBuffer = (
  scope: string,
  bufferedScope: string,
  events: readonly FlagEgressEventModel[]
) => (scope === bufferedScope ? events : [])

export const flagEgressSnapshotIsCurrent = (
  activeScope: string,
  requestedScope: string,
  latestRequest: number,
  requestedAt: number
) => activeScope === requestedScope && latestRequest === requestedAt

export const flagEgressPushIsCurrent = (
  activeFeedScope: string,
  connectedFeedScope: string,
  messageGameId: number,
  connectedGameId: number
) => activeFeedScope === connectedFeedScope && messageGameId === connectedGameId

export const flagEgressMatchesSearch = (event: FlagEgressEventModel, search: string) => {
  const normalized = normalizeFlagEgressSearch(search)
  if (!normalized) return true
  return [event.teamName, event.challengeTitle, event.remoteIp]
    .map((value) => value.toLowerCase())
    .some((value) => value.includes(normalized))
}

/** Keep the relative-time plugin next to the formatter so this feed cannot
 * depend on another lazily loaded page having initialized Day.js first. */
export const formatFlagEgressAge = (timestamp: number, locale: string) => dayjs(timestamp).locale(locale).fromNow()
