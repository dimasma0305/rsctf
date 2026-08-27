const inFlightDownloads = new Map<string, Promise<unknown>>()

/**
 * Start at most one download operation for a stable resource key. The map is
 * populated before the request factory runs, closing the same-tick gap between
 * a click and React committing a disabled button state. Mounted controls share
 * this module-level owner and the key is released only after the whole operation
 * settles.
 */
export const runDownloadSingleFlight = <T>(key: string, requestFactory: () => Promise<T>): Promise<T> => {
  const existing = inFlightDownloads.get(key)
  if (existing) return existing as Promise<T>

  let tracked: Promise<T>
  const started = Promise.resolve().then(requestFactory)
  tracked = started.finally(() => {
    if (inFlightDownloads.get(key) === tracked) inFlightDownloads.delete(key)
  })
  inFlightDownloads.set(key, tracked)
  return tracked
}
