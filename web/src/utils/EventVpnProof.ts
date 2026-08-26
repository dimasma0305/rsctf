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

export const protectedEventGameId = (value: string | undefined): number | null => {
  if (!value) return null
  const path = new URL(value, window.location.origin).pathname
  const match = path.match(/^\/api\/game\/(\d+)(?:\/([^/]+))?/i)
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

const mintProof = (instance: AxiosInstance, gameId: number): Promise<VpnProof> => {
  const existing = proofFlights.get(gameId)
  if (existing) return existing
  const flight = (async () => {
    const challengeResponse = await instance.post(`/api/game/${gameId}/vpn/challenge`)
    const challenge = responseData<VpnChallenge>(challengeResponse)
    const proofUrl = new URL(challenge.proofUrl, window.location.origin)
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

export const installEventVpnProof = (instance: AxiosInstance) => {
  instance.interceptors.request.use((config) => {
    const gameId = protectedEventGameId(config.url)
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
    const gameId = protectedEventGameId(config?.url)
    if (error.response?.status !== 401 || gameId === null || !config || config.rsctfVpnProofRetry) {
      throw error
    }
    config.rsctfVpnProofRetry = true
    proofCache.delete(gameId)
    try {
      const proof = await mintProof(instance, gameId)
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
