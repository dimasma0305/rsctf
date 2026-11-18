import assert from 'node:assert/strict'
import { spawn, execFileSync } from 'node:child_process'
import { readFile, writeFile, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { resolve, join } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'
import net from 'node:net'
import dgram from 'node:dgram'
import dns from 'node:dns/promises'

const [fixturePath, helperPath] = process.argv.slice(2)
assert.ok(fixturePath && helperPath, 'usage: node run.mjs fixture.json vpn-fixture[.exe]')
const fixture = JSON.parse(await readFile(fixturePath, 'utf8'))
assert.equal(fixture.schema, 1)
assert.equal(fixture.sourceSha, process.env.RSCTF_VPN_SOURCE_SHA, 'fixture must come from this commit')
assert.deepEqual(fixture.cases.map(({ name }) => name), ['ad', 'koth'])
const field = (profile, name) => profile.split(/\r?\n/).find((line) => line.startsWith(`${name} = `))?.slice(name.length + 3)
const run = (file, args) => execFileSync(file, args, { stdio: 'inherit', timeout: 60_000 })
const windows = process.platform === 'win32'
const wireguard = join(process.env.ProgramFiles ?? 'C:\\Program Files', 'WireGuard', 'wireguard.exe')
const directory = await mkdtemp(join(tmpdir(), 'rsctf-vpn-'))
const config = join(directory, 'rsctfvpn.conf')
const report = []
let helper
let installed = false
let interrupted = false
function tunnel(action) {
  assert.ok(action !== 'up' || !interrupted, 'test interrupted')
  if (windows) run(wireguard, action === 'up' ? ['/installtunnelservice', config] : ['/uninstalltunnelservice', 'rsctfvpn'])
  else run('sudo', ['-n', 'env', `PATH=${process.env.PATH}`, 'wg-quick', action, config])
}
async function stopTunnel() {
  if (installed) { tunnel('down'); installed = false }
}
async function stopHelper() {
  if (!helper) return
  const child = helper
  helper = undefined
  if (child.exitCode !== null) return
  const exited = new Promise((done) => child.once('exit', done))
  child.kill('SIGINT')
  await Promise.race([exited, delay(5_000).then(() => child.kill('SIGKILL'))])
}
for (const signal of ['SIGINT', 'SIGTERM']) {
  process.once(signal, () => {
    interrupted = true
    void (async () => {
      try { await stopTunnel() } finally { await stopHelper(); await rm(directory, { recursive: true, force: true }) }
    })().catch((error) => console.error('Tunnel cleanup failed:', error.message)).finally(() => process.exit(1))
  })
}
async function echo(protocol, host, size) {
  const payload = Buffer.alloc(size, 65)
  if (protocol === 'tcp') {
    await new Promise((done, fail) => {
      const socket = net.connect({ host, port: 2222 })
      const chunks = []
      let received = 0
      socket.setTimeout(3_000, () => socket.destroy(new Error('TCP timeout')))
      socket.on('error', fail)
      socket.on('connect', () => socket.write(payload))
      socket.on('data', (chunk) => {
        chunks.push(chunk); received += chunk.length
        if (received >= payload.length) {
          socket.destroy()
          try { assert.deepEqual(Buffer.concat(chunks), payload); done() } catch (error) { fail(error) }
        }
      })
    })
  } else {
    await new Promise((done, fail) => {
      const socket = dgram.createSocket('udp4')
      const finish = (error) => { clearTimeout(timer); socket.close(); error ? fail(error) : done() }
      const timer = setTimeout(() => finish(new Error('UDP timeout')), 3_000)
      socket.once('error', finish)
      socket.once('message', (reply) => {
        try { assert.deepEqual(reply, payload); finish() } catch (error) { finish(error) }
      })
      socket.send(payload, 9999, host)
    })
  }
}
async function waitConnected() {
  let last
  for (let i = 0; i < 10; i++) {
    try { await echo('tcp', fixture.hubAddress, 64); return } catch (error) { last = error; await delay(250) }
  }
  throw last
}
try {
  for (const [index, scenario] of fixture.cases.entries()) {
    assert.equal(scenario.profiles.length, 2)
    const [first, second] = scenario.profiles
    assert.ok(field(first, 'PrivateKey') !== field(second, 'PrivateKey'), 'shared teammate key')
    assert.notEqual(field(first, 'Address'), field(second, 'Address'))
    for (const profile of scenario.profiles) {
      assert.equal(field(profile, 'Endpoint'), '127.0.0.1:51822')
      assert.equal(field(profile, 'DNS'), fixture.hubAddress)
    }
    await writeFile(config, first, { mode: 0o600 })
    const ready = join(directory, `ready-${index}.json`)
    helper = spawn(resolve(helperPath), [resolve(fixturePath), String(index), ready], { stdio: 'inherit' })
    let startError
    helper.on('error', (error) => { startError = error })
    let control
    for (let attempt = 0; attempt < 80; attempt++) {
      if (startError) throw startError
      assert.equal(helper.exitCode, null, 'transport helper exited before readiness')
      try { ({ control } = JSON.parse(await readFile(ready, 'utf8'))); break } catch (error) {
        if (error.code !== 'ENOENT') throw error
        await delay(250)
      }
    }
    assert.match(control ?? '', /^http:\/\/127\.0\.0\.1:\d+$/)
    const stats = async () => {
      assert.equal(helper.exitCode, null, 'transport helper exited')
      const response = await fetch(`${control}/stats`, { signal: AbortSignal.timeout(3_000) })
      assert.equal(response.status, 200)
      const result = await response.json()
      assert.equal(result.companionFailures, 0, 'second teammate lost traffic')
      return result
    }
    for (let cycle = 0; cycle < 3; cycle++) {
      const before = await stats()
      installed = true // Attempt cleanup even if activation partially fails.
      tunnel('up')
      await waitConnected()
      const resolver = new dns.Resolver({ timeout: 2_000, tries: 2 })
      resolver.setServers([fixture.hubAddress])
      assert.deepEqual(await resolver.resolve4(fixture.hostname), [fixture.ingressAddress])
      // Check the OS resolver too: parsing DNS= alone does not prove native DNS works.
      const lookup = await Promise.race([
        dns.lookup(fixture.hostname, { family: 4 }),
        delay(5_000).then(() => { throw new Error('OS DNS timeout') }),
      ])
      assert.equal(lookup.address, fixture.ingressAddress)
      for (let batch = 0; batch < 10; batch++) {
        await Promise.all([
          echo('tcp', fixture.hubAddress, 4_096),
          echo('tcp', fixture.hostname, 4_096),
          echo('tcp', fixture.serviceAddress, 4_096),
          echo('udp', fixture.serviceAddress, 1_200),
        ])
        await delay(100)
      }
      const during = await stats()
      assert.equal(during.peersWithHandshake, 2, 'both personal peers must complete handshakes')
      assert.ok(during.companionSuccesses >= before.companionSuccesses + 5, 'no concurrent teammate traffic')
      await stopTunnel()
      await delay(1_000)
      const after = await stats()
      assert.ok(after.companionSuccesses >= during.companionSuccesses + 5, 'disconnect interrupted teammate')
      report.push({ scenario: scenario.name, cycle, ...after })
      console.log(`${scenario.name}: native TCP/UDP, OS DNS and reconnect ${cycle + 1}/3 passed; teammate stayed connected`)
    }
    await stopHelper()
  }
  await writeFile('vpn-client-results.json', JSON.stringify({ platform: process.platform, sourceSha: fixture.sourceSha, checks: report }, null, 2))
} finally {
  try { await stopTunnel() } finally { await stopHelper(); await rm(directory, { recursive: true, force: true }) }
}
