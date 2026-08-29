export type ApiCollectionResult<T> = { status: 'loading' } | { status: 'ready'; items: T[] } | { status: 'invalid' }

/**
 * Accept the released raw-array response and the paginated response used by
 * newer rsctf servers. Keep malformed payloads distinct from loading so the
 * caller can report a contract error instead of rendering an empty list.
 */
export function decodeApiCollection<T>(payload: unknown): ApiCollectionResult<T> {
  if (payload === undefined) return { status: 'loading' }
  if (Array.isArray(payload)) return { status: 'ready', items: payload as T[] }

  if (typeof payload === 'object' && payload !== null && 'data' in payload && Array.isArray(payload.data)) {
    return { status: 'ready', items: payload.data as T[] }
  }

  return { status: 'invalid' }
}
