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
  assert.match(entry, /setInterval\(\(\) => void wsrx\.sync\(\)/)
  assert.match(entry, /type ProxyEntryMode = 'wsrx' \| 'wss'/)
  assert.match(entry, /isWssMode \? wsrxRemoteEntry : localEntry/)
  assert.match(entry, /value=\{proxyEntryMode\}/)
  assert.match(entry, /await wsrx\.delete\(localTraffic\.local\)/)
})

test('the optional local daemon is contacted only after an explicit player action', () => {
  const provider = readFileSync('src/components/WsrxProvider.tsx', 'utf8')
  const manager = readFileSync('src/components/WsrxManager.tsx', 'utf8')
  const entry = readFileSync('src/components/InstanceEntry.tsx', 'utf8')
  const optionsEffect = provider.match(
    /useEffect\(\(\) => \{\s+if \(!wsrxOptions[\s\S]*?wsrx\.setOptions\(getWsrxConfig\(wsrxOptions\)\)[\s\S]*?\}, \[[^\]]+\]\)/
  )

  assert.ok(optionsEffect)
  assert.doesNotMatch(optionsEffect[0], /doWsrxConnect/)
  assert.match(manager, /onClick=\{doWsrxConnect\}/)
  assert.match(entry, /wsrxState !== WsrxState\.Usable\) doWsrxConnect\(\)/)
})
