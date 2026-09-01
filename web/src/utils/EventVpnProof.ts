import { AxiosError, AxiosHeaders, AxiosInstance, InternalAxiosRequestConfig } from 'axios'
import { retryAfterMilliseconds } from '@Utils/ProfileRetry'
import { getServerNowMilliseconds } from '@Utils/ServerClock'

type VpnChallenge = {
  challenge: string
  proofUrl: string
  proofHeader: string
  expiresAtUtc: number
}

type VpnProof = {
  proof: string
  proofHeader: string
  expiresAtUtc: number
}

type RetryConfig = InternalAxiosRequestConfig & { rsctfVpnProofRetry?: boolean }
type EventVpnFailureKind = 'disconnected' | 'rate-limited' | 'unavailable'
type ProofFailure = {
  attempts: number
  retryAt: number
  error: EventVpnAccessError
}

const MAX_FAILURES = 32
const MAX_BACKOFF_MS = 30_000
const MAX_RETRY_AFTER_MS = 5 * 60_000
const proofCache = new Map<number, VpnProof>()
const proofFlights = new Map<number, Promise<VpnProof>>()
const proofFailures = new Map<number, ProofFailure>()
let proofClient: AxiosInstance | null = null

/**
 * A protected Event-VPN request failed while the ordinary login session may
 * still be valid. Deliberately omit an HTTP `status`: global authentication
 * handlers must not reinterpret a disconnected tunnel or proof outage as an
 * expired account session.
 */
export class EventVpnAccessError extends Error {
  readonly kind: EventVpnFailureKind
  readonly retryAt: number
  override readonly cause: unknown

  constructor(kind: EventVpnFailureKind, message: string, retryAt: number, cause?: unknown) {
    super(message)
    this.name = 'EventVpnAccessError'
    this.kind = kind
    this.retryAt = retryAt
    this.cause = cause
  }
}

export const isEventVpnAccessError = (error: unknown): error is EventVpnAccessError =>
  error instanceof EventVpnAccessError

const browserOrigin = () => (typeof window === 'undefined' ? 'http://localhost' : window.location.origin)

export const protectedEventGameId = (value: string | undefined): number | null => {
  if (!value) return null
  const origin = browserOrigin()
  const url = new URL(value, origin)
  if (url.origin !== origin) return null
  const match = url.pathname.match(/^\/api\/game\/(\d+)(?:\/([^/]+))?/i)
  if (!match || !match[2] || match[2].toLowerCase() === 'vpn' || match[2].toLowerCase() === 'check') return null
  const gameId = Number(match[1])
  return Number.isSafeInteger(gameId) ? gameId : null
}

const responseData = <T>(value: unknown): T => {
  const body = value as { data?: unknown }
  // RequestResponse serializes its successful model directly today. Keeping
  // this fallback makes the proof bootstrap resilient to older API wrappers.
  return ((body?.data as { data?: unknown } | undefined)?.data ?? body?.data ?? value) as T
}

const errorStatus = (error: unknown) => {
  if (!error || typeof error !== 'object') return null
  const candidate = error as { status?: unknown; response?: { status?: unknown } }
  const status = candidate.response?.status ?? candidate.status
  return typeof status === 'number' && Number.isInteger(status) ? status : null
}

const forgetOldestFailure = () => {
  if (proofFailures.size < MAX_FAILURES) return
  const oldest = proofFailures.keys().next().value
  if (oldest !== undefined) proofFailures.delete(oldest)
}

const rememberFailure = (gameId: number, kind: EventVpnFailureKind, cause: unknown) => {
  const now = getServerNowMilliseconds()
  const attempts = Math.min(6, (proofFailures.get(gameId)?.attempts ?? 0) + 1)
  const jitter = 0.8 + Math.min(1, Math.max(0, Math.random())) * 0.4
  const backoff = Math.min(MAX_BACKOFF_MS, Math.round(1_000 * 2 ** (attempts - 1) * jitter))
  const retryAfter = Math.min(MAX_RETRY_AFTER_MS, retryAfterMilliseconds(cause, now) ?? 0)
  const retryAt = now + Math.max(backoff, retryAfter)
  const message =
    kind === 'disconnected'
      ? 'Connect to the event VPN, then retry this request.'
      : kind === 'rate-limited'
        ? 'Event VPN verification is temporarily rate limited. Retry after the indicated delay.'
        : 'Event VPN verification is temporarily unavailable. Retry shortly.'
  const error = new EventVpnAccessError(kind, message, retryAt, cause)
  if (!proofFailures.has(gameId)) forgetOldestFailure()
  // Refresh insertion order so the bounded map evicts the least recently
  // failing game rather than one that is actively recovering.
  proofFailures.delete(gameId)
  proofFailures.set(gameId, { attempts, retryAt, error })
  return error
}

const accessFailure = (gameId: number, cause: unknown, proofStage: boolean) => {
  const status = errorStatus(cause)
  const kind: EventVpnFailureKind =
    proofStage && status === 403 ? 'disconnected' : status === 429 ? 'rate-limited' : 'unavailable'
  return rememberFailure(gameId, kind, cause)
}

const liveProof = (gameId: number) => {
  const proof = proofCache.get(gameId)
  if (proof && proof.expiresAtUtc > getServerNowMilliseconds() + 1_000) return proof
  proofCache.delete(gameId)
  return undefined
}

const mintProof = (instance: AxiosInstance, gameId: number): Promise<VpnProof> => {
  const existing = proofFlights.get(gameId)
  if (existing) return existing
  const blocked = proofFailures.get(gameId)
  if (blocked && blocked.retryAt > getServerNowMilliseconds()) return Promise.reject(blocked.error)

  const flight = (async () => {
    let challengeResponse
    try {
      challengeResponse = await instance.post(`/api/game/${gameId}/vpn/challenge`)
    } catch (error) {
      // A 401 from the same-origin challenge endpoint is authoritative session
      // expiry. Preserve it so the existing login redirect remains correct.
      if (errorStatus(error) === 401) throw error
      throw accessFailure(gameId, error, false)
    }

    const challenge = responseData<VpnChallenge>(challengeResponse)
    const proofUrl = new URL(challenge.proofUrl, browserOrigin())
    if (proofUrl.protocol !== 'https:') {
      throw accessFailure(gameId, new Error('Event VPN proof URL must use HTTPS'), false)
    }

    let proofResponse
    try {
      proofResponse = await instance.post(
        proofUrl.toString(),
        { challenge: challenge.challenge },
        { withCredentials: true, headers: { 'Content-Type': 'application/json' } }
      )
    } catch (error) {
      // The VPN proof controller reserves 401 for an invalid/expired account
      // session or freshly issued challenge. Tunnel/source rejection is 403.
      if (errorStatus(error) === 401) throw error
      throw accessFailure(gameId, error, true)
    }
    const proof = responseData<VpnProof>(proofResponse)
    if (!proof.proof || !proof.proofHeader || proof.expiresAtUtc <= getServerNowMilliseconds()) {
      throw accessFailure(gameId, new Error('Event VPN returned an invalid proof'), true)
    }
    proofFailures.delete(gameId)
    proofCache.set(gameId, proof)
    return proof
  })().finally(() => proofFlights.delete(gameId))
  proofFlights.set(gameId, flight)
  return flight
}

const requestWithProof = (request: Request, proof: VpnProof | undefined) => {
  if (!proof) return request
  const headers = new Headers(request.headers)
  headers.set(proof.proofHeader, proof.proof)
  return new Request(request, { headers })
}

/** Native-fetch counterpart to the generated Axios interceptor. */
export const eventVpnFetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
  const normalizedInput = typeof input === 'string' ? new URL(input, browserOrigin()).toString() : input
  const request = new Request(normalizedInput, init)
  const gameId = protectedEventGameId(request.url)
  if (gameId === null) return fetch(request)

  // Clone before dispatch so a one-shot request body can be replayed exactly
  // once after proof minting. GET arena reads have no body, but the wrapper is
  // safe for every same-origin protected request.
  const retryRequest = request.clone()
  const response = await fetch(requestWithProof(request, liveProof(gameId)))
  if (response.status !== 401 || !proofClient) return response

  proofCache.delete(gameId)
  try {
    const proof = await mintProof(proofClient, gameId)
    return await fetch(requestWithProof(retryRequest, proof))
  } catch (error) {
    // Return the original protected 401 only when the challenge endpoint proved
    // the account session itself expired. Its native caller remains the owner
    // of the normal authentication response.
    if (!isEventVpnAccessError(error) && errorStatus(error) === 401) return response
    throw error
  }
}

export const installEventVpnProof = (instance: AxiosInstance) => {
  proofClient = instance
  instance.interceptors.request.use((config) => {
    const gameId = protectedEventGameId(config.url)
    const proof = gameId === null ? undefined : liveProof(gameId)
    if (proof) {
      const headers = AxiosHeaders.from(config.headers)
      headers.set(proof.proofHeader, proof.proof)
      config.headers = headers
    }
    return config
  })

  instance.interceptors.response.use(undefined, async (error: AxiosError) => {
    const config = error.config as RetryConfig | undefined
    const gameId = protectedEventGameId(config?.url)
    if (error.response?.status !== 401 || gameId === null || !config || config.rsctfVpnProofRetry) {
      throw error
    }
    config.rsctfVpnProofRetry = true
    proofCache.delete(gameId)
    const proof = await mintProof(instance, gameId)
    const headers = AxiosHeaders.from(config.headers)
    headers.set(proof.proofHeader, proof.proof)
    config.headers = headers
    return await instance.request(config)
  })
}

export const eventVpnProofTestApi = {
  reset: () => {
    proofCache.clear()
    proofFlights.clear()
    proofFailures.clear()
    proofClient = null
  },
  cachedGames: () => proofCache.size,
  failedGames: () => proofFailures.size,
}
