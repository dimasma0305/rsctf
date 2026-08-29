import type { WsrxInstance } from '@xdsec/wsrx'
import { WsrxState } from '@xdsec/wsrx'

export type WsrxTunnelPhase =
  'direct' | 'disconnected' | 'authorization' | 'requesting' | 'connecting' | 'checking' | 'ready' | 'unhealthy'
export type ProxyEntryMode = 'wsrx' | 'wss'
export type WsrxRefreshSource = 'automatic' | 'player'

export const DEFAULT_PROXY_ENTRY_MODE: ProxyEntryMode = 'wss'

interface WsrxConnectIntent {
  mode: ProxyEntryMode
  source: WsrxRefreshSource
  state: WsrxState
}

export const shouldConnectLocalWsrx = ({ mode, source, state }: WsrxConnectIntent) =>
  source === 'player' && mode === 'wsrx' && state !== WsrxState.Usable

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
