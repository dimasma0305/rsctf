import type { GameEvent, Submission } from '@Api'

type RowIdentity<T> = (row: T) => string

const monitorTimeIdentity = (value: unknown) => {
  if (typeof value === 'number' && Number.isFinite(value)) return value
  if (typeof value === 'string') {
    const numeric = Number(value)
    if (value.trim() && Number.isFinite(numeric)) return numeric
    const parsed = Date.parse(value)
    if (Number.isFinite(parsed)) return parsed
  }
  return value ?? null
}

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

/** Merge real-time and HTTP rows by durable identity, newest commit first. */
export const mergeGameEventBuffer = (incoming: readonly GameEvent[], current: readonly GameEvent[], limit: number) => {
  const seen = new Set<number>()
  return [...incoming, ...current]
    .sort((left, right) => right.cursor - left.cursor)
    .filter((event) => {
      if (seen.has(event.id)) return false
      seen.add(event.id)
      return true
    })
    .slice(0, Math.max(0, limit))
}

/** Keep only pushes newer than a snapshot's authoritative checkpoint. */
export const rebaseGameEventBuffer = (current: readonly GameEvent[], checkpoint: number) =>
  current.filter((event) => event.cursor > checkpoint)

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

export const submissionMonitorIdentity = (submission: Submission) =>
  JSON.stringify([
    monitorTimeIdentity(submission.time),
    submission.status ?? null,
    submission.answer ?? null,
    submission.user ?? null,
    submission.team ?? null,
    submission.challenge ?? null,
  ])
