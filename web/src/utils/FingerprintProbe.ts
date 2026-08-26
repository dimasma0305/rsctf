export const FINGERPRINT_PROBE_NAMES = [
  'brave',
  'workerScope',
  'voices',
  'offlineAudioContext',
  'canvasWebgl',
  'canvas2d',
  'windowFeatures',
  'htmlElementVersion',
  'css',
  'cssMedia',
  'screen',
  'maths',
  'consoleErrors',
  'timezone',
  'clientRects',
  'fonts',
  'media',
  'svg',
  'resistance',
  'intl',
  'navigator',
  'headless',
  'features',
  'lies',
  'trash',
  'capturedErrors',
  'fingerprintHash',
] as const

export type FingerprintProbeName = (typeof FINGERPRINT_PROBE_NAMES)[number]
export type FingerprintProbeUnavailableReason = 'failed' | 'timeout' | 'unsupported'

export type FingerprintProbeResult<T> =
  | { status: 'available'; value: T }
  | { status: 'unavailable'; probe: FingerprintProbeName; reason: FingerprintProbeUnavailableReason }

export type FingerprintCollectionErrorCode = 'fingerprint-unavailable' | 'required-signal-unavailable'

export class FingerprintCollectionError extends Error {
  readonly retriable = true

  constructor(
    readonly code: FingerprintCollectionErrorCode,
    readonly unavailableSignals: string[] = []
  ) {
    super(
      code === 'required-signal-unavailable'
        ? 'Required device verification signals are unavailable. Check browser permissions and try again.'
        : 'Device verification is unavailable in this browser. Check browser permissions and try again.'
    )
    this.name = 'FingerprintCollectionError'
  }
}

const abortError = (signal?: AbortSignal): Error => {
  if (signal?.reason instanceof Error) return signal.reason
  return new DOMException('The operation was aborted.', 'AbortError')
}

export const isAbortError = (error: unknown): boolean => {
  if (error instanceof DOMException) return error.name === 'AbortError'
  if (!(error instanceof Error)) return false
  return (
    error.name === 'AbortError' ||
    error.name === 'CanceledError' ||
    (error as Error & { code?: string }).code === 'ERR_CANCELED'
  )
}

export const throwIfAborted = (signal?: AbortSignal): void => {
  if (signal?.aborted) throw abortError(signal)
}

export const runFingerprintProbe = async <T>(
  probe: FingerprintProbeName,
  collect: () => T | null | undefined | Promise<T | null | undefined>,
  options: { signal?: AbortSignal; timeoutMs: number }
): Promise<FingerprintProbeResult<T>> => {
  const { signal, timeoutMs } = options
  throwIfAborted(signal)

  return new Promise<FingerprintProbeResult<T>>((resolve, reject) => {
    let settled = false
    const finish = (result: FingerprintProbeResult<T>) => {
      if (settled) return
      settled = true
      clearTimeout(timeout)
      signal?.removeEventListener('abort', onAbort)
      resolve(result)
    }
    const onAbort = () => {
      if (settled) return
      settled = true
      clearTimeout(timeout)
      signal?.removeEventListener('abort', onAbort)
      reject(abortError(signal))
    }
    const timeout = setTimeout(
      () => finish({ status: 'unavailable', probe, reason: 'timeout' }),
      Math.max(1, timeoutMs)
    )

    signal?.addEventListener('abort', onAbort, { once: true })
    Promise.resolve()
      .then(collect)
      .then((value) => {
        if (value === undefined || value === null) {
          finish({ status: 'unavailable', probe, reason: 'unsupported' })
          return
        }
        finish({ status: 'available', value })
      })
      .catch(() => finish({ status: 'unavailable', probe, reason: 'failed' }))
  })
}

export const fingerprintProbeValue = <T>(result: FingerprintProbeResult<T>): T | undefined =>
  result.status === 'available' ? result.value : undefined

type FingerprintEvidence = {
  lies?: unknown
  headless?: unknown
  navigator?: unknown
  workerScope?: unknown
  canvasWebgl?: unknown
}

const asRecord = (value: unknown): Record<string, unknown> | undefined =>
  value !== null && typeof value === 'object' ? (value as Record<string, unknown>) : undefined

const nested = (value: unknown, ...keys: string[]): unknown => {
  let current = value
  for (const key of keys) {
    current = asRecord(current)?.[key]
  }
  return current
}

const finiteNumber = (value: unknown): number | undefined =>
  typeof value === 'number' && Number.isFinite(value) ? value : undefined

const nonEmptyString = (value: unknown): string | undefined =>
  typeof value === 'string' && value.trim().length > 0 ? value.trim() : undefined

const requiredSignalValue = (evidence: FingerprintEvidence, signal: string): string | undefined => {
  switch (signal) {
    case 'lie_count': {
      const count = finiteNumber(nested(evidence.lies, 'totalLies'))
      return count === undefined || count < 0 ? undefined : `${Math.trunc(count)}`
    }
    case 'headless_rating': {
      const rating = finiteNumber(nested(evidence.headless, 'headlessRating'))
      return rating === undefined || rating < 0 || rating > 100 ? undefined : `${Math.trunc(rating)}`
    }
    case 'platform_consistent': {
      const browser = nonEmptyString(nested(evidence.navigator, 'platform'))
      const worker = nonEmptyString(nested(evidence.workerScope, 'platform'))
      return browser && worker ? (browser === worker ? '1' : '0') : undefined
    }
    case 'ua_consistent': {
      const browser = nonEmptyString(nested(evidence.navigator, 'system'))
      const worker = nonEmptyString(nested(evidence.workerScope, 'system'))
      return browser && worker ? (browser === worker ? '1' : '0') : undefined
    }
    case 'webgl_consistent': {
      const browser = nonEmptyString(nested(evidence.canvasWebgl, 'parameters', 'UNMASKED_RENDERER_WEBGL'))
      const worker = nonEmptyString(nested(evidence.workerScope, 'webglRenderer'))
      return browser && worker ? (browser.includes(worker) ? '1' : '0') : undefined
    }
    default:
      return undefined
  }
}

export const collectRequiredFingerprintSignals = (
  evidence: FingerprintEvidence,
  requiredSignals: string[]
): Record<string, string> => {
  const signals: Record<string, string> = {}
  const unavailable: string[] = []

  for (const signal of requiredSignals) {
    const value = requiredSignalValue(evidence, signal)
    if (value === undefined) unavailable.push(signal)
    else signals[signal] = value
  }

  if (unavailable.length > 0) {
    throw new FingerprintCollectionError('required-signal-unavailable', unavailable)
  }
  return signals
}
