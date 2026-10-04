import { WsrxState } from '@xdsec/wsrx'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import {
  DEFAULT_PROXY_ENTRY_MODE,
  getLocalWsrxTunnelAction,
  getWsrxCapabilityBackoffMilliseconds,
  getWsrxCapabilityNextBatchDelay,
  getWsrxCapabilityRetryDelay,
  getWsrxCapabilityRetryAt,
  getWsrxListenerDrainDelay,
  getWsrxRetryAfterMilliseconds,
  getWsrxTunnelPhase,
  isLatestWsrxCapabilityRequest,
  isRetryableWsrxCapabilityStatus,
  isWsrxReplacementReady,
  shouldDeletePreparedWsrxListener,
  shouldInvalidateWsrxCapability,
  shouldConnectLocalWsrx,
  shouldKeepWsrxListener,
  WSRX_CAPABILITY_MAX_RETRY_DELAY_MS,
  WSRX_MAX_TIMER_DELAY_MS,
  WSRX_PROXY_SESSION_DRAIN_MS,
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

test('renewal retry policy is bounded, honors Retry-After, and stops on terminal client failures', () => {
  const now = Date.parse('2026-08-30T09:00:00Z')
  assert.equal(getWsrxRetryAfterMilliseconds('3', now), 3_000)
  assert.equal(getWsrxRetryAfterMilliseconds('Sun, 30 Aug 2026 09:00:04 GMT', now), 4_000)
  assert.equal(getWsrxRetryAfterMilliseconds('120', now), 120_000)
  assert.equal(getWsrxRetryAfterMilliseconds('9999999999', now), WSRX_MAX_TIMER_DELAY_MS)
  assert.equal(getWsrxRetryAfterMilliseconds('invalid', now), null)
  assert.equal(getWsrxCapabilityBackoffMilliseconds(0, 1, '3', now), 3_000)
  assert.ok(getWsrxCapabilityBackoffMilliseconds(99, 1, undefined, now) <= WSRX_CAPABILITY_MAX_RETRY_DELAY_MS)
  assert.equal(getWsrxCapabilityRetryDelay(0, 1, '3', now, now + 4_000), 3_000)
  assert.equal(getWsrxCapabilityRetryDelay(0, 1, '120', now, now + 60_000), null)
  assert.equal(getWsrxCapabilityRetryDelay(0, 1, 'Sun, 30 Aug 2026 09:02:00 GMT', now, now + 60_000), null)
  assert.equal(getWsrxCapabilityNextBatchDelay(now, now + 120_000, 3_000), 30_000)
  assert.equal(getWsrxCapabilityNextBatchDelay(now, now + 20_000, 3_000), 15_000)
  assert.equal(getWsrxCapabilityNextBatchDelay(now, now + 20_000, 16_000), null)
  assert.equal(getWsrxCapabilityNextBatchDelay(now, now + 20_000, Number.NaN), null)

  assert.equal(isRetryableWsrxCapabilityStatus(undefined), true)
  assert.equal(isRetryableWsrxCapabilityStatus(408), true)
  assert.equal(isRetryableWsrxCapabilityStatus(429), true)
  assert.equal(isRetryableWsrxCapabilityStatus(503), true)
  assert.equal(isRetryableWsrxCapabilityStatus(400), false)
  assert.equal(isRetryableWsrxCapabilityStatus(403), false)
})

test('replacement readiness is strict and the preceding listener drains after bounded stream lifetime', () => {
  const traffic = { remote: 'wss://replacement.invalid', local: '127.0.0.1:31337' }
  assert.equal(isWsrxReplacementReady(undefined), false)
  assert.equal(isWsrxReplacementReady(traffic), false)
  assert.equal(isWsrxReplacementReady({ ...traffic, latency: -1 }), false)
  assert.equal(isWsrxReplacementReady({ ...traffic, latency: 0 }), true)

  const now = 1_000_000
  assert.equal(getWsrxListenerDrainDelay(now, null), WSRX_PROXY_SESSION_DRAIN_MS)
  assert.equal(getWsrxListenerDrainDelay(now, now + 30_000), WSRX_PROXY_SESSION_DRAIN_MS + 30_000)
})

test('a listener survives only while its exact async owner and requested settings remain current', () => {
  const current = {
    mounted: true,
    ownerCurrent: true,
    mode: 'wsrx' as const,
    state: WsrxState.Usable,
    allowLan: false,
    requestedAllowLan: false,
  }
  assert.equal(shouldKeepWsrxListener(current), true)
  assert.equal(shouldKeepWsrxListener({ ...current, mounted: false }), false)
  assert.equal(shouldKeepWsrxListener({ ...current, ownerCurrent: false }), false)
  assert.equal(shouldKeepWsrxListener({ ...current, mode: 'wss' }), false)
  assert.equal(shouldKeepWsrxListener({ ...current, state: WsrxState.Invalid }), false)
  assert.equal(shouldKeepWsrxListener({ ...current, allowLan: true }), false)
})

test('switching to WSS removes only the unused prepared replacement listener', () => {
  assert.equal(shouldDeletePreparedWsrxListener(false, '127.0.0.1:41000', '127.0.0.1:31337'), true)
  assert.equal(shouldDeletePreparedWsrxListener(true, '127.0.0.1:41000', '127.0.0.1:31337'), false)
  assert.equal(shouldDeletePreparedWsrxListener(false, undefined, '127.0.0.1:31337'), false)
  assert.equal(shouldDeletePreparedWsrxListener(false, '127.0.0.1:31337', '127.0.0.1:31337'), false)
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
  assert.match(entry, /watchPendingTunnel\(pendingWsrxRemoteEntry/)
  assert.match(entry, /isWsrxReplacementReady\(pendingTraffic\)/)
  assert.match(provider, /scheduleTunnelDrain/)
  assert.match(provider, /for \(const local of drainingLocals\) void wsrx\.delete\(local\)/)
  assert.match(provider, /typeof instance\.latency === 'number' && instance\.latency >= 0/)
  assert.doesNotMatch(entry, /setInterval\(\(\) => void wsrx\.sync\(\)/)
  assert.match(provider, /syncInFlight/)
  assert.match(provider, /ACCELERATED_SYNC_WINDOW_MS = 8_000/)
  assert.match(entry, /isWssMode \? wsrxRemoteEntry : localEntry/)
  assert.match(entry, /value=\{proxyEntryMode\}/)
  assert.match(entry, /getLocalWsrxTunnelAction\(/)
  assert.match(entry, /useEffect\(\(\) => \{[\s\S]*?getLocalWsrxTunnelAction\(/)
  assert.match(entry, /action === 'reuse'[\s\S]*?setTunnelRequestComplete\(true\)/)
  assert.match(entry, /action === 'rebind'[\s\S]*?await wsrx\.delete\(existingLocal\)/)
  assert.match(entry, /await wsrx\.delete\(added\.local\)/)
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
  assert.match(entry, /setWsrxRemoteEntry\(''\)[\s\S]*?drainLocalListener\(expiredLocal, capabilityExpiresAt\)/)
  assert.match(manager, /applyWsrxOptions\(\{ \.\.\.debounced, name: wsrxOptions\.name \}\)/)
  assert.match(provider, /const applyWsrxOptions = useCallback\([\s\S]*?doWsrxConnect\(\)/)
})
