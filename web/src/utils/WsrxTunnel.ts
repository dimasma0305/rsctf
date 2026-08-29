import type { WsrxInstance } from '@xdsec/wsrx'
import { WsrxState } from '@xdsec/wsrx'

export type WsrxTunnelPhase =
  'direct' | 'disconnected' | 'authorization' | 'requesting' | 'connecting' | 'checking' | 'ready' | 'unhealthy'
export type ProxyEntryMode = 'wsrx' | 'wss'
export type WsrxRefreshSource = 'automatic' | 'player'

export const DEFAULT_PROXY_ENTRY_MODE: ProxyEntryMode = 'wss'
export const WSRX_CAPABILITY_RETRY_DELAY_MS = 30_000
export const WSRX_CAPABILITY_EXPIRY_MARGIN_MS = 5_000

export const isLatestWsrxCapabilityRequest = (requestSequence: number, latestSequence: number) =>
  requestSequence === latestSequence

export const getWsrxCapabilityRetryAt = (serverNow: number, expiresAt: number) => {
  const retryAt = Math.min(serverNow + WSRX_CAPABILITY_RETRY_DELAY_MS, expiresAt - WSRX_CAPABILITY_EXPIRY_MARGIN_MS)
  return retryAt > serverNow ? retryAt : null
}

export const shouldInvalidateWsrxCapability = (
  serverNow: number,
  scheduledExpiresAt: number,
  currentExpiresAt: number | null
) => currentExpiresAt === scheduledExpiresAt && serverNow >= scheduledExpiresAt

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
