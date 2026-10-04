import type { AxiosInstance, AxiosResponse } from 'axios'
import { useEffect, useRef, useSyncExternalStore } from 'react'
import { useTicker } from '@Hooks/useTicker'

type Listener = () => void

const listeners = new Set<Listener>()
let offsetMilliseconds = 0
let liveSampleReady = false
let nextRequestSequence = 0
let latestAcceptedRequestSequence = 0
let bestRoundTripMilliseconds = Number.POSITIVE_INFINITY
let offsetSampleRoundTripMilliseconds = Number.POSITIVE_INFINITY
let sampleWindowStartedAt = Number.NEGATIVE_INFINITY
const installedInstances = new WeakSet<AxiosInstance>()
const requestTiming = new WeakMap<object, { sequence: number; startedAt: number }>()
const CLOCK_SAMPLE_WINDOW_MS = 5 * 60_000
const CLOCK_CORRECTION_NOISE_MARGIN_MS = 1_000

const httpUrl = (value: string, base?: string): URL | null => {
  try {
    const parsed = base === undefined ? new URL(value) : new URL(value, base)
    return parsed.protocol === 'http:' || parsed.protocol === 'https:' ? parsed : null
  } catch {
    return null
  }
}

const canonicalApiOrigin = (instance: AxiosInstance): string | null => {
  if (typeof window !== 'undefined') return httpUrl(window.location.href)?.origin ?? null
  const baseUrl = instance.defaults.baseURL
  return typeof baseUrl === 'string' ? (httpUrl(baseUrl)?.origin ?? null) : null
}

const stringProperty = (value: unknown, property: string): string | null => {
  if (!value || typeof value !== 'object') return null
  try {
    const candidate = (value as Record<string, unknown>)[property]
    return typeof candidate === 'string' && candidate.length > 0 ? candidate : null
  } catch {
    return null
  }
}

/**
 * Axios exposes the redirect-resolved URL as XHR.responseURL in browsers and
 * IncomingMessage.responseUrl when its Node adapter uses follow-redirects.
 * Request.url from the fetch adapter is deliberately excluded because it is
 * the pre-redirect URL and cannot prove which origin supplied the response.
 */
const finalResponseUrl = (response: AxiosResponse): string | null => {
  const request = response.request as unknown
  const browserUrl = stringProperty(request, 'responseURL')
  if (browserUrl !== null) return browserUrl
  const incomingResponse = request && typeof request === 'object' ? (request as Record<string, unknown>).res : null
  return stringProperty(incomingResponse, 'responseUrl')
}

const isCanonicalApiUrl = (value: string, origin: string): boolean => {
  const parsed = httpUrl(value, origin)
  return (
    parsed !== null && parsed.origin === origin && (parsed.pathname === '/api' || parsed.pathname.startsWith('/api/'))
  )
}

const isTrustedApiResponse = (instance: AxiosInstance, origin: string | null, response: AxiosResponse): boolean => {
  if (origin === null) return false
  try {
    const configuredUrl = instance.getUri(response.config)
    const resolvedUrl = finalResponseUrl(response)
    return (
      typeof configuredUrl === 'string' &&
      configuredUrl.length > 0 &&
      resolvedUrl !== null &&
      isCanonicalApiUrl(configuredUrl, origin) &&
      isCanonicalApiUrl(resolvedUrl, origin)
    )
  } catch {
    return false
  }
}

const allocateRequestSequence = () => {
  nextRequestSequence += 1
  return nextRequestSequence
}

const subscribe = (listener: Listener) => {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

const getSnapshot = () => offsetMilliseconds
const getReadySnapshot = () => liveSampleReady
const getAuthoritativeOffsetSnapshot = () => (liveSampleReady ? offsetMilliseconds : null)

/** Current authoritative-clock estimate for non-React expiry calculations. */
export const getServerNowMilliseconds = (localNow: number = Date.now()) => localNow + offsetMilliseconds
export const hasLiveServerClockSample = () => liveSampleReady

const finiteServerTime = (value: unknown): number | null =>
  typeof value === 'number' && Number.isFinite(value) && value > 0 ? value : null

const modelServerTime = (value: unknown): number | null => {
  if (!value || typeof value !== 'object') return null
  const record = value as Record<string, unknown>
  const direct = finiteServerTime(record.serverTime)
  if (direct !== null) return direct

  const data = record.data
  if (Array.isArray(data)) return modelServerTime(data[0])
  if (data && typeof data === 'object') return modelServerTime(data)
  if (Array.isArray(value)) return modelServerTime(value[0])
  return null
}

/**
 * Accept a response-stamped sample at receipt, normally preferring the lowest
 * RTT in a bounded window. A slower sample may replace the offset only when
 * its disagreement exceeds both samples' RTT uncertainty plus a noise margin.
 * The sequence is captured before dispatch so a late response is fenced
 * without assuming clocks are monotonic across API replicas.
 */
export const observeServerTime = (
  serverTime: number,
  receivedAt: number = Date.now(),
  startedAt: number = receivedAt,
  requestSequence: number = allocateRequestSequence()
): boolean => {
  if (
    !Number.isFinite(serverTime) ||
    serverTime <= 0 ||
    !Number.isFinite(receivedAt) ||
    !Number.isFinite(startedAt) ||
    startedAt > receivedAt ||
    !Number.isSafeInteger(requestSequence) ||
    requestSequence <= 0
  )
    return false
  nextRequestSequence = Math.max(nextRequestSequence, requestSequence)
  if (requestSequence <= latestAcceptedRequestSequence) return false

  const readinessChanged = !liveSampleReady
  liveSampleReady = true
  latestAcceptedRequestSequence = requestSequence
  if (
    !Number.isFinite(sampleWindowStartedAt) ||
    receivedAt < sampleWindowStartedAt ||
    receivedAt - sampleWindowStartedAt >= CLOCK_SAMPLE_WINDOW_MS
  ) {
    sampleWindowStartedAt = receivedAt
    bestRoundTripMilliseconds = Number.POSITIVE_INFINITY
  }

  const roundTripMilliseconds = receivedAt - startedAt
  // API serverTime values are sampled near response creation, after handler
  // work. Anchoring them at the request midpoint would mistake server-side
  // processing for clock skew and can move lifecycle controls early.
  const nextOffset = serverTime - receivedAt
  const improvesRoundTrip = roundTripMilliseconds <= bestRoundTripMilliseconds
  const correctionUncertaintyMilliseconds =
    Math.max(roundTripMilliseconds, offsetSampleRoundTripMilliseconds) + CLOCK_CORRECTION_NOISE_MARGIN_MS
  const isMeaningfulCorrection =
    liveSampleReady &&
    Number.isFinite(offsetSampleRoundTripMilliseconds) &&
    Math.abs(nextOffset - offsetMilliseconds) > correctionUncertaintyMilliseconds
  if (!improvesRoundTrip && !isMeaningfulCorrection) {
    if (readinessChanged) listeners.forEach((listener) => listener())
    return true
  }
  if (improvesRoundTrip) bestRoundTripMilliseconds = roundTripMilliseconds
  offsetSampleRoundTripMilliseconds = roundTripMilliseconds

  if (nextOffset === offsetMilliseconds) {
    if (readinessChanged) listeners.forEach((listener) => listener())
    return true
  }
  offsetMilliseconds = nextOffset
  listeners.forEach((listener) => listener())
  return true
}

const observeResponse = (instance: AxiosInstance, origin: string | null, response: AxiosResponse) => {
  const receivedAt = Date.now()
  const timing = requestTiming.get(response.config) ?? {
    sequence: allocateRequestSequence(),
    startedAt: receivedAt,
  }
  requestTiming.delete(response.config)
  if (!isTrustedApiResponse(instance, origin, response)) return response
  const serverTime = modelServerTime(response.data)
  if (serverTime !== null) observeServerTime(serverTime, receivedAt, timing.startedAt, timing.sequence)
  return response
}

/** Install once on the shared generated API client so cached models never seed the clock. */
export const installServerClock = (instance: AxiosInstance) => {
  if (installedInstances.has(instance)) return
  installedInstances.add(instance)
  const origin = canonicalApiOrigin(instance)
  instance.interceptors.request.use((config) => {
    requestTiming.set(config, { sequence: allocateRequestSequence(), startedAt: Date.now() })
    return config
  })
  instance.interceptors.response.use((response) => observeResponse(instance, origin, response))
}

/** One server-corrected shared clock for lifecycle labels, progress, and poll ownership. */
export const useServerNow = () => {
  const localNow = useTicker()
  const offset = useSyncExternalStore(subscribe, getSnapshot, getSnapshot)
  return localNow.add(offset, 'millisecond')
}

/** True only after a live HTTP response supplies an authoritative clock sample. */
export const useServerClockReady = () => useSyncExternalStore(subscribe, getReadySnapshot, getReadySnapshot)

/**
 * The latest authoritative offset, or null until a live response supplies one.
 * Unlike the readiness-only hook, this snapshot also changes after a better
 * clock sample corrects an earlier estimate.
 */
export const useServerClockOffset = () =>
  useSyncExternalStore(subscribe, getAuthoritativeOffsetSnapshot, getAuthoritativeOffsetSnapshot)

/**
 * Schedule one server-time deadline only after the shared clock is live, then
 * replace it whenever a better sample corrects the offset. This is deliberately
 * a one-shot timeout rather than another polling loop.
 */
export const useServerClockTimeout = (
  callback: () => void,
  targetServerTimeMilliseconds: number | null | undefined,
  advanceMilliseconds: number = 0,
  minimumDelayMilliseconds: number = 0
) => {
  const authoritativeOffset = useServerClockOffset()
  const callbackRef = useRef(callback)

  useEffect(() => {
    callbackRef.current = callback
  }, [callback])

  useEffect(() => {
    if (
      authoritativeOffset === null ||
      typeof targetServerTimeMilliseconds !== 'number' ||
      !Number.isFinite(targetServerTimeMilliseconds) ||
      !Number.isFinite(advanceMilliseconds) ||
      advanceMilliseconds < 0 ||
      !Number.isFinite(minimumDelayMilliseconds) ||
      minimumDelayMilliseconds < 0
    )
      return

    const serverNow = Date.now() + authoritativeOffset
    const delay = Math.max(targetServerTimeMilliseconds - serverNow - advanceMilliseconds, minimumDelayMilliseconds)
    const timeout = setTimeout(() => callbackRef.current(), delay)
    return () => clearTimeout(timeout)
  }, [advanceMilliseconds, authoritativeOffset, minimumDelayMilliseconds, targetServerTimeMilliseconds])
}

export const serverClockTestApi = {
  reset: () => {
    offsetMilliseconds = 0
    liveSampleReady = false
    nextRequestSequence = 0
    latestAcceptedRequestSequence = 0
    bestRoundTripMilliseconds = Number.POSITIVE_INFINITY
    offsetSampleRoundTripMilliseconds = Number.POSITIVE_INFINITY
    sampleWindowStartedAt = Number.NEGATIVE_INFINITY
  },
  offset: getSnapshot,
  ready: getReadySnapshot,
  bestRoundTrip: () => bestRoundTripMilliseconds,
  modelServerTime,
}
