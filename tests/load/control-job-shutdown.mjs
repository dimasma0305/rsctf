// Real process signals and Docker side effects against an invocation-owned database.
// No production endpoint, database, container, or runtime configuration is changed.
import assert from 'node:assert/strict'
import { execFile, spawn } from 'node:child_process'
import { promisify } from 'node:util'
import { createHash, randomBytes, randomUUID } from 'node:crypto'
import { mkdir, mkdtemp, readFile, writeFile, rm } from 'node:fs/promises'
import { tmpdir, release } from 'node:os'
import path from 'node:path'
import http from 'node:http'
import net from 'node:net'
import { setTimeout as pause } from 'node:timers/promises'

const execute = promisify(execFile)
const binary = process.env.ARCHITECTURE_RUNTIME_BINARY
assert.ok(binary && path.isAbsolute(binary), 'absolute tested server binary required')
const pgContainer = process.env.SHUTDOWN_PG_CONTAINER
assert.match(pgContainer || '', /^[a-zA-Z0-9][a-zA-Z0-9_.-]*$/, 'disposable PostgreSQL container required')
const database = new URL(process.env.RSCTF_TEST_DATABASE_URL)
assert.equal(database.hostname, '127.0.0.1', 'only loopback PostgreSQL is supported')
assert.equal(process.env.RSCTF_SHUTDOWN_DISPOSABLE, '1', 'explicit disposable acknowledgement required')
const role = 'control'
const id = randomBytes(8).toString('hex')
const name = `rsctf_shutdown_${id}`
const network = `rsctf-shutdown-${id}`
const subnet = `10.254.${parseInt(id.slice(0, 2), 16)}.0/24`
const directory = await mkdtemp(path.join(tmpdir(), 'rsctf-shutdown-'))
const reportPath = path.resolve(process.env.SHUTDOWN_REPORT || 'tests/load/results/control-job-shutdown.json')
const observations = []
const images = new Set()
const operations = new Set()
const logs = []
const gates = new Map()
let app, failure, ownsDatabase = false, ownsNetwork = false, barrier
const hash = value => createHash('sha256').update(value).digest('hex')
const literal = value => "'" + String(value).replaceAll("'", "''") + "'"
const docker = args => execute('docker', ['--host=unix:///var/run/docker.sock', ...args], { timeout: 90_000, maxBuffer: 4 * 1024 * 1024 })
const sql = async statement => (await docker(['exec', '-i', pgContainer, 'psql', '-X', '-v', 'ON_ERROR_STOP=1', '-U', decodeURIComponent(database.username), '-d', ownsDatabase ? name : 'postgres', '-qAtc', statement])).stdout.trim()
const config = {
  RSCTF_JWT_SECRET: randomBytes(32).toString('hex'),
  RSCTF_IDENTITY_HASH_KEY: randomBytes(32).toString('hex'),
  RSCTF_EVENT_VPN_CREDENTIAL_KEY: randomBytes(32).toString('hex'),
  RSCTF_CONTAINER_BACKEND: 'docker', RSCTF_STORAGE_BACKEND: 'local',
  RSCTF_SHARED_DOCKER_DAEMON: 'true', RSCTF_SHARED_STORAGE: 'true',
  RSCTF_DOCKER_SCOPE: name,
  RSCTF_AD_VPN_ENABLED: 'false', RSCTF_AD_VPN_SERVICES_NETWORK: network,
  RSCTF_AD_VPN_SERVICES_CIDR: subnet,
  RSCTF_COOKIE_SECURE: 'false', RSCTF_MIGRATE: '0',
  RSCTF_DB_MAX_CONNECTIONS: '40', TOKIO_WORKER_THREADS: '2', RUST_LOG: 'info',
}
async function until(check, description, timeout = 30_000) {
  const deadline = performance.now() + timeout
  while (performance.now() < deadline) {
    if (await check()) return
    await pause(100)
  }
  assert.fail(`timeout: ${description}`)
}
async function start(runtimeRole = role) {
  assert.ok(!app)
  const socket = net.createServer()
  await new Promise(resolve => socket.listen(0, '127.0.0.1', resolve))
  const port = socket.address().port
  await new Promise(resolve => socket.close(resolve))
  const url = new URL(database); url.pathname = '/' + name
  const env = { ...process.env, ...config, DOCKER_HOST: 'unix:///var/run/docker.sock',
    RSCTF_DATABASE_URL: url.href, RSCTF_STORAGE_ROOT: directory,
    RSCTF_ROLE: runtimeRole, RSCTF_BIND: `127.0.0.1:${port}` }
  delete env.DOCKER_CONTEXT
  if (process.env.RSCTF_TEST_REDIS_URL) env.RSCTF_REDIS_URL = process.env.RSCTF_TEST_REDIS_URL
  else delete env.RSCTF_REDIS_URL
  const child = spawn(binary, [], { env, stdio: ['ignore', 'pipe', 'pipe'] })
  let log = ''
  for (const stream of [child.stdout, child.stderr]) stream.on('data', bytes => { log = (log + bytes).slice(-256 * 1024) })
  const item = { child, base: `http://127.0.0.1:${port}`, result: null, log: () => log }
  item.exited = new Promise(resolve => {
    child.once('error', () => { item.result = { code: -1 }; resolve(item.result) })
    child.once('exit', (code, signal) => { item.result = { code, signal }; resolve(item.result) })
  })
  app = item
  if (runtimeRole === 'migrate') return finish()
  await until(async () => {
    assert.equal(item.result, null, 'server exited before readiness')
    return await status('/healthz') === 200
  }, 'server readiness', 45_000)
}
async function status(endpoint) {
  try { return (await fetch(app.base + endpoint, { signal: AbortSignal.timeout(1000) })).status } catch { return 0 }
}
async function finish(signal = null) {
  const item = app
  const timer = setTimeout(() => item.child.kill('SIGKILL'), 45_000)
  try {
    const result = await item.exited
    assert.equal(result.signal, signal)
    assert.equal(result.code, signal ? null : 0)
  } finally { clearTimeout(timer); logs.push(item.log()); app = null }
}
async function stop() {
  if (!app) return
  app.child.kill('SIGTERM')
  await finish()
}
async function enqueue(kind, challenge, input, fingerprint = 'a'.repeat(64)) {
  const job = randomUUID(); operations.add(job)
  await sql(`INSERT INTO "ControlPlaneJobs" (id,kind,scope_key,game_id,challenge_id,operation_id,fingerprint,input)
    VALUES ('${job}',${literal(kind)},'shutdown:${job}',1,${challenge ?? 'NULL'},'${job}',${literal(fingerprint)},${literal(JSON.stringify(input))});
    INSERT INTO "ControlPlaneJobOperations" (operation_id,job_id) VALUES ('${job}','${job}')`)
  return job
}
async function jobState(job) {
  return JSON.parse(await sql(`SELECT row_to_json(j) FROM "ControlPlaneJobs" j WHERE id='${job}'`))
}
async function succeeded(job) {
  await until(async () => {
    const row = await jobState(job)
    assert.notEqual(row.status, 3, `job failed: ${row.error}`)
    return row.status === 2
  }, 'durable job completion', 60_000)
}
async function lockCheckpoint(job) {
  // Block only this reset's first replacement checkpoint, after Docker create/start.
  // PostgreSQL releases the barrier connection on cleanup; no application hook exists.
  await sql(`CREATE OR REPLACE FUNCTION shutdown_checkpoint() RETURNS trigger LANGUAGE plpgsql AS $$
    BEGIN IF NEW.id='${job}' AND NEW.input ? 'replacementBackendId'
      AND NOT OLD.input ? 'replacementBackendId' THEN PERFORM pg_advisory_xact_lock(193700,1); END IF;
      RETURN NEW; END $$;
    DROP TRIGGER IF EXISTS shutdown_checkpoint ON "ControlPlaneJobs";
    CREATE TRIGGER shutdown_checkpoint BEFORE UPDATE ON "ControlPlaneJobs"
      FOR EACH ROW EXECUTE FUNCTION shutdown_checkpoint()`)
  const child = spawn('docker', ['--host=unix:///var/run/docker.sock', 'exec', '-i', pgContainer,
    'psql', '-X', '-v', 'ON_ERROR_STOP=1', '-U', decodeURIComponent(database.username), '-d', name, '-qAt'],
  { stdio: ['pipe', 'pipe', 'pipe'] })
  let output = ''
  child.stdout.on('data', bytes => { output += bytes })
  child.stderr.resume()
  const exited = new Promise(resolve => child.once('exit', resolve))
  barrier = { child, exited }
  child.stdin.write("SELECT pg_advisory_lock(193700,1); SELECT 'barrier-ready';\n")
  await until(() => output.includes('barrier-ready'), 'checkpoint barrier ready')
}
async function unlockCheckpoint() {
  if (!barrier) return
  barrier.child.stdin.end('SELECT pg_advisory_unlock(193700,1);\n')
  await barrier.exited
  barrier = null
}
async function activeCheckpoint() {
  return await sql(`SELECT count(*) FROM pg_locks WHERE locktype='advisory' AND NOT granted
    AND classid=193700 AND objid=1 AND database=(SELECT oid FROM pg_database WHERE datname=current_database())`) === '1'
}
async function resources(job) {
  return (await docker(['ps', '-aq', '--filter', `label=rsctf.operation=ad-reset:${job}`])).stdout.trim().split('\n').filter(Boolean)
}
const gateServer = http.createServer((req, res) => {
  const gate = gates.get(req.url)
  if (!gate) { res.writeHead(404).end(); return }
  gate.entered = true
  gate.responses.add(res)
  res.on('close', () => gate.responses.delete(res))
  if (gate.open) res.end('continue\n')
})
gateServer.requestTimeout = 60_000
async function buildFixture(challenge) {
  const image = `rsctf/shutdown-${id}-${challenge}:latest`; images.add(image)
  const route = '/' + randomBytes(16).toString('hex')
  const gate = { entered: false, open: false, responses: new Set() }; gates.set(route, gate)
  const gateway = JSON.parse((await docker(['network', 'inspect', 'bridge'])).stdout)[0].IPAM.Config[0].Gateway
  assert.ok(net.isIPv4(gateway), 'Docker bridge IPv4 gateway required')
  const dockerfile = `FROM ${process.env.SHUTDOWN_BASE_IMAGE || 'postgres:18.4-alpine3.24'}\nRUN wget -qO- http://${gateway}:${gateServer.address().port}${route}\nENTRYPOINT []\nCMD ["sleep", "3600"]\n`
  const archivePath = path.join(directory, `build-${challenge}.zip`)
  await execute('python3', ['-c', 'import sys,zipfile\nwith zipfile.ZipFile(sys.argv[1],"w") as z: z.writestr("Dockerfile",sys.argv[2])', archivePath, dockerfile])
  const archive = hash(await readFile(archivePath))
  const blob = path.join(directory, archive.slice(0, 2), archive.slice(2, 4), archive)
  await mkdir(path.dirname(blob), { recursive: true }); await writeFile(blob, await readFile(archivePath))
  await sql(`UPDATE "GameChallenges" SET container_image=${literal(image)}, original_archive_blob_path='${archive}',
    build_context_subdir='', build_status=5 WHERE id=${challenge}`)
  const fingerprint = hash(JSON.stringify({ archive, containerImage: image, context: '' }))
  return { image, fingerprint, gate, release() { gate.open = true; for (const response of gate.responses) response.end('continue\n') } }
}
async function signalWhileActive(kind, job, force, releaseWork, verify) {
  const started = performance.now()
  assert.equal((await jobState(job)).status, 1, 'job must be running before SIGTERM')
  assert.equal(app.child.kill('SIGTERM'), true)
  await until(async () => await status('/healthz') === 503 && await status('/livez') === 200, 'readiness withdrawn while alive', 4000)
  const queued = await enqueue('BuildBatch', null, {})
  await pause(1100) // Longer than an idle claim poll; the drain must reject new work.
  assert.equal((await jobState(queued)).status, 0, 'no claim after drain begins')
  assert.equal(app.result, null, 'active work must remain supervised during drain')
  if (force) {
    assert.equal(app.child.kill('SIGKILL'), true)
    await finish('SIGKILL')
    const interrupted = await jobState(job)
    assert.equal(interrupted.status, 1)
    assert.ok(interrupted.lease_token && Date.parse(interrupted.lease_expires_at_utc) > Date.now(), 'crash must retain its live lease')
    await releaseWork()
    // Simulate expiry only after confirmed process death. This exercises recovery
    // without falsely reporting a fifteen-minute production lease as fast recovery.
    await sql(`UPDATE "ControlPlaneJobs" SET lease_expires_at_utc=clock_timestamp()-interval '1 second' WHERE id='${job}';
      UPDATE "ControlPlaneResourceLeases" SET lease_expires_at_utc=clock_timestamp()-interval '1 second' WHERE owner_job_id='${job}'`)
    const recovery = performance.now()
    await start(); await succeeded(job); await succeeded(queued)
    await verify()
    observations.push({ kind, scenario: 'sigterm-then-crash', recoveryAfterInjectedExpiryMs: performance.now() - recovery,
      productionLeaseSeconds: 900, noClaimsDuringDrain: true })
    await stop()
  } else {
    await releaseWork(); await finish(); await succeeded(job)
    assert.equal((await jobState(queued)).status, 0)
    await verify()
    const receipt = JSON.stringify((await jobState(job)).result)
    await start(); await succeeded(queued)
    assert.equal(JSON.stringify((await jobState(job)).result), receipt, 'restart must preserve terminal receipt')
    await verify(); await stop()
    observations.push({ kind, scenario: 'graceful-sigterm', totalWithRestartMs: performance.now() - started, noClaimsDuringDrain: true })
  }
}
try {
  await sql(`CREATE DATABASE "${name}"`); ownsDatabase = true
  await docker(['network', 'create', '--internal', '--subnet', subnet, network]); ownsNetwork = true
  await new Promise(resolve => gateServer.listen(0, '0.0.0.0', resolve))
  await start('migrate')
  let seed = await readFile(new URL('./restore-drill/seed.sql', import.meta.url), 'utf8')
  for (const [key, value] of Object.entries({ USER: randomUUID(), POSTER: 'a'.repeat(64), WRITEUP: 'b'.repeat(64), POSTER_SIZE: 1, WRITEUP_SIZE: 1 })) seed = seed.replaceAll(`:${key}:`, String(value))
  await sql(seed)
  await sql(`UPDATE "Games" SET start_time_utc=now()-interval '1 hour',end_time_utc=now()+interval '1 day';
    UPDATE "GameChallenges" SET "Type"=1;
    INSERT INTO "GameChallenges" SELECT (jsonb_populate_record(NULL::"GameChallenges",to_jsonb(c)||'{"id":2,"Type":4,"title":"Reset shutdown","ad_allow_self_reset":true}')).* FROM "GameChallenges" c WHERE id=1;
    INSERT INTO "AdTeamServices" (id,game_id,participation_id,challenge_id,host,port,status) VALUES (1,1,1,2,'',80,0)`)
  for (const force of [false, true]) {
    const fixture = await buildFixture(1)
    await start()
    const job = await enqueue('ChallengeBuild', 1, { challengeId: 1, trigger: 'Manual', attempt: force ? 2 : 1 }, fixture.fingerprint)
    await until(() => fixture.gate.entered, 'Docker RUN reached external barrier', 60_000)
    await signalWhileActive('ChallengeBuild', job, force, () => fixture.release(), async () => {
      const row = await jobState(job)
      const inspect = JSON.parse((await docker(['image', 'inspect', fixture.image])).stdout)[0]
      assert.equal(row.result.imageDigest, inspect.Id)
      assert.equal(await sql(`SELECT count(*) FROM "BuildRecords" WHERE challenge_id=1 AND attempt=${force ? 2 : 1}`), '1', 'one build receipt per attempt')
    })
  }
  const imageId = JSON.parse((await docker(['image', 'inspect', [...images][0]])).stdout)[0].Id
  await sql(`UPDATE "GameChallenges" SET container_image='${imageId}',build_image_digest='${imageId}',build_status=1 WHERE id=2`)
  for (const force of [false, true]) {
    const previous = await sql('SELECT COALESCE(container_id,\'\') FROM "AdTeamServices" WHERE id=1')
    const job = await enqueue('AdReset', 2, { serviceId: 1, participationId: 1, expectedBackendId: previous || null, playerPolicy: false })
    await lockCheckpoint(job); await start()
    await until(activeCheckpoint, 'reset created Docker replacement and is checkpointing', 60_000)
    const created = await resources(job); assert.equal(created.length, 1)
    await signalWhileActive('AdReset', job, force, unlockCheckpoint, async () => {
      assert.deepEqual(await resources(job), created, 'recovery must reuse the exact Docker replacement')
      const backend = await sql('SELECT container_id FROM "AdTeamServices" WHERE id=1')
      assert.ok(created[0].startsWith(backend) || backend.startsWith(created[0]))
      assert.equal((await jobState(job)).result.reset, true)
      if (previous) assert.equal((await docker(['ps', '-aq', '--filter', `id=${previous}`])).stdout.trim(), '', 'retired container must be gone')
    })
  }
} catch (error) { failure = error }
finally {
  for (const gate of gates.values()) { gate.open = true; for (const response of gate.responses) response.end('cleanup\n') }
  await unlockCheckpoint().catch(error => { failure ||= error })
  if (app) { app.child.kill('SIGKILL'); await app.exited; logs.push(app.log()); app = null }
  gateServer.closeAllConnections(); await new Promise(resolve => gateServer.close(resolve))
  for (const job of operations) for (const container of await resources(job).catch(() => [])) await docker(['rm', '-f', container]).catch(error => { failure ||= error })
  for (const image of images) await docker(['image', 'rm', '-f', image]).catch(error => { failure ||= error })
  if (ownsNetwork) await docker(['network', 'rm', network]).catch(error => { failure ||= error })
  if (ownsDatabase) {
    ownsDatabase = false // Drop from the administrative connection, never from the target.
    await sql(`DROP DATABASE "${name}" WITH (FORCE)`).catch(error => { failure ||= error })
  }
  await mkdir(path.dirname(reportPath), { recursive: true })
  let diagnostics = logs.join('\n')
  for (const value of [database.href, decodeURIComponent(database.password), ...Object.values(config).filter(value => value.length >= 32)]) diagnostics = diagnostics.replaceAll(value, '[redacted]')
  if (failure) await writeFile(reportPath + '.log', diagnostics, { mode: 0o600 })
  await writeFile(reportPath, JSON.stringify({ date: new Date().toISOString(), role, kernel: release(),
    sourceRevision: process.env.GITHUB_SHA || (await execute('git', ['rev-parse', 'HEAD'])).stdout.trim(),
    binarySha256: hash(await readFile(binary)), harnessSha256: hash(await readFile(new URL(import.meta.url))),
    scope: 'Real Docker archive builds and A&D resets during SIGTERM and forced process death. Crash recovery injects lease expiry after death; no production recovery-time claim. VPN disabled.',
    observations, passed: !failure, failure: failure?.message }, null, 2) + '\n')
  await rm(directory, { recursive: true, force: true })
}
if (failure) throw failure
console.log(`Control-job shutdown checks passed: ${reportPath}`)
