export interface FingerprintCollectionOptions {
  signal?: AbortSignal
  probeTimeoutMs?: number
}

export class FingerprintCollectionError extends Error {
  readonly code: 'aborted' | 'required-signal-unavailable'

  constructor(code: FingerprintCollectionError['code'], message: string) {
    super(message)
    this.name = 'FingerprintCollectionError'
    this.code = code
  }
}

const DEFAULT_PROBE_TIMEOUT_MS = 8_000

const requiredSignalDependencies: Record<string, string[]> = {
  lie_count: ['lies'],
  trash_count: ['trash'],
  error_count: ['capturedErrors'],
  headless_rating: ['headless'],
  stealth_rating: ['headless'],
  like_headless_rating: ['headless'],
  platform_consistent: ['workerScope', 'navigator'],
  ua_consistent: ['workerScope', 'navigator'],
  webgl_consistent: ['workerScope', 'canvasWebgl'],
  resistance_extension: ['resistance'],
  resistance_privacy: ['resistance'],
}

export const assertRequiredFingerprintSignalsAvailable = (requiredSignals: string[], failed: Set<string>) => {
  const unavailable = requiredSignals.filter((signal) => {
    const dependencies = requiredSignalDependencies[signal]
    return !dependencies || dependencies.some((dependency) => failed.has(dependency))
  })
  if (unavailable.length > 0) {
    throw new FingerprintCollectionError(
      'required-signal-unavailable',
      `Required browser identity signal unavailable: ${unavailable.join(', ')}`
    )
  }
}

export const throwIfFingerprintCollectionAborted = (signal?: AbortSignal) => {
  if (signal?.aborted) {
    throw new FingerprintCollectionError('aborted', 'Browser identity collection was cancelled')
  }
}

/** Resolve one optional entropy probe without letting it reject the collection. */
export const settleFingerprintProbe = async <T>(
  name: string,
  operation: () => Promise<T> | T,
  failed: Set<string>,
  options: FingerprintCollectionOptions = {}
): Promise<T | undefined> => {
  throwIfFingerprintCollectionAborted(options.signal)
  const timeoutMs = Math.max(25, Math.min(options.probeTimeoutMs ?? DEFAULT_PROBE_TIMEOUT_MS, 30_000))
  let timeout: ReturnType<typeof setTimeout> | undefined
  let abortListener: (() => void) | undefined
  try {
    const cancellation = new Promise<never>((_, reject) => {
      timeout = setTimeout(() => reject(new Error(`Browser identity probe ${name} timed out`)), timeoutMs)
      if (options.signal) {
        abortListener = () =>
          reject(new FingerprintCollectionError('aborted', 'Browser identity collection was cancelled'))
        options.signal.addEventListener('abort', abortListener, { once: true })
      }
    })
    return await Promise.race([Promise.resolve().then(operation), cancellation])
  } catch (error) {
    if (error instanceof FingerprintCollectionError && error.code === 'aborted') throw error
    failed.add(name)
    return undefined
  } finally {
    if (timeout) clearTimeout(timeout)
    if (abortListener && options.signal) options.signal.removeEventListener('abort', abortListener)
  }
}
