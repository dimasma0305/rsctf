/** Remove socket rows already present in the authoritative HTTP snapshot while
 * retaining messages that arrived after that snapshot was read. */
export const reconcileLiveRows = <T>(
  live: readonly T[],
  snapshot: readonly T[],
  identity: (row: T) => string | number
) => {
  const authoritative = new Map<string | number, number>()
  for (const row of snapshot) {
    const key = identity(row)
    authoritative.set(key, (authoritative.get(key) ?? 0) + 1)
  }

  return live.filter((row) => {
    const key = identity(row)
    const remaining = authoritative.get(key) ?? 0
    if (remaining === 0) return true
    if (remaining === 1) authoritative.delete(key)
    else authoritative.set(key, remaining - 1)
    return false
  })
}

const finiteLimit = (limit: number) => (Number.isFinite(limit) ? Math.max(0, Math.floor(limit)) : 0)

/** Add a newest socket row without allowing an outage or idle tab to grow the
 * client-side recovery buffer without bound. */
export const prependBoundedRow = <T>(row: T, current: readonly T[], limit: number) => {
  const bounded = finiteLimit(limit)
  if (bounded === 0) return []
  return [row, ...current.slice(0, bounded - 1)]
}

/** Concatenate a live buffer that has already been reconciled as a multiset
 * with its authoritative snapshot. Unlike a Set union, this deliberately keeps
 * repeated snapshot rows because separate audit records can share every
 * displayed field and millisecond timestamp. */
export const mergeReconciledRows = <T>(live: readonly T[], snapshot: readonly T[], limit: number) =>
  [...live, ...snapshot].slice(0, finiteLimit(limit))

/** Socket rows precede their HTTP equivalents. This final de-duplication also
 * covers a render that occurs between an HTTP response and its reconciliation
 * effect. */
export const mergeUniqueRows = <T>(live: T[], snapshot: T[], identity: (row: T) => string | number) => {
  const seen = new Set<string | number>()
  return [...live, ...snapshot].filter((row) => {
    const key = identity(row)
    if (seen.has(key)) return false
    seen.add(key)
    return true
  })
}
