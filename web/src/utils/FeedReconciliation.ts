/** Remove socket rows already present in the authoritative HTTP snapshot while
 * retaining messages that arrived after that snapshot was read. */
export const reconcileLiveRows = <T>(live: T[], snapshot: T[], identity: (row: T) => string | number) => {
  const authoritative = new Set(snapshot.map(identity))
  return live.filter((row) => !authoritative.has(identity(row)))
}

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
