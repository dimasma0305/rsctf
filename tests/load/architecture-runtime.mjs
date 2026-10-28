// Real process lifecycle checks for a disposable PostgreSQL/Redis installation.
// Production startup performs its normal sandbox preflight. The local development
// profile is explicitly separate and cannot satisfy the privileged CI gate.
import assert from 'node:assert/strict'
import { execFile, spawn } from 'node:child_process'
import { promisify } from 'node:util'
import { randomBytes, createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, writeFile, rm } from 'node:fs/promises'
import { tmpdir, release } from 'node:os'
import path from 'node:path'
import net from 'node:net'
import { setTimeout as pause } from 'node:timers/promises'

const execute = promisify(execFile)
const binary = process.env.ARCHITECTURE_RUNTIME_BINARY
assert.ok(binary && path.isAbsolute(binary), 'absolute same-run server binary required')
const database = new URL(process.env.RSCTF_TEST_DATABASE_URL)
const redis = new URL(process.env.RSCTF_TEST_REDIS_URL)
assert.equal(database.hostname, '127.0.0.1')
assert.equal(database.pathname, '/rsctf_architecture_test', 'dedicated disposable database required')
assert.equal(redis.hostname, '127.0.0.1')
const profile = process.env.ARCHITECTURE_RUNTIME_PROFILE || 'privileged'
assert.ok(['privileged', 'development'].includes(profile))
if (process.env.CI) assert.equal(profile, 'privileged', 'CI must not skip production startup')
if (profile === 'privileged') assert.equal(process.getuid(), 0, 'root required for the real checker launcher')
const directory = path.resolve(process.env.ARCHITECTURE_RUNTIME_RESULTS || 'tests/load/results/runtime')
await mkdir(directory, { recursive: true, mode: 0o700 })
const storage = await mkdtemp(path.join(tmpdir(), 'rsctf-architecture-runtime-'))
const metricsToken = randomBytes(32).toString('hex')
const config = {
  RSCTF_DATABASE_URL: database.href, RSCTF_REDIS_URL: redis.href,
  RSCTF_JWT_SECRET: randomBytes(32).toString('hex'),
  RSCTF_IDENTITY_HASH_KEY: randomBytes(32).toString('hex'),
  RSCTF_EVENT_VPN_CREDENTIAL_KEY: randomBytes(32).toString('hex'),
  RSCTF_METRICS_TOKEN: metricsToken, RSCTF_STORAGE_ROOT: storage,
  RSCTF_SHARED_STORAGE: 'true', RSCTF_CONTAINER_BACKEND: 'none',
  RSCTF_AD_VPN_ENABLED: 'false', RSCTF_COOKIE_SECURE: 'false',
  RSCTF_MIGRATE: '0', TOKIO_WORKER_THREADS: '2', RSCTF_DB_MAX_CONNECTIONS: '50',
  RUST_LOG: 'info',
}
const processes = new Set()
const observations = []
let sequence = 0
async function start(role) {
  const socket = net.createServer()
  await new Promise(resolve => socket.listen(0, '127.0.0.1', resolve))
  const port = socket.address().port
  await new Promise(resolve => socket.close(resolve))
  const started = performance.now()
  const child = spawn(binary, [], {
    env: { ...process.env, ...config, RSCTF_ROLE: role, RSCTF_BIND: `127.0.0.1:${port}` },
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  let output = ''
  for (const stream of [child.stdout, child.stderr]) {
    stream.setEncoding('utf8')
    stream.on('data', chunk => { output = (output + chunk).slice(-2 * 1024 * 1024) })
  }
  const item = { child, role, started, target: `http://127.0.0.1:${port}`, output: () => output,
    file: path.join(directory, `${sequence++}-${role}.log`), result: null }
  item.exited = new Promise(resolve => {
    child.once('error', error => { item.result = { error: error.message }; resolve(item.result) })
    child.once('exit', (code, signal) => { item.result = { code, signal }; resolve(item.result) })
  })
  processes.add(item)
  return item
}
async function request(item, endpoint, headers = {}) {
  try {
    const response = await fetch(item.target + endpoint, { headers, signal: AbortSignal.timeout(1500) })
    return { status: response.status, body: await response.text(), headers: response.headers }
  } catch { return { status: 0, body: '' } }
}
async function ready(item) {
  const deadline = performance.now() + 40_000
  while (performance.now() < deadline) {
    assert.equal(item.result, null, `${item.role} exited before readiness; see ${item.file}`)
    const response = await request(item, '/healthz')
    if (response.status === 200 && response.body === 'ok') {
      const unauthorized = await request(item, '/metrics')
      assert.equal(unauthorized.status, 401)
      assert.equal(unauthorized.body, '')
      const metrics = await request(item, '/metrics', { Authorization: `Bearer ${metricsToken}` })
      assert.equal(metrics.status, 200)
      assert.equal(metrics.headers.get('cache-control'), 'no-store')
      assert.ok(metrics.body.includes('rsctf_control_job_reclaimed_total'))
      if (['all', 'control', 'network'].includes(item.role)) {
        assert.match(metrics.body, /^rsctf_network_owner_lease_valid 1$/m)
      }
      assert.ok(!metrics.body.includes(metricsToken))
      observations.push({ role: item.role, event: 'ready', milliseconds: performance.now() - item.started })
      return
    }
    await pause(100)
  }
  assert.fail(`${item.role} never became ready; see ${item.file}`)
}
async function exitWithin(item, timeout, expected) {
  const timer = setTimeout(() => item.child.kill('SIGKILL'), timeout)
  let result
  try { result = await item.exited } finally { clearTimeout(timer) }
  await writeFile(item.file, item.output(), { mode: 0o600 })
  processes.delete(item)
  assert.equal(result.signal, null, `process exceeded exit deadline or was signaled; see ${item.file}`)
  assert.equal(result.code, expected, `unexpected process result; see ${item.file}`)
}
async function terminate(item) {
  const started = performance.now()
  item.child.kill('SIGTERM')
  // The listener deliberately remains live for five seconds after dropping
  // readiness; check both states while it drains instead of only observing exit.
  let draining = false
  for (let attempt = 0; attempt < 30 && !item.result; attempt++) {
    const health = await request(item, '/healthz')
    const live = await request(item, '/livez')
    if (health.status === 503 && live.status === 200) { draining = true; break }
    await pause(100)
  }
  assert.ok(draining, `${item.role} must withdraw readiness before closing HTTP`)
  await exitWithin(item, 40_000, 0)
  observations.push({ role: item.role, event: 'sigterm-drained', milliseconds: performance.now() - started })
}
async function sql(statement) {
  const { stdout } = await execute('psql', ['-X', '-v', 'ON_ERROR_STOP=1', '-qAtc', statement], {
    env: { ...process.env, PGHOST: database.hostname, PGPORT: database.port || '5432',
      PGDATABASE: database.pathname.slice(1), PGUSER: decodeURIComponent(database.username),
      PGPASSWORD: decodeURIComponent(database.password) },
    timeout: 5000,
  })
  return stdout.trim()
}
let failure
try {
  await exitWithin(await start('migrate'), 90_000, 0)
  if (profile === 'development') {
    const app = await start('development')
    await ready(app)
    await terminate(app)
  } else {
    for (const role of ['all', 'control']) {
      const app = await start(role)
      await ready(app)
      await terminate(app)
    }
    // Real cooperating processes, one shared filesystem and one source build.
    // This process-only fixture shares the runner's network namespace, unlike
    // deployed engine/network containers. Serialize startup so the two checker
    // firewall installers cannot race on the same OUTPUT jump. No checker
    // workloads run here; shared-namespace workload isolation is not tested.
    const engine = await start('engine')
    await ready(engine)
    const network = await start('network')
    await ready(network)
    const web = await start('web')
    await ready(web)
    const duplicate = await start('network')
    await exitWithin(duplicate, 15_000, 1)
    assert.match(duplicate.output(), /already owns the singleton network\/BYOC lease/)
    observations.push({ role: 'network', event: 'duplicate-owner-rejected' })
    // Kill exactly the dedicated lock session in this isolated job's database.
    // No global database stop and no application connection is guessed by PID.
    const killed = await sql(`SELECT pg_terminate_backend(pid) FROM pg_locks
      WHERE locktype = 'advisory' AND granted AND objsubid = 1
      AND classid::bigint = 1381188436 AND objid::bigint = 1180061774
      AND database = (SELECT oid FROM pg_database WHERE datname = current_database())`)
    assert.equal(killed, 't', 'exactly one network ownership session must be terminated')
    const lostAt = performance.now()
    await exitWithin(network, 17_000, 1)
    assert.match(network.output(), /network singleton lease was lost/)
    observations.push({ role: 'network', event: 'lease-loss-exit', milliseconds: performance.now() - lostAt })
    const replacement = await start('network')
    await ready(replacement)
    await ready(web)
    observations.push({ role: 'network', event: 'replacement-ready', milliseconds: performance.now() - lostAt })
    // Keep its database session intact while userspace stops running. On resume,
    // a response from that still-live session must not revive expired proof.
    assert.equal(replacement.child.kill('SIGSTOP'), true)
    try {
      let stopped = false
      for (let attempt = 0; attempt < 20; attempt++) {
        const status = await readFile(`/proc/${replacement.child.pid}/status`, 'utf8')
        if (/^State:\s+T/m.test(status)) { stopped = true; break }
        await pause(50)
      }
      assert.ok(stopped, 'network owner must actually enter stopped state')
      await pause(12_000)
      const held = await sql(`SELECT count(*) FROM pg_locks
        WHERE locktype = 'advisory' AND granted AND objsubid = 1
        AND classid::bigint = 1381188436 AND objid::bigint = 1180061774
        AND database = (SELECT oid FROM pg_database WHERE datname = current_database())`)
      assert.equal(held, '1', 'pause must retain the ownership session')
    } finally { replacement.child.kill('SIGCONT') }
    const resumedAt = performance.now()
    const resumedHealth = await request(replacement, '/healthz')
    assert.notEqual(resumedHealth.status, 200, 'stale process must not re-enter readiness')
    await exitWithin(replacement, 5000, 1)
    assert.match(replacement.output(), /network singleton lease was lost/)
    observations.push({ role: 'network', event: 'expired-proof-exit', milliseconds: performance.now() - resumedAt })
    const afterPause = await start('network')
    await ready(afterPause)
    observations.push({ role: 'network', event: 'replacement-after-pause-ready', milliseconds: performance.now() - resumedAt })
    for (const item of [web, engine, afterPause]) await terminate(item)
  }
} catch (error) { failure = error }
finally {
  for (const item of processes) {
    if (!item.result) item.child.kill('SIGTERM')
    const timer = setTimeout(() => item.child.kill('SIGKILL'), 40_000)
    try { await item.exited } finally { clearTimeout(timer) }
    await writeFile(item.file, item.output(), { mode: 0o600 })
  }
  await rm(storage, { recursive: true, force: true })
  await writeFile(path.join(directory, 'report.json'), JSON.stringify({
    date: new Date().toISOString(), sourceRevision: process.env.GITHUB_SHA || null,
    binarySha256: createHash('sha256').update(await readFile(binary)).digest('hex'),
    kernel: release(), profile, backend: 'none', vpnEnabled: false,
    scope: profile === 'privileged'
      ? 'Process startup, HTTP readiness, SIGTERM, network-owner exclusion and session-loss recovery. No live VPN routes or worker workloads.'
      : 'Development API startup, HTTP readiness and SIGTERM only. No privileged roles, VPN routes or worker workloads.',
    observations, passed: !failure, failure: failure?.message,
  }, null, 2) + '\n')
}
if (failure) throw failure
console.log(`Architecture ${profile} runtime checks passed; evidence: ${directory}`)
