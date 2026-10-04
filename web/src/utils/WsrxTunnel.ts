import type { WsrxInstance } from '@xdsec/wsrx'
import { WsrxState } from '@xdsec/wsrx'

export type WsrxTunnelPhase =
  'direct' | 'disconnected' | 'authorization' | 'requesting' | 'connecting' | 'checking' | 'ready' | 'unhealthy'
export type ProxyEntryMode = 'wsrx' | 'wss'
export type WsrxRefreshSource = 'automatic' | 'player'

export const DEFAULT_PROXY_ENTRY_MODE: ProxyEntryMode = 'wss'
export const WSRX_CAPABILITY_RETRY_DELAY_MS = 30_000
export const WSRX_CAPABILITY_EXPIRY_MARGIN_MS = 5_000
export const WSRX_CAPABILITY_MAX_RETRY_DELAY_MS = 10_000
export const WSRX_PROXY_SESSION_DRAIN_MS = 30 * 60_000
export const WSRX_MAX_TIMER_DELAY_MS = 2_147_000_000

export const isLatestWsrxCapabilityRequest = (requestSequence: number, latestSequence: number) =>
  requestSequence === latestSequence

export const getWsrxCapabilityRetryAt = (serverNow: number, expiresAt: number) => {
  const retryAt = Math.min(serverNow + WSRX_CAPABILITY_RETRY_DELAY_MS, expiresAt - WSRX_CAPABILITY_EXPIRY_MARGIN_MS)
  return retryAt > serverNow ? retryAt : null
}

export const getWsrxCapabilityNextBatchDelay = (serverNow: number, expiresAt: number, retryDelay: number) => {
  const retryAt = getWsrxCapabilityRetryAt(serverNow, expiresAt)
  if (retryAt === null || !Number.isFinite(retryDelay) || retryDelay < 0) return null
  const delay = Math.max(retryDelay, retryAt - serverNow)
  return delay <= expiresAt - WSRX_CAPABILITY_EXPIRY_MARGIN_MS - serverNow ? delay : null
}

export const shouldInvalidateWsrxCapability = (
  serverNow: number,
  scheduledExpiresAt: number,
  currentExpiresAt: number | null
) => currentExpiresAt === scheduledExpiresAt && serverNow >= scheduledExpiresAt

export const isRetryableWsrxCapabilityStatus = (status: number | undefined) =>
  status === undefined || status === 408 || status === 429 || status >= 500

export const getWsrxRetryAfterMilliseconds = (
  value: unknown,
  now: number,
  maximumDelay: number = WSRX_MAX_TIMER_DELAY_MS
): number | null => {
  if (typeof value !== 'string' && typeof value !== 'number') return null
  const normalized = String(value).trim()
  if (!normalized) return null

  const seconds = /^\d+$/.test(normalized) ? Number.parseInt(normalized, 10) : Number.NaN
  const requestedDelay = Number.isFinite(seconds) ? seconds * 1000 : Date.parse(normalized) - now
  if (!Number.isFinite(requestedDelay)) return null
  return Math.min(Math.max(requestedDelay, 0), maximumDelay)
}

export const getWsrxCapabilityBackoffMilliseconds = (
  attempt: number,
  generation: number,
  retryAfter: unknown,
  now: number
) => {
  const requestedDelay = getWsrxRetryAfterMilliseconds(retryAfter, now)
  if (requestedDelay !== null) return requestedDelay

  const exponent = Math.min(Math.max(Math.trunc(attempt), 0), 6)
  const base = Math.min(250 * 2 ** exponent, WSRX_CAPABILITY_MAX_RETRY_DELAY_MS)
  const stableJitter = 0.5 + ((Math.abs(Math.trunc(generation)) % 17) + 1) / 36
  return Math.max(50, Math.floor(base * stableJitter))
}

export const getWsrxCapabilityRetryDelay = (
  attempt: number,
  generation: number,
  retryAfter: unknown,
  now: number,
  latestRetryAt: number | null
): number | null => {
  const delay = getWsrxCapabilityBackoffMilliseconds(attempt, generation, retryAfter, now)
  if (latestRetryAt !== null && (latestRetryAt <= now || delay > latestRetryAt - now)) return null
  return delay
}

export const isWsrxReplacementReady = (traffic: WsrxInstance | undefined) =>
  !!traffic?.local && typeof traffic.latency === 'number' && traffic.latency >= 0

interface WsrxListenerOwnership {
  mounted: boolean
  ownerCurrent: boolean
  mode: ProxyEntryMode
  state: WsrxState
  allowLan: boolean
  requestedAllowLan: boolean
}

export const shouldKeepWsrxListener = ({
  mounted,
  ownerCurrent,
  mode,
  state,
  allowLan,
  requestedAllowLan,
}: WsrxListenerOwnership) =>
  mounted && ownerCurrent && mode === 'wsrx' && state === WsrxState.Usable && allowLan === requestedAllowLan

export const shouldDeletePreparedWsrxListener = (
  replacementReady: boolean,
  preparedLocal: string | undefined,
  precedingLocal: string | undefined
) => !replacementReady && !!preparedLocal && preparedLocal !== precedingLocal

export const getWsrxListenerDrainDelay = (serverNow: number, capabilityExpiresAt: number | null) =>
  Math.max(0, (capabilityExpiresAt ?? serverNow) - serverNow) + WSRX_PROXY_SESSION_DRAIN_MS

interface WsrxConnectIntent {
  mode: ProxyEntryMode
  source: WsrxRefreshSource
  state: WsrxState
}

export const shouldConnectLocalWsrx = ({ mode, source, state }: WsrxConnectIntent) =>
  source === 'player' && mode === 'wsrx' && state !== WsrxState.Usable

export type LocalWsrxTunnelAction = 'idle' | 'create' | 'rebind' | 'reuse'

interface LocalWsrxTunnelIntent {
  mode: ProxyEntryMode
  state: WsrxState
  remoteEntry: string
  localEntry?: string
  allowLan: boolean
}

export const getLocalWsrxTunnelAction = ({
  mode,
  state,
  remoteEntry,
  localEntry,
  allowLan,
}: LocalWsrxTunnelIntent): LocalWsrxTunnelAction => {
  if (mode !== 'wsrx' || state !== WsrxState.Usable || remoteEntry.length === 0) return 'idle'
  if (!localEntry) return 'create'

  const desiredHost = allowLan ? '0.0.0.0' : '127.0.0.1'
  return localEntry.startsWith(`${desiredHost}:`) ? 'reuse' : 'rebind'
}

interface WsrxTunnelPhaseInput {
  isPlatformProxy: boolean
  wsrxState: WsrxState
  remoteEntry: string
  traffic?: WsrxInstance
  requestComplete: boolean
  checkExpired: boolean
  requestFailed: boolean
}

export const getWsrxTunnelPhase = ({
  isPlatformProxy,
  wsrxState,
  remoteEntry,
  traffic,
  requestComplete,
  checkExpired,
  requestFailed,
}: WsrxTunnelPhaseInput): WsrxTunnelPhase => {
  if (!isPlatformProxy) return 'direct'
  if (wsrxState === WsrxState.Pending) return 'authorization'
  if (wsrxState !== WsrxState.Usable) return 'disconnected'
  if (requestFailed) return 'unhealthy'
  if (!remoteEntry) return 'requesting'
  if (!requestComplete) return 'connecting'
  if (!traffic?.local) return 'unhealthy'
  if (traffic.latency === -1) return checkExpired ? 'unhealthy' : 'checking'
  return 'ready'
}
