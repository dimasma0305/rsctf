import type { AxiosInstance, AxiosResponse } from 'axios'
import { useSyncExternalStore } from 'react'
import { useTicker } from '@Hooks/useTicker'

type Listener = () => void

const listeners = new Set<Listener>()
let offsetMilliseconds = 0
let liveSampleReady = false
let latestServerTime = Number.NEGATIVE_INFINITY
let bestRoundTripMilliseconds = Number.POSITIVE_INFINITY
let sampleWindowStartedAt = Number.NEGATIVE_INFINITY
const installedInstances = new WeakSet<AxiosInstance>()
const requestStartedAt = new WeakMap<object, number>()
const CLOCK_SAMPLE_WINDOW_MS = 5 * 60_000

const subscribe = (listener: Listener) => {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

const getSnapshot = () => offsetMilliseconds
const getReadySnapshot = () => liveSampleReady

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

/** Accept a response-stamped sample at receipt, preferring the lowest RTT in a bounded window. */
export const observeServerTime = (
  serverTime: number,
  receivedAt: number = Date.now(),
  startedAt: number = receivedAt
): boolean => {
  if (
    !Number.isFinite(serverTime) ||
    serverTime <= 0 ||
    !Number.isFinite(receivedAt) ||
    !Number.isFinite(startedAt) ||
    startedAt > receivedAt
  )
    return false
  if (serverTime < latestServerTime) return false

  const readinessChanged = !liveSampleReady
  liveSampleReady = true
  latestServerTime = serverTime
  if (
    !Number.isFinite(sampleWindowStartedAt) ||
    receivedAt < sampleWindowStartedAt ||
    receivedAt - sampleWindowStartedAt >= CLOCK_SAMPLE_WINDOW_MS
  ) {
    sampleWindowStartedAt = receivedAt
    bestRoundTripMilliseconds = Number.POSITIVE_INFINITY
  }

  const roundTripMilliseconds = receivedAt - startedAt
  if (roundTripMilliseconds > bestRoundTripMilliseconds) {
    if (readinessChanged) listeners.forEach((listener) => listener())
    return true
  }
  bestRoundTripMilliseconds = roundTripMilliseconds

  // API serverTime values are sampled near response creation, after handler
  // work. Anchoring them at the request midpoint would mistake server-side
  // processing for clock skew and can move lifecycle controls early.
  const nextOffset = serverTime - receivedAt
  if (nextOffset === offsetMilliseconds) {
    if (readinessChanged) listeners.forEach((listener) => listener())
    return true
  }
  offsetMilliseconds = nextOffset
  listeners.forEach((listener) => listener())
  return true
}

const observeResponse = (response: AxiosResponse) => {
  const receivedAt = Date.now()
  const startedAt = requestStartedAt.get(response.config) ?? receivedAt
  requestStartedAt.delete(response.config)
  const serverTime = modelServerTime(response.data)
  if (serverTime !== null) observeServerTime(serverTime, receivedAt, startedAt)
  return response
}

/** Install once on the shared generated API client so cached models never seed the clock. */
export const installServerClock = (instance: AxiosInstance) => {
  if (installedInstances.has(instance)) return
  installedInstances.add(instance)
  instance.interceptors.request.use((config) => {
    requestStartedAt.set(config, Date.now())
    return config
  })
  instance.interceptors.response.use(observeResponse)
}

/** One server-corrected shared clock for lifecycle labels, progress, and poll ownership. */
export const useServerNow = () => {
  const localNow = useTicker()
  const offset = useSyncExternalStore(subscribe, getSnapshot, getSnapshot)
  return localNow.add(offset, 'millisecond')
}

/** True only after a live HTTP response supplies an authoritative clock sample. */
export const useServerClockReady = () => useSyncExternalStore(subscribe, getReadySnapshot, getReadySnapshot)

export const serverClockTestApi = {
  reset: () => {
    offsetMilliseconds = 0
    liveSampleReady = false
    latestServerTime = Number.NEGATIVE_INFINITY
    bestRoundTripMilliseconds = Number.POSITIVE_INFINITY
    sampleWindowStartedAt = Number.NEGATIVE_INFINITY
  },
  offset: getSnapshot,
  ready: getReadySnapshot,
  bestRoundTrip: () => bestRoundTripMilliseconds,
  modelServerTime,
}
