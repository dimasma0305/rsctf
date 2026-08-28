import { WsrxState } from '@xdsec/wsrx'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { getWsrxTunnelPhase } from './WsrxTunnel'

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

test('instance UI listens for daemon updates and exposes WSS only through the explicit mode', () => {
  const provider = readFileSync('src/components/WsrxProvider.tsx', 'utf8')
  const entry = readFileSync('src/components/InstanceEntry.tsx', 'utf8')

  assert.match(provider, /onInstancesChange\(updateInstances\)/)
  assert.match(entry, /phase === 'ready' \? \(localTraffic\?\.local \?\? ''\) :/)
  assert.doesNotMatch(entry, /setInterval\(/)
  assert.match(provider, /createWsrxReadinessScheduler/)
  assert.match(provider, /instance\.latency === -1/)
  assert.match(entry, /wsrxReadinessExpired\.has\(wsrxRemoteEntry\)/)
  assert.match(entry, /type ProxyEntryMode = 'wsrx' \| 'wss'/)
  assert.match(entry, /isWssMode \? wsrxRemoteEntry : localEntry/)
  assert.match(entry, /value=\{proxyEntryMode\}/)
  assert.match(entry, /drainingLocals\.current\.add\(oldLocal\)/)
  assert.match(entry, /void wsrx\.delete\(oldLocal\)/)
})
