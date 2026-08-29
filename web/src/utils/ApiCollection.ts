export type ApiCollectionResult<T> =
  { status: 'loading' } | { status: 'ready'; items: T[]; total: number; paginated: boolean } | { status: 'invalid' }

export type ApiCollectionView = 'loading' | 'ready' | 'stale' | 'failed'

export interface ApiCollectionDecodeOptions {
  /** Explicit legacy field names that may carry the collection. */
  itemKeys?: readonly string[]
  /** Human-readable boundary name used when a required collection is malformed. */
  label?: string
}

export type ReadyApiCollection<T> = Extract<ApiCollectionResult<T>, { status: 'ready' }>

/**
 * Accept raw arrays, the bounded pagination envelope, and explicitly named
 * legacy collection fields used during rolling upgrades. Keep malformed
 * payloads distinct from loading so callers never pass an object into array
 * methods by accident.
 */
export function decodeApiCollection<T>(
  payload: unknown,
  options: ApiCollectionDecodeOptions = {}
): ApiCollectionResult<T> {
  if (payload === undefined) return { status: 'loading' }
  if (Array.isArray(payload)) {
    return { status: 'ready', items: payload as T[], total: payload.length, paginated: false }
  }

  if (typeof payload === 'object' && payload !== null && 'data' in payload && Array.isArray(payload.data)) {
    const hasTotal = 'total' in payload
    const hasLength = 'length' in payload
    if (!hasTotal && !hasLength) {
      return { status: 'ready', items: payload.data as T[], total: payload.data.length, paginated: false }
    }

    const total = hasTotal ? payload.total : undefined
    const length = hasLength ? payload.length : payload.data.length
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

  if (typeof payload === 'object' && payload !== null) {
    const record = payload as Record<string, unknown>
    for (const itemKey of options.itemKeys ?? []) {
      if (!Object.hasOwn(record, itemKey)) continue
      const items = record[itemKey]
      if (!Array.isArray(items)) return { status: 'invalid' }
      return { status: 'ready', items: items as T[], total: items.length, paginated: false }
    }
  }

  return { status: 'invalid' }
}

export function requireApiCollection<T>(
  payload: unknown,
  options: ApiCollectionDecodeOptions = {}
): ReadyApiCollection<T> {
  const collection = decodeApiCollection<T>(payload, options)
  if (collection.status !== 'ready') {
    throw new TypeError(`${options.label ?? 'API collection'} response has an invalid collection shape`)
  }
  return collection
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
