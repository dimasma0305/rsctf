import { AxiosError, AxiosHeaders, AxiosInstance, InternalAxiosRequestConfig } from 'axios'
import { httpErrorStatus, retryAfterMilliseconds } from '@Utils/HttpError'
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

const proofCache = new Map<number, VpnProof>()
const proofFlights = new Map<number, Promise<VpnProof>>()
type MintFailure = { attempts: number; nextAllowedAt: number }
const mintFailures = new Map<number, MintFailure>()

const GAME_ID_MAX = 2_147_483_647
const MAX_TRACKED_GAMES = 128
const MAX_MINT_BACKOFF_MS = 60_000
export const EVENT_VPN_AUTH_REASON_HEADER = 'x-rsctf-auth-reason'
export const EVENT_VPN_AUTH_REASON = 'event-vpn'

const responseHeader = (error: unknown, name: string): string | null => {
  if (!error || typeof error !== 'object') return null
  const headers = (error as { response?: { headers?: unknown } }).response?.headers
  if (!headers || typeof headers !== 'object') return null
  const candidate = headers as { get?: (key: string) => unknown; [key: string]: unknown }
  const value = typeof candidate.get === 'function' ? candidate.get(name) : candidate[name] ?? candidate[name.toLowerCase()]
  return typeof value === 'string' ? value : typeof value === 'number' ? String(value) : null
}

export const isEventVpnUnauthorized = (error: unknown): boolean =>
  httpErrorStatus(error) === 401 &&
  responseHeader(error, EVENT_VPN_AUTH_REASON_HEADER)?.toLowerCase() === EVENT_VPN_AUTH_REASON

export const eventVpnMintRetryDelay = (
  error: unknown,
  attempts: number,
  random: () => number = Math.random,
  now: number = Date.now()
): number => {
  const retryAfter = retryAfterMilliseconds(error, now)
  if (retryAfter !== null) return Math.min(5 * 60_000, Math.max(250, retryAfter))
  const ceiling = Math.min(MAX_MINT_BACKOFF_MS, 1_000 * 2 ** Math.max(0, attempts - 1))
  const jitter = 0.75 + Math.min(1, Math.max(0, random())) * 0.5
  return Math.max(250, Math.round(ceiling * jitter))
}

const setBounded = <T>(map: Map<number, T>, gameId: number, value: T) => {
  map.delete(gameId)
  map.set(gameId, value)
  while (map.size > MAX_TRACKED_GAMES) {
    const oldest = map.keys().next().value
    if (oldest === undefined) break
    map.delete(oldest)
  }
}

const recordMintFailure = (gameId: number, error: unknown) => {
  const attempts = Math.min(16, (mintFailures.get(gameId)?.attempts ?? 0) + 1)
  setBounded(mintFailures, gameId, {
    attempts,
    nextAllowedAt: Date.now() + eventVpnMintRetryDelay(error, attempts),
  })
}

const mintCircuitIsOpen = (gameId: number): boolean =>
  (mintFailures.get(gameId)?.nextAllowedAt ?? 0) > Date.now()

/** Test-only state reset; production callers install one interceptor per API instance. */
export const resetEventVpnProofForTests = () => {
  proofCache.clear()
  proofFlights.clear()
  mintFailures.clear()
}

/** Keep this segment contract aligned with `middlewares/event_vpn.rs`. */
export const protectedEventGamePathId = (path: string): number | null => {
  const segments = path.split('/').filter(Boolean)
  if (segments[0]?.toLowerCase() !== 'api' || segments[1]?.toLowerCase() !== 'game') return null
  // Rust's i32 parser (used by both the middleware and Axum's Path extractor)
  // accepts an optional leading plus sign.
  if (!/^\+?\d+$/.test(segments[2] ?? '')) return null
  const gameId = Number(segments[2])
  if (!Number.isSafeInteger(gameId) || gameId <= 0 || gameId > GAME_ID_MAX) return null
  const suffix = segments[3]?.toLowerCase()
  return !suffix || suffix === 'vpn' || suffix === 'check' ? null : gameId
}

export const protectedEventGameId = (value: string | undefined, origin?: string): number | null => {
  if (!value) return null
  try {
    const expectedOrigin = new URL(origin ?? window.location.origin).origin
    const target = new URL(value, expectedOrigin)
    return target.origin === expectedOrigin ? protectedEventGamePathId(target.pathname) : null
  } catch {
    return null
  }
}

const responseData = <T>(value: unknown): T => {
  const body = value as { data?: unknown }
  // RequestResponse serializes its successful model directly today. Keeping
  // this fallback makes the proof bootstrap resilient to older API wrappers.
  return ((body?.data as { data?: unknown } | undefined)?.data ?? body?.data ?? value) as T
}

const mintProof = (instance: AxiosInstance, gameId: number, origin?: string): Promise<VpnProof> => {
  const existing = proofFlights.get(gameId)
  if (existing) return existing
  if (proofFlights.size >= MAX_TRACKED_GAMES) {
    throw new Error('Too many Event VPN proof exchanges are already active')
  }
  const flight = (async () => {
    try {
      const challengeResponse = await instance.post(`/api/game/${gameId}/vpn/challenge`)
      const challenge = responseData<VpnChallenge>(challengeResponse)
      const proofUrl = new URL(challenge.proofUrl, origin ?? window.location.origin)
      if (proofUrl.protocol !== 'https:') throw new Error('Event VPN proof URL must use HTTPS')
      const proofResponse = await instance.post(
        proofUrl.toString(),
        { challenge: challenge.challenge },
        { withCredentials: true, headers: { 'Content-Type': 'application/json' } }
      )
      const proof = responseData<VpnProof>(proofResponse)
      if (!proof.proof || !proof.proofHeader || proof.expiresAtUtc <= getServerNowMilliseconds()) {
        throw new Error('Event VPN returned an invalid proof')
      }
      setBounded(proofCache, gameId, proof)
      mintFailures.delete(gameId)
      return proof
    } catch (error) {
      recordMintFailure(gameId, error)
      throw error
    }
  })().finally(() => proofFlights.delete(gameId))
  proofFlights.set(gameId, flight)
  return flight
}

export const installEventVpnProof = (instance: AxiosInstance, origin?: string) => {
  instance.interceptors.request.use((config) => {
    const gameId = protectedEventGameId(config.url, origin)
    const proof = gameId === null ? undefined : proofCache.get(gameId)
    if (proof && proof.expiresAtUtc > getServerNowMilliseconds() + 1_000) {
      const headers = AxiosHeaders.from(config.headers)
      headers.set(proof.proofHeader, proof.proof)
      config.headers = headers
    }
    return config
  })

  instance.interceptors.response.use(undefined, async (error: AxiosError) => {
    const config = error.config as RetryConfig | undefined
    const gameId = protectedEventGameId(config?.url, origin)
    if (!isEventVpnUnauthorized(error) || gameId === null || !config || config.rsctfVpnProofRetry) {
      throw error
    }
    config.rsctfVpnProofRetry = true
    proofCache.delete(gameId)
    if (mintCircuitIsOpen(gameId)) throw error
    try {
      const proof = await mintProof(instance, gameId, origin)
      const headers = AxiosHeaders.from(config.headers)
      headers.set(proof.proofHeader, proof.proof)
      config.headers = headers
      return await instance.request(config)
    } catch (mintError) {
      // If the session expired between the protected request and its proof
      // bootstrap, surface that unlabelled 401 to the global auth handler.
      if (httpErrorStatus(mintError) === 401 && !isEventVpnUnauthorized(mintError)) {
        throw mintError
      }
      // Preserve the original protected request error so the existing global
      // UI can explain that this event requires its VPN.
      throw error
    }
  })
}
