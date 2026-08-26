import { AxiosError, AxiosHeaders, AxiosInstance, InternalAxiosRequestConfig } from 'axios'
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

const GAME_ID_MAX = 2_147_483_647

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
  const flight = (async () => {
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
    proofCache.set(gameId, proof)
    return proof
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
    if (error.response?.status !== 401 || gameId === null || !config || config.rsctfVpnProofRetry) {
      throw error
    }
    config.rsctfVpnProofRetry = true
    proofCache.delete(gameId)
    try {
      const proof = await mintProof(instance, gameId, origin)
      const headers = AxiosHeaders.from(config.headers)
      headers.set(proof.proofHeader, proof.proof)
      config.headers = headers
      return await instance.request(config)
    } catch {
      // Preserve the original protected request error so the existing global
      // session-expiry handling remains authoritative.
      throw error
    }
  })
}
