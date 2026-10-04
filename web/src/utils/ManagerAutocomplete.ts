export const MANAGER_AUTOCOMPLETE_MIN_CHARS = 2
export const MANAGER_AUTOCOMPLETE_MAX_CHARS = 64

export const normalizeManagerAutocompleteQuery = (query: string): string | null => {
  const normalized = query.trim()
  const length = Array.from(normalized).length
  if (length < MANAGER_AUTOCOMPLETE_MIN_CHARS || length > MANAGER_AUTOCOMPLETE_MAX_CHARS) return null
  if (
    Array.from(normalized).some((character) => {
      const codepoint = character.codePointAt(0) ?? 0
      return codepoint <= 0x1f || (codepoint >= 0x7f && codepoint <= 0x9f)
    })
  )
    return null
  return normalized
}

export interface AutocompleteRequestHandlers<Result> {
  setLoading: (loading: boolean) => void
  setResults: (results: Result) => void
  onError: (error: unknown) => void
}

/**
 * Own the single browser request whose result may update an autocomplete.
 * Abort reduces server work; the generation check is still authoritative when
 * a transport or test double resolves after ignoring its abort signal.
 */
export const createLatestAutocompleteRequests = () => {
  let generation = 0
  let active: AbortController | null = null

  const invalidate = () => {
    generation += 1
    active?.abort()
    active = null
  }

  return {
    async run<Result>(
      request: (signal: AbortSignal) => Promise<Result>,
      handlers: AutocompleteRequestHandlers<Result>
    ): Promise<void> {
      invalidate()
      const requestGeneration = generation
      const controller = new AbortController()
      active = controller
      handlers.setLoading(true)

      const isCurrent = () => generation === requestGeneration && active === controller && !controller.signal.aborted
      try {
        const results = await request(controller.signal)
        if (isCurrent()) handlers.setResults(results)
      } catch (error) {
        if (isCurrent()) handlers.onError(error)
      } finally {
        if (isCurrent()) {
          active = null
          handlers.setLoading(false)
        }
      }
    },
    invalidate,
    pending: () => (active === null ? 0 : 1),
  }
}
