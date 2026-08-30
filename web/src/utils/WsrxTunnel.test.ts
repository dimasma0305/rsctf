import { WsrxState } from '@xdsec/wsrx'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import {
  DEFAULT_PROXY_ENTRY_MODE,
  getLocalWsrxTunnelAction,
  getWsrxCapabilityRetryAt,
  getWsrxTunnelPhase,
  isLatestWsrxCapabilityRequest,
  shouldInvalidateWsrxCapability,
  shouldConnectLocalWsrx,
} from './WsrxTunnel'

test('capability ownership rejects a late response and renewal retries stay bounded before expiry', () => {
  const initialRequest = 1
  const refreshRequest = 2
  assert.equal(isLatestWsrxCapabilityRequest(refreshRequest, refreshRequest), true)
  assert.equal(isLatestWsrxCapabilityRequest(initialRequest, refreshRequest), false)

  const now = 1_000_000
  assert.equal(getWsrxCapabilityRetryAt(now, now + 120_000), now + 30_000)
  assert.equal(getWsrxCapabilityRetryAt(now, now + 20_000), now + 15_000)
  const expiresAt = now + 5_000
  assert.equal(getWsrxCapabilityRetryAt(now, expiresAt), null)
  assert.equal(shouldInvalidateWsrxCapability(expiresAt - 1, expiresAt, expiresAt), false)
  assert.equal(shouldInvalidateWsrxCapability(expiresAt, expiresAt, expiresAt), true)
  assert.equal(shouldInvalidateWsrxCapability(expiresAt, expiresAt, expiresAt + 60_000), false)
})

const base = {
  isPlatformProxy: true,
  wsrxState: WsrxState.Usable,
  remoteEntry: 'wss://example.invalid/api/proxy/example?capability=secret',
  requestComplete: true,
  checkExpired: false,
  requestFailed: false,
}

test('a WSRX tunnel is not ready while its health probe reports -1', () => {
  const traffic = { remote: base.remoteEntry, local: '127.0.0.1:31337', latency: -1 }
  assert.equal(getWsrxTunnelPhase({ ...base, traffic }), 'checking')
  assert.equal(getWsrxTunnelPhase({ ...base, traffic, checkExpired: true }), 'unhealthy')
})

test('a verified desktop tunnel and a legacy daemon tunnel can become ready', () => {
  assert.equal(
    getWsrxTunnelPhase({
      ...base,
      traffic: { remote: base.remoteEntry, local: '127.0.0.1:31337', latency: 12 },
    }),
    'ready'
  )
  assert.equal(
    getWsrxTunnelPhase({
      ...base,
      traffic: { remote: base.remoteEntry, local: '127.0.0.1:31337' },
    }),
    'ready'
  )
})

test('missing, failed, and disconnected tunnels never appear ready', () => {
  assert.equal(getWsrxTunnelPhase({ ...base, traffic: undefined }), 'unhealthy')
  assert.equal(getWsrxTunnelPhase({ ...base, requestFailed: true }), 'unhealthy')
  assert.equal(getWsrxTunnelPhase({ ...base, wsrxState: WsrxState.Invalid }), 'disconnected')
  assert.equal(getWsrxTunnelPhase({ ...base, wsrxState: WsrxState.Pending }), 'authorization')
})

test('local listener lifecycle creates, reuses, and rebinds the requested scope', () => {
  const intent = {
    mode: 'wsrx' as const,
    state: WsrxState.Usable,
    remoteEntry: base.remoteEntry,
    allowLan: false,
  }

  assert.equal(getLocalWsrxTunnelAction(intent), 'create')
  assert.equal(getLocalWsrxTunnelAction({ ...intent, mode: 'wss' }), 'idle')
  assert.equal(getLocalWsrxTunnelAction({ ...intent, state: WsrxState.Invalid }), 'idle')
  assert.equal(getLocalWsrxTunnelAction({ ...intent, remoteEntry: '' }), 'idle')
  assert.equal(getLocalWsrxTunnelAction({ ...intent, localEntry: '127.0.0.1:31337' }), 'reuse')
  assert.equal(getLocalWsrxTunnelAction({ ...intent, localEntry: '0.0.0.0:31337' }), 'rebind')
  assert.equal(getLocalWsrxTunnelAction({ ...intent, allowLan: true, localEntry: '0.0.0.0:31337' }), 'reuse')
  assert.equal(getLocalWsrxTunnelAction({ ...intent, allowLan: true, localEntry: '127.0.0.1:31337' }), 'rebind')
})

test('instance UI listens for daemon updates and exposes WSS only through the explicit mode', () => {
  const provider = readFileSync('src/components/WsrxProvider.tsx', 'utf8')
  const entry = readFileSync('src/components/InstanceEntry.tsx', 'utf8')

  assert.match(provider, /onInstancesChange\(updateInstances\)/)
  assert.match(entry, /phase === 'ready' \? \(localTraffic\?\.local \?\? ''\) :/)
  assert.match(entry, /watchPendingTunnel\(wsrxRemoteEntry/)
  assert.doesNotMatch(entry, /setInterval\(\(\) => void wsrx\.sync\(\)/)
  assert.match(provider, /syncInFlight/)
  assert.match(provider, /ACCELERATED_SYNC_WINDOW_MS = 8_000/)
  assert.match(entry, /isWssMode \? wsrxRemoteEntry : localEntry/)
  assert.match(entry, /value=\{proxyEntryMode\}/)
  assert.match(entry, /getLocalWsrxTunnelAction\(/)
  assert.match(entry, /useEffect\(\(\) => \{\s+if \(tunnelRetrying\) return[\s\S]*?getLocalWsrxTunnelAction\(/)
  assert.match(entry, /action === 'reuse'[\s\S]*?setTunnelRequestComplete\(true\)/)
  assert.match(entry, /action === 'rebind'[\s\S]*?await wsrx\.delete\(localTraffic\.local\)/)
  assert.match(entry, /await wsrx\.delete\(localTraffic\.local\)/)
})

test('the optional local daemon is contacted only after an explicit player action', () => {
  const provider = readFileSync('src/components/WsrxProvider.tsx', 'utf8')
  const entry = readFileSync('src/components/InstanceEntry.tsx', 'utf8')
  const manager = readFileSync('src/components/WsrxManager.tsx', 'utf8')
  const optionsEffect = provider.match(
    /useEffect\(\(\) => \{\s+if \(!wsrxOptions[\s\S]*?wsrx\.setOptions\(getWsrxConfig\(wsrxOptions\)\)[\s\S]*?\}, \[[^\]]+\]\)/
  )

  assert.ok(optionsEffect)
  assert.doesNotMatch(optionsEffect[0], /doWsrxConnect/)
  assert.equal(DEFAULT_PROXY_ENTRY_MODE, 'wss')
  assert.equal(shouldConnectLocalWsrx({ mode: 'wsrx', source: 'automatic', state: WsrxState.Invalid }), false)
  assert.equal(shouldConnectLocalWsrx({ mode: 'wss', source: 'player', state: WsrxState.Invalid }), false)
  assert.equal(shouldConnectLocalWsrx({ mode: 'wsrx', source: 'player', state: WsrxState.Invalid }), true)
  assert.equal(shouldConnectLocalWsrx({ mode: 'wsrx', source: 'player', state: WsrxState.Usable }), false)
  assert.match(entry, /onRefreshProxyEntry\('automatic'\)/)
  assert.match(entry, /onRefreshProxyEntry\('player'\)/)
  assert.match(entry, /useServerClockTimeout\([\s\S]*?invalidateExpiredProxyCapability[\s\S]*?capabilityExpiresAt/)
  assert.match(entry, /setWsrxRemoteEntry\(''\)[\s\S]*?wsrx\.delete\(localTraffic\.local\)/)
  assert.match(manager, /applyWsrxOptions\(\{ \.\.\.debounced, name: wsrxOptions\.name \}\)/)
  assert.match(provider, /const applyWsrxOptions = useCallback\([\s\S]*?doWsrxConnect\(\)/)
})
