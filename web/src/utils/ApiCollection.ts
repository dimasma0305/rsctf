export type ApiCollectionResult<T> =
  { status: 'loading' } | { status: 'ready'; items: T[]; total: number; paginated: boolean } | { status: 'invalid' }

export type ApiCollectionView = 'loading' | 'ready' | 'stale' | 'failed'

/**
 * Accept the released raw-array response and the paginated response used by
 * newer rsctf servers. Keep malformed payloads distinct from loading so the
 * caller can report a contract error instead of rendering an empty list.
 */
export function decodeApiCollection<T>(payload: unknown): ApiCollectionResult<T> {
  if (payload === undefined) return { status: 'loading' }
  if (Array.isArray(payload)) {
    return { status: 'ready', items: payload as T[], total: payload.length, paginated: false }
  }

  if (typeof payload === 'object' && payload !== null && 'data' in payload && Array.isArray(payload.data)) {
    const total = 'total' in payload ? payload.total : undefined
    const length = 'length' in payload ? payload.length : payload.data.length
    if (
      typeof total !== 'number' ||
      !Number.isSafeInteger(total) ||
      total < payload.data.length ||
      typeof length !== 'number' ||
      !Number.isSafeInteger(length) ||
      length !== payload.data.length
    ) {
      return { status: 'invalid' }
    }
    return { status: 'ready', items: payload.data as T[], total, paginated: true }
  }

  return { status: 'invalid' }
}

/** Keep successfully decoded cached data usable when only revalidation fails. */
export function apiCollectionView<T>(collection: ApiCollectionResult<T>, requestError: unknown): ApiCollectionView {
  if (collection.status === 'ready') return requestError === undefined ? 'ready' : 'stale'
  if (collection.status === 'invalid' || requestError !== undefined) return 'failed'
  return 'loading'
}

/** Return a page count only when a server pagination response is available. */
export function apiCollectionPageCount<T>(collection: ApiCollectionResult<T>, pageSize: number): number | undefined {
  if (collection.status !== 'ready' || !collection.paginated) return undefined
  if (!Number.isSafeInteger(pageSize) || pageSize <= 0) return undefined
  return Math.max(1, Math.ceil(collection.total / pageSize))
}
