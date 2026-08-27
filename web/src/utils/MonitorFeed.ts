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

export const gameEventMonitorIdentity = (event: GameEvent) =>
  JSON.stringify([monitorTimeIdentity(event.time), event.type, event.values, event.user ?? null, event.team ?? null])

export const submissionMonitorIdentity = (submission: Submission) =>
  JSON.stringify([
    monitorTimeIdentity(submission.time),
    submission.status ?? null,
    submission.answer ?? null,
    submission.user ?? null,
    submission.team ?? null,
    submission.challenge ?? null,
  ])
