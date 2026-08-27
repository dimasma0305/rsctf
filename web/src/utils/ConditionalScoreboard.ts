export const MAX_SCOREBOARD_VALIDATORS = 64

type ConditionalResponse = {
  status: number
  data: unknown
  etag?: string
}

type ConditionalRequest = (path: string, etag?: string) => Promise<ConditionalResponse>

type ValidatorEntry = {
  etag: string
  generation: number
  value: WeakRef<object>
}

export const isConditionalScoreboardPath = (path: string) =>
  /^\/api\/game\/\d+\/(?:scoreboard|ad\/koth\/scoreboard)$/.test(path)

/**
 * Keep only weak references to parsed boards. SWR owns the live value; this
 * reader retains a bounded validator index and can return SWR's exact object on
 * a 304 without parsing JSON or publishing an equivalent React state tree.
 */
export const createConditionalScoreboardReader = (
  request: ConditionalRequest,
  maximumEntries: number = MAX_SCOREBOARD_VALIDATORS
) => {
  if (!Number.isSafeInteger(maximumEntries) || maximumEntries < 1) {
    throw new RangeError('maximumEntries must be a positive integer')
  }
  const validators = new Map<string, ValidatorEntry>()
  let generation = 0

  const retain = (path: string, etag: string, value: object, responseGeneration: number) => {
    validators.delete(path)
    validators.set(path, { etag, generation: responseGeneration, value: new WeakRef(value) })
    while (validators.size > maximumEntries) {
      const oldest = validators.keys().next().value
      if (oldest === undefined) break
      validators.delete(oldest)
    }
  }

  const read = async <T extends object>(path: string): Promise<T> => {
    const requestGeneration = ++generation
    let entry = validators.get(path)
    const retained = entry?.value.deref()
    if (entry && !retained) {
      validators.delete(path)
      entry = undefined
    }

    let response = await request(path, entry?.etag)
    const freshest = validators.get(path)
    if (freshest && freshest.generation > requestGeneration) {
      const freshestValue = freshest.value.deref()
      if (freshestValue) return freshestValue as T
      validators.delete(path)
    }
    if (response.status === 304) {
      if (entry && retained) {
        retain(path, entry.etag, retained, requestGeneration)
        return retained as T
      }
      // A weak value can disappear between request construction and response.
      // Retry once without a validator so 304 never becomes an empty board.
      validators.delete(path)
      response = await request(path)
    }
    if (response.status < 200 || response.status >= 300) {
      throw new Error(`unexpected conditional scoreboard status ${response.status}`)
    }
    const etag = response.etag?.trim()
    // A browser/proxy may expose a successfully revalidated HTTP-cache entry as
    // 200. Its unchanged validator is still authoritative, so do not decode or
    // publish the duplicate body.
    if (entry && retained && etag === entry.etag) {
      retain(path, entry.etag, retained, requestGeneration)
      return retained as T
    }
    const data = typeof response.data === 'string' ? JSON.parse(response.data) : response.data
    if (typeof data !== 'object' || data === null) {
      throw new TypeError('scoreboard response must be a JSON object')
    }
    if (etag) retain(path, etag, data, requestGeneration)
    else validators.delete(path)
    return data as T
  }

  return {
    read,
    validatorCount: () => validators.size,
  }
}
