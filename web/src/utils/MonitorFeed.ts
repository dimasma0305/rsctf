import { AnswerResult, type GameEvent, type MonitorSubmission } from '@Api'

type RowIdentity<T> = (row: T) => string

/**
 * Keep only pushed rows not already represented by an authoritative snapshot.
 * Snapshot identities are consumed as a multiset so genuinely repeated rows
 * retain their server-owned multiplicity instead of being collapsed by a Set.
 */
export const unreconciledMonitorRows = <T>(
  pushedRows: readonly T[],
  snapshotRows: readonly T[],
  identity: RowIdentity<T>
) => {
  const represented = new Map<string, number>()
  for (const row of snapshotRows) {
    const key = identity(row)
    represented.set(key, (represented.get(key) ?? 0) + 1)
  }

  return pushedRows.filter((row) => {
    const key = identity(row)
    const remaining = represented.get(key) ?? 0
    if (remaining === 0) return true
    if (remaining === 1) represented.delete(key)
    else represented.set(key, remaining - 1)
    return false
  })
}

export const gameEventMonitorIdentity = (event: GameEvent) => String(event.id)

export const submissionMonitorIdentity = (submission: MonitorSubmission) => String(submission.id)

interface CursorMonitorRow {
  id: number
  cursor: number
}

/** Merge durable real-time, snapshot, and backfill rows newest-cursor first. */
export const mergeCursorMonitorBuffer = <Row extends CursorMonitorRow>(
  incoming: readonly Row[],
  current: readonly Row[],
  limit: number
) => {
  const seen = new Set<number>()
  return [...incoming, ...current]
    .sort((left, right) => right.cursor - left.cursor)
    .filter((row) => {
      if (seen.has(row.id)) return false
      seen.add(row.id)
      return true
    })
    .slice(0, Math.max(0, limit))
}

/** Keep only pushed rows newer than a snapshot's authoritative checkpoint. */
export const rebaseCursorMonitorBuffer = <Row extends CursorMonitorRow>(current: readonly Row[], checkpoint: number) =>
  current.filter((row) => row.cursor > checkpoint)

/** Merge real-time and HTTP rows by durable identity, newest cursor first. */
export const mergeGameEventBuffer = (incoming: readonly GameEvent[], current: readonly GameEvent[], limit: number) =>
  mergeCursorMonitorBuffer(incoming, current, limit)

export const mergeSubmissionBuffer = (
  incoming: readonly MonitorSubmission[],
  current: readonly MonitorSubmission[],
  limit: number
) => mergeCursorMonitorBuffer(incoming, current, limit)

/** Keep only pushes newer than a snapshot's authoritative checkpoint. */
export const rebaseGameEventBuffer = (current: readonly GameEvent[], checkpoint: number) =>
  rebaseCursorMonitorBuffer(current, checkpoint)

export const rebaseSubmissionBuffer = (current: readonly MonitorSubmission[], checkpoint: number) =>
  rebaseCursorMonitorBuffer(current, checkpoint)

/** Fence delayed HTTP snapshots to the query scope and newest request. */
export const monitorSnapshotIsCurrent = (
  activeScope: string,
  requestedScope: string,
  latestRequest: number,
  requestedAt: number
) => activeScope === requestedScope && latestRequest === requestedAt

/** Reject a push from a hub whose game/account scope is being torn down. */
export const monitorPushIsCurrent = <Scope extends string | number>(
  activeScope: Scope,
  connectedScope: Scope,
  cancelled: boolean
) => !cancelled && activeScope === connectedScope

/** Reject a delayed monitor push once its assignment is covered by the durable cursor. */
export const monitorCursorPushIsCurrent = <Scope extends string | number>(
  activeScope: Scope,
  connectedScope: Scope,
  cancelled: boolean,
  cursorInitialized: boolean,
  durableCursor: number,
  pushedCursor: number
) =>
  monitorPushIsCurrent(activeScope, connectedScope, cancelled) && (!cursorInitialized || pushedCursor > durableCursor)

export const monitorEventPushIsCurrent = monitorCursorPushIsCurrent

export interface ScopedMonitorSnapshot<Row> {
  scope: string
  rows: Row[]
}

/** Hide a prior game's/query's snapshot immediately when the route scope changes. */
export const currentMonitorSnapshotRows = <Row>(activeScope: string, snapshot?: ScopedMonitorSnapshot<Row>) =>
  snapshot?.scope === activeScope ? snapshot.rows : undefined

/** Hide buffered pushes from the previous game/account before teardown runs. */
export const currentMonitorBufferRows = <Row>(activeScope: string, bufferedScope: string, rows: readonly Row[]) =>
  activeScope === bufferedScope ? rows : []

const monitorWhitespacePattern = /^\p{White_Space}$/u

const normalizedMonitorSearch = (search: string, locale: string) => {
  let normalized = ''
  let scalarCount = 0
  let pendingSpace = false
  let inspected = 0

  for (const character of search) {
    if (inspected === 512) break
    inspected += 1
    if (monitorWhitespacePattern.test(character)) {
      pendingSpace = scalarCount > 0
      continue
    }
    if (pendingSpace) {
      if (scalarCount === 128) break
      normalized += ' '
      scalarCount += 1
      pendingSpace = false
    }
    for (const lower of character.toLocaleLowerCase(locale)) {
      if (scalarCount === 128) return normalized
      normalized += lower
      scalarCount += 1
    }
  }

  return normalized
}

/** Match a pushed submission with the same result/search dimensions as HTTP. */
export const submissionMatchesMonitorFilter = (
  submission: MonitorSubmission,
  type: AnswerResult | 'All',
  search: string,
  locale: string
) => {
  if (type !== 'All' && submission.status !== type) return false
  const normalizedSearch = normalizedMonitorSearch(search, locale)
  if (!normalizedSearch) return true
  return [submission.answer, submission.user, submission.team, submission.challenge]
    .filter((value): value is string => typeof value === 'string')
    .some((value) => value.toLocaleLowerCase(locale).includes(normalizedSearch))
}
