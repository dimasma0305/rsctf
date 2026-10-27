// Controlled server comparison. Owns its databases, Redis, containers and files;
// uses separately built immutable release binaries and the same runtime image.
import assert from 'node:assert/strict'
import { execFile, spawn } from 'node:child_process'
import { promisify } from 'node:util'
import { createHash, randomBytes, randomUUID } from 'node:crypto'
import { mkdir, readFile, writeFile, rm } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { setTimeout as pause } from 'node:timers/promises'
import net from 'node:net'

const execute = promisify(execFile)
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..')
const { stdout: common } = await execute('git', ['rev-parse', '--path-format=absolute', '--git-common-dir'], { cwd: root })
if (process.env.RSCTF_PLATFORM_AUDIT_LOCKED !== '1') {
  const child = spawn('flock', [path.join(common.trim(), 'rsctf-build.lock'), process.execPath, fileURLToPath(import.meta.url)], {
    cwd: root, stdio: 'inherit', env: { ...process.env, RSCTF_PLATFORM_AUDIT_LOCKED: '1' },
  })
  const code = await new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', resolve) })
  process.exit(code ?? 1)
}
const binaries = {
  before: process.env.ARCHITECTURE_BASELINE_BINARY,
  after: process.env.ARCHITECTURE_CANDIDATE_BINARY,
}
const candidateRevision = process.env.ARCHITECTURE_CANDIDATE_REVISION
assert.match(candidateRevision || '', /^[a-f0-9]{40}$/, 'exact candidate source revision required')
for (const binary of Object.values(binaries)) {
  assert.ok(binary && path.isAbsolute(binary), 'absolute baseline/candidate release binary paths required')
}
const pg = process.env.PG_CONTAINER
const network = process.env.ARCHITECTURE_DOCKER_NETWORK
assert.ok(pg && network, 'disposable PG_CONTAINER and ARCHITECTURE_DOCKER_NETWORK required')
assert.match(pg, /^rsctf-architecture-/, 'select the explicitly disposable architecture PostgreSQL')
const pgHost = process.env.ARCHITECTURE_DATABASE_HOST || 'architecture-pg'
const pgPassword = process.env.ARCHITECTURE_DATABASE_PASSWORD
assert.ok(pgPassword, 'disposable database password required')
const servingRole = process.env.ARCHITECTURE_ROLE || 'all'
assert.ok(['all', 'development'].includes(servingRole))
const allowed = (await readFile('/proc/self/status', 'utf8')).match(/^Cpus_allowed_list:\s*(.*)$/m)[1]
  .split(',').flatMap(part => {
    const [start, end = start] = part.split('-').map(Number)
    return Array.from({ length: end - start + 1 }, (_, index) => start + index)
  }).slice(0, 2).join(',')
const { stdout: runtimeImage } = await execute('docker', ['image', 'inspect', '--format', '{{.Id}}',
  process.env.ARCHITECTURE_RUNTIME_IMAGE || 'rsctf-architecture-runtime:local'])
const { stdout: redisImage } = await execute('docker', ['image', 'inspect', '--format', '{{.Id}}', 'redis:7-alpine'])
const { stdout: pgImage } = await execute('docker', ['inspect', '--format', '{{.Image}}', pg])
const id = randomUUID().replaceAll('-', '')
const scope = `rsctf-architecture-platform-${id}`
const directory = path.join('/workspace', scope)
const databases = { before: `arch_platform_${id}_before`, after: `arch_platform_${id}_after` }
const redis = `${scope}-redis`
const jwt = randomBytes(32).toString('hex')
const identityKey = randomBytes(32).toString('hex')
const metricsToken = randomBytes(32).toString('hex')
const vpnKey = randomBytes(32).toString('hex')
const results = []
const createdDatabases = []
let redisCreated = false
let redisPort
let active = null
await mkdir(directory, { mode: 0o700 })
async function unusedLoopbackPort() {
  const socket = net.createServer()
  await new Promise(resolve => socket.listen(0, '127.0.0.1', resolve))
  const port = socket.address().port
  await new Promise(resolve => socket.close(resolve))
  return port
}
const sql = async (database, statement) => {
  const { stdout } = await execute('docker', ['exec', pg, 'psql', '-U', 'postgres', '-d', database,
    '-X', '-v', 'ON_ERROR_STOP=1', '-qAtc', statement], { maxBuffer: 1024 * 1024 })
  return stdout.trim()
}
async function run(program, args, env, logfile) {
  let output = ''
  const child = spawn(program, args, { cwd: root, env: { ...process.env, RSCTF_LOAD_DATABASE_URL: '', ...env }, stdio: ['ignore', 'pipe', 'pipe'] })
  for (const stream of [child.stdout, child.stderr]) {
    stream.setEncoding('utf8')
    stream.on('data', chunk => { output += chunk })
  }
  const timer = setTimeout(() => child.kill('SIGTERM'), 180_000)
  let code
  try { code = await new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', resolve) }) }
  finally { clearTimeout(timer); await writeFile(logfile, output, { mode: 0o600 }) }
  assert.equal(code, 0, `harness failed; see ${logfile}`)
}
async function stopApp() {
  if (!active) return
  if (active.kind === 'host') {
    active.child.kill('SIGTERM')
    const timer = setTimeout(() => active.child.kill('SIGKILL'), 45_000)
    try { await active.exited } finally { clearTimeout(timer) }
    await writeFile(active.log, active.output(), { mode: 0o600 })
    active = null
    return
  }
  await execute('docker', ['stop', '--time', '40', active.name])
  const logs = await execute('docker', ['logs', '--tail', '1000', active.name], { maxBuffer: 4 * 1024 * 1024 })
  await writeFile(active.log, logs.stdout + logs.stderr, { mode: 0o600 })
  await execute('docker', ['rm', '--volumes', active.name])
  active = null
}
async function start(mode, role = servingRole) {
  assert.equal(active, null)
  const name = `${scope}-${mode}-${role}`
  const data = path.join(directory, `data-${mode}`)
  await mkdir(data, { recursive: true })
  const envFile = path.join(directory, `${mode}.env`)
  const config = {
    RSCTF_ROLE: role, RSCTF_CONTAINER_BACKEND: 'none', RSCTF_DATABASE_URL:
      `postgres://postgres:${encodeURIComponent(pgPassword)}@${pgHost}:5432/${databases[mode]}`,
    RSCTF_REDIS_URL: `redis://${redis}:6379/${mode === 'before' ? 0 : 1}`,
    RSCTF_JWT_SECRET: jwt, RSCTF_IDENTITY_HASH_KEY: identityKey,
    RSCTF_EVENT_VPN_CREDENTIAL_KEY: vpnKey,
    RSCTF_METRICS_TOKEN: metricsToken, RSCTF_STORAGE_ROOT: data,
    RSCTF_MIGRATE: role === 'migrate' ? '1' : '0', RSCTF_DB_MAX_CONNECTIONS: '50',
    RSCTF_COOKIE_SECURE: 'false', TOKIO_WORKER_THREADS: '2', RUST_LOG: 'warn',
  }
  if (role === 'development') {
    // The supported development role is API-only and binds strictly to loopback.
    // Use it explicitly on kernels without Landlock; never weaken engine startup.
    const port = await unusedLoopbackPort()
    config.RSCTF_BIND = `127.0.0.1:${port}`
    config.RSCTF_DATABASE_URL = `postgres://postgres:${encodeURIComponent(pgPassword)}@127.0.0.1:${process.env.ARCHITECTURE_DATABASE_PORT || '55432'}/${databases[mode]}`
    config.RSCTF_REDIS_URL = `redis://127.0.0.1:${redisPort}/${mode === 'before' ? 0 : 1}`
    let output = ''
    const child = spawn('taskset', ['-c', allowed, binaries[mode]], {
      cwd: root, env: { ...process.env, ...config }, stdio: ['ignore', 'pipe', 'pipe'],
    })
    for (const stream of [child.stdout, child.stderr]) {
      stream.setEncoding('utf8')
      stream.on('data', chunk => { output = (output + chunk).slice(-4 * 1024 * 1024) })
    }
    const exited = new Promise(resolve => { child.once('exit', resolve); child.once('error', resolve) })
    active = { kind: 'host', name, child, exited, output: () => output,
      log: path.join(directory, `${mode}-${role}.log`), pid: String(child.pid), target: `http://127.0.0.1:${port}` }
    await waitReady()
    return
  }
  await writeFile(envFile, Object.entries(config).map(([key, value]) => `${key}=${value}`).join('\n'), { mode: 0o600 })
  const args = ['run', '-d', '--name', name, '--label', `rsctf.architecture.scope=${scope}`,
    '--network', network, '--cpus', '2', '--memory', '2g', '--pids-limit', '256',
    '--env-file', envFile, '--mount', `type=bind,src=${binaries[mode]},dst=/usr/local/bin/rsctf,readonly`,
    '--mount', `type=bind,src=${directory},dst=${directory}`]
  // The all-role checker preflight owns an isolated container firewall even
  // with no workload backend. No host namespace or Docker socket is mounted.
  if (role !== 'migrate') args.push('--cap-add', 'NET_ADMIN', '--cap-add', 'NET_RAW',
    '--sysctl', 'net.ipv6.conf.all.disable_ipv6=1', '--sysctl', 'net.ipv6.conf.default.disable_ipv6=1',
    '-p', '127.0.0.1::8080')
  args.push(runtimeImage.trim())
  await execute('docker', args)
  active = { name, log: path.join(directory, `${mode}-${role}.log`) }
  if (role === 'migrate') {
    const { stdout } = await execute('docker', ['wait', name], { timeout: 60_000 })
    assert.equal(stdout.trim(), '0', `migration failed; inspect ${name}`)
    await stopApp()
    return
  }
  const { stdout } = await execute('docker', ['port', name, '8080/tcp'])
  active.target = `http://${stdout.trim()}`
  await waitReady()
  const { stdout: pid } = await execute('docker', ['inspect', '--format', '{{.State.Pid}}', name])
  active.pid = pid.trim()
}
async function waitReady() {
  let ready = false
  for (let attempt = 0; attempt < 60; attempt += 1) {
    try {
      const response = await fetch(`${active.target}/healthz`, { signal: AbortSignal.timeout(2000) })
      ready = response.status === 200 && await response.text() === 'ok'
      if (ready) break
    } catch {}
    await pause(500)
  }
  assert.ok(ready, `server never became ready; diagnostics: ${active.log}`)
}
let failure
try {
  // Docker reallocates an automatically published host port after stop/start.
  // Bind one explicit loopback port so the outage test restores the same endpoint.
  const fixedRedisPort = await unusedLoopbackPort()
  await execute('docker', ['run', '-d', '--name', redis, '--network', network,
    '--label', `rsctf.architecture.scope=${scope}`, '--label', 'com.docker.compose.service=redis',
    '--memory', '192m', '--cpus', '1', '-p', `127.0.0.1:${fixedRedisPort}:6379`, redisImage.trim(), 'redis-server', '--save', '',
    '--appendonly', 'no', '--maxmemory', '128mb', '--maxmemory-policy', 'allkeys-lru'])
  redisCreated = true
  const { stdout: redisAddress } = await execute('docker', ['port', redis, '6379/tcp'])
  redisPort = new URL(`http://${redisAddress.trim()}`).port
  assert.equal(redisPort, String(fixedRedisPort))
  await sql('postgres', `CREATE DATABASE "${databases.before}"`)
  createdDatabases.push(databases.before)
  await start('before', 'migrate')
  await start('before')
  await run(process.execPath, ['tests/load/scoreboard-page-fixture.mjs'], {
    PG_CONTAINER: pg, PG_DATABASE: databases.before, RSCTF_CONTAINER: active.kind === 'host' ? pg : active.name,
    SCOREBOARD_DISPOSABLE_SEED: '1', TEAMS: '500', RSCTF_JWT_SECRET: jwt,
  }, path.join(directory, 'seed.log'))
  await stopApp()
  // PostgreSQL clones the same committed fixture, including timestamps and UUIDs.
  await sql('postgres', `CREATE DATABASE "${databases.after}" TEMPLATE "${databases.before}"`)
  createdDatabases.push(databases.after)
  await start('after', 'migrate')
  for (const [repetition, order] of [['before', 'after'], ['after', 'before'], ['before', 'after']].entries()) {
    for (const mode of order) {
      await execute('docker', ['exec', redis, 'redis-cli', '-n', mode === 'before' ? '0' : '1', 'FLUSHDB'])
      await start(mode)
      const summary = path.join(directory, `scoreboard-${mode}-${repetition}.json`)
      await run(process.execPath, ['tests/load/scoreboard-page.mjs'], {
        PG_CONTAINER: pg, PG_DATABASE: databases[mode], RSCTF_CONTAINER: active.kind === 'host' ? pg : active.name,
        RSCTF_JWT_SECRET: jwt, TARGET: active.target, GAME: '1', TEAMS: '500',
        SCOREBOARD_APP_PID: active.pid, SCOREBOARD_CASE: 'page', SUMMARY_JSON: summary,
        RATE: process.env.ARCHITECTURE_RATE || '100', VUS: '64', DURATION: '30s',
      }, path.join(directory, `scoreboard-${mode}-${repetition}.log`))
      const metrics = JSON.parse(await readFile(summary, 'utf8'))
      const resources = JSON.parse(await readFile(`${summary}.resources.json`, 'utf8'))
      results.push({ mode, repetition, metrics, resources })
      console.error(`${mode} run ${repetition + 1}: p95 ${metrics.metrics.http_req_duration['p(95)'].toFixed(2)} ms`)
      await stopApp()
    }
  }
  await start('after')
  await run(process.execPath, ['tests/load/metrics-scrape.mjs'], {
    TARGET: active.target, METRICS_APP_PID: active.pid, PG_CONTAINER: pg,
    RSCTF_METRICS_TOKEN: metricsToken, RATE: '100',
    SUMMARY_JSON: path.join(directory, 'metrics-scrape.json'),
  }, path.join(directory, 'metrics-scrape.log'))
  const exporter = {
    metrics: JSON.parse(await readFile(path.join(directory, 'metrics-scrape.json'), 'utf8')),
    resources: JSON.parse(await readFile(path.join(directory, 'metrics-scrape.json.resources.json'), 'utf8')),
  }
  await run(process.execPath, ['tests/load/redis-outage.mjs'], {
    TARGET: active.target, REDIS_CONTAINER: redis, CONFIRM_REDIS_OUTAGE: redis,
    SUMMARY_JSON: path.join(directory, 'redis-outage.json'), RATE: '1', DURATION: '15s',
  }, path.join(directory, 'redis-outage.log'))
  const outage = JSON.parse(await readFile(path.join(directory, 'redis-outage.json'), 'utf8'))
  await stopApp()
  const hashes = {}
  for (const [mode, binary] of Object.entries(binaries)) {
    hashes[mode] = createHash('sha256').update(await readFile(binary)).digest('hex')
  }
  const harnessHashes = {}
  for (const file of ['architecture-platform.mjs', 'architecture-runtime/Dockerfile',
    'scoreboard-page-fixture.mjs', 'scoreboard-page.mjs', 'k6/scoreboard-page.js', 'lib.mjs',
    'metrics-scrape.mjs', 'k6/metrics-scrape.js', 'redis-outage.mjs', 'k6/redis-outage.js']) {
    harnessHashes[file] = createHash('sha256').update(await readFile(new URL(file, import.meta.url))).digest('hex')
  }
  const report = { date: new Date().toISOString(), binarySha256: hashes,
    baselineRevision: 'bff5d1a31ae6f964d877fd1fc11cf8328e8e99de',
    candidateRevision, harnessHashes,
    runtimeImage: runtimeImage.trim(), postgresImage: pgImage.trim(), redisImage: redisImage.trim(),
    servingRole, validatedScope: 'HTTP reads, authenticated metrics, and malformed-request Redis fallback',
    appLimits: { cpu: 2, affinity: servingRole === 'development' ? allowed : null,
      memoryMiB: servingRole === 'all' ? 2048 : null,
      tokioThreads: 2, databaseConnections: 50, ipFamily: 'IPv4' },
    teams: 500, challenges: 20, solves: 10000, results, exporter, outage, diagnostics: directory }
  await writeFile(path.join(root, 'tests/load/results/architecture-platform.json'), `${JSON.stringify(report, null, 2)}\n`)
} catch (error) {
  failure = error
} finally {
  const errors = failure ? [failure] : []
  const cleanup = async action => { try { await action() } catch (error) { errors.push(error) } }
  await cleanup(stopApp)
  if (redisCreated) {
    await cleanup(() => execute('docker', ['stop', '--time', '5', redis]))
    await cleanup(() => execute('docker', ['rm', '--volumes', redis]))
  }
  for (const database of createdDatabases.reverse()) {
    await cleanup(() => sql('postgres', `DROP DATABASE "${database}"`))
  }
  // Keep scoped logs/results for diagnosis, remove temporary credential files.
  for (const mode of ['before', 'after']) await cleanup(() => rm(path.join(directory, `${mode}.env`), { force: true }))
  if (errors.length) throw new AggregateError(errors, `architecture platform run failed; diagnostics: ${directory}`)
}
