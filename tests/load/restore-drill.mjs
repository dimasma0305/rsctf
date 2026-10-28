// Destructive only to databases/directories created by this invocation.
// A stopped-writer local-files restore drill, not a production backup command.
import assert from 'node:assert/strict'
import { execFile, spawn } from 'node:child_process'
import { promisify } from 'node:util'
import { randomBytes, randomUUID, createHash, createHmac } from 'node:crypto'
import { cp, mkdir, mkdtemp, readFile, writeFile, rm, stat } from 'node:fs/promises'
import { tmpdir, release } from 'node:os'
import path from 'node:path'
import net from 'node:net'
import { setTimeout as pause } from 'node:timers/promises'
import { blobFixture } from './restore-drill/blobs.mjs'

const execute = promisify(execFile)
const binary = process.env.ARCHITECTURE_RUNTIME_BINARY
const container = process.env.RESTORE_PG_CONTAINER
const database = new URL(process.env.RSCTF_TEST_DATABASE_URL)
assert.ok(binary && path.isAbsolute(binary), 'absolute tested server binary required')
assert.ok(container && /^[a-zA-Z0-9][a-zA-Z0-9_.-]*$/.test(container), 'disposable PostgreSQL container required')
assert.equal(database.hostname, '127.0.0.1', 'only loopback disposable PostgreSQL is supported')
assert.equal(process.env.RSCTF_RESTORE_DISPOSABLE, '1', 'explicit disposable fixture acknowledgement required')
const id = randomBytes(8).toString('hex')
const source = `rsctf_restore_${id}_source`
const restored = `rsctf_restore_${id}_restored`
const owned = new Set()
const directory = await mkdtemp(path.join(tmpdir(), 'rsctf-restore-'))
const files = path.join(directory, 'files')
const recovered = path.join(directory, 'recovered')
const backup = path.join(directory, 'backup')
const reportPath = path.resolve(process.env.RESTORE_REPORT || 'tests/load/results/restore-drill.json')
const observations = []
let active
let failure
let expectedScore
let objects
const backend = process.env.RESTORE_STORAGE_BACKEND || 'local'
const config = {
  RSCTF_JWT_SECRET: randomBytes(32).toString('hex'),
  RSCTF_IDENTITY_HASH_KEY: randomBytes(32).toString('hex'),
  RSCTF_EVENT_VPN_CREDENTIAL_KEY: randomBytes(32).toString('hex'),
  RSCTF_CONTAINER_BACKEND: 'none', RSCTF_STORAGE_BACKEND: 'local',
  RSCTF_AD_VPN_ENABLED: 'false', RSCTF_COOKIE_SECURE: 'false',
  RSCTF_MIGRATE: '0', RSCTF_DB_MAX_CONNECTIONS: '40', TOKIO_WORKER_THREADS: '2',
  RUST_LOG: 'warn',
}
const user = randomUUID()
const payloads = [Buffer.from('restore drill poster\n'), Buffer.from('%PDF-1.4\nrestore drill writeup\n')]
const hashes = payloads.map(bytes => createHash('sha256').update(bytes).digest('hex'))
const pg = (tool, args, options = {}) => execute('docker', [
  'exec', '-i', container, tool, '-U', decodeURIComponent(database.username), ...args,
], { timeout: 90_000, maxBuffer: 64 * 1024 * 1024, ...options })
async function sql(name, statement) {
  return (await pg('psql', ['-X', '-v', 'ON_ERROR_STOP=1', '-d', name, '-qAtc', statement])).stdout.trim()
}
async function create(name) {
  await sql('postgres', `CREATE DATABASE "${name}"`)
  owned.add(name)
}
async function drop(name) {
  assert.ok(owned.has(name))
  await sql('postgres', `DROP DATABASE "${name}" WITH (FORCE)`)
  owned.delete(name)
}
async function start(name, root, role, secrets = config) {
  assert.ok(!active)
  const socket = net.createServer()
  await new Promise(resolve => socket.listen(0, '127.0.0.1', resolve))
  const port = socket.address().port
  await new Promise(resolve => socket.close(resolve))
  const url = new URL(database); url.pathname = '/' + name
  const env = {
    ...process.env, ...objects.config(root), ...secrets,
    RSCTF_STORAGE_BACKEND: backend,
    ...(backend === 's3' ? { RSCTF_S3_PREFIX: objects.config(root).RSCTF_S3_PREFIX } : {}),
    RSCTF_DATABASE_URL: url.href, RSCTF_STORAGE_ROOT: root,
    RSCTF_ROLE: role, RSCTF_BIND: `127.0.0.1:${port}`,
  }
  delete env.RSCTF_REDIS_URL
  const child = spawn(binary, [], { env, stdio: ['ignore', 'pipe', 'pipe'] })
  let log = ''
  for (const stream of [child.stdout, child.stderr]) stream.on('data', data => { log = (log + data).slice(-65536) })
  const item = { child, base: `http://127.0.0.1:${port}`, result: null, log: () => log }
  item.exited = new Promise(resolve => {
    child.once('error', () => { item.result = { code: -1 }; resolve(item.result) })
    child.once('exit', (code, signal) => { item.result = { code, signal }; resolve(item.result) })
  })
  active = item
  if (role === 'migrate') { await stop(false); return }
  for (let i = 0; i < 300; i++) {
    assert.equal(item.result, null, 'server exited before readiness')
    try {
      const response = await fetch(item.base + '/healthz', { signal: AbortSignal.timeout(1000) })
      if (response.status === 200 && await response.text() === 'ok') return
    } catch {}
    await pause(100)
  }
  assert.fail('restored server failed readiness')
}
async function stop(signal = true) {
  if (!active) return
  const item = active
  if (signal) item.child.kill('SIGTERM')
  const deadline = setTimeout(() => item.child.kill('SIGKILL'), 60_000)
  try {
    const result = await item.exited
    assert.equal(result.code, 0, 'server failed; inspect private fixture diagnostics')
    assert.equal(result.signal, null)
  } finally {
    clearTimeout(deadline)
    await writeFile(path.join(directory, 'server.log'), item.log(), { mode: 0o600 })
    active = null
  }
}
async function snapshot(name) {
  const tables = JSON.parse(await sql(name, `SELECT json_agg(tablename ORDER BY tablename)
    FROM pg_tables WHERE schemaname='public'`))
  const quote = value => '"' + value.replaceAll('"', '""') + '"'
  const rows = await sql(name, tables.map(table => `SELECT json_build_object(
    'table', '${table.replaceAll("'", "''")}', 'count', count(*),
    'digest', md5(COALESCE(string_agg(to_jsonb(t)::text, E'\\n' ORDER BY to_jsonb(t)::text), '')))
    FROM public.${quote(table)} t`).join(' UNION ALL '))
  return rows.split('\n').map(JSON.parse).sort((a, b) => a.table.localeCompare(b.table))
}
async function inventory(name, root) {
  const entries = JSON.parse(await sql(name, 'SELECT json_agg(json_build_object(\'hash\', hash, \'size\', file_size)) FROM "Files"'))
  for (const entry of entries) {
    assert.match(entry.hash, /^[a-f0-9]{64}$/)
    const bytes = await objects.read(root, entry.hash)
    assert.equal(bytes.length, Number(entry.size), 'restored object size mismatch')
    assert.equal(createHash('sha256').update(bytes).digest('hex'), entry.hash, 'restored object hash mismatch')
  }
  return entries.length
}
function token(secret) {
  const encode = value => Buffer.from(JSON.stringify(value)).toString('base64url')
  const now = Math.floor(Date.now() / 1000)
  const body = encode({ alg: 'HS256', typ: 'JWT' }) + '.' +
    encode({ sub: user, role: 3, name: 'restore-admin', stamp: 'restore-stamp', iat: now, exp: now + 3600 })
  return body + '.' + createHmac('sha256', secret).update(body).digest('base64url')
}
async function smoke(session, expected = 200) {
  const profile = await fetch(active.base + '/api/account/profile', {
    headers: { Cookie: `RSCTF_Token=${session}` }, signal: AbortSignal.timeout(5000),
  })
  assert.equal(profile.status, expected, 'restored session authentication')
  if (expected !== 200) return
  const board = await fetch(active.base + '/api/game/1/scoreboard', { signal: AbortSignal.timeout(5000) })
  assert.equal(board.status, 200)
  const team = (await board.json()).items.find(item => item.id === 1)
  assert.ok(team && team.score > 0 && team.solvedCount === 1, 'restored competition score must remain nonzero')
  expectedScore ??= team.score
  assert.equal(team.score, expectedScore, 'restore must preserve the published score')
  for (const hash of hashes) {
    const response = await fetch(active.base + `/assets/${hash}/restore-fixture`, {
      headers: { Cookie: `RSCTF_Token=${session}` }, signal: AbortSignal.timeout(5000),
    })
    assert.equal(response.status, 200)
    assert.equal(createHash('sha256').update(Buffer.from(await response.arrayBuffer())).digest('hex'), hash)
  }
}
try {
  await mkdir(backup, { mode: 0o700 })
  await mkdir(files)
  objects = await blobFixture(directory, id, [files, recovered], backend)
  await create(source)
  await start(source, files, 'migrate')
  const seed = await readFile(new URL('./restore-drill/seed.sql', import.meta.url), 'utf8')
  await sql(source, seed.replaceAll(':USER:', user).replaceAll(':POSTER:', hashes[0])
    .replaceAll(':WRITEUP:', hashes[1]).replaceAll(':POSTER_SIZE:', String(payloads[0].length))
    .replaceAll(':WRITEUP_SIZE:', String(payloads[1].length)))
  for (const [i, hash] of hashes.entries()) {
    await objects.write(files, hash, payloads[i])
  }
  await mkdir(path.join(files, 'repositories'))
  await writeFile(path.join(files, 'repositories', 'restore-marker'), 'mutable working state\n')
  const session = token(config.RSCTF_JWT_SECRET)
  await start(source, files, 'development')
  await smoke(session)
  await stop()
  // No writers exist from this point through the database/file/config snapshot.
  const expected = await snapshot(source)
  const backupStarted = performance.now()
  const dump = (await pg('pg_dump', ['-Fc', '--no-owner', '--no-acl', '-d', source], { encoding: 'buffer' })).stdout
  await writeFile(path.join(backup, 'database.dump'), dump, { mode: 0o600 })
  await cp(files, path.join(backup, 'files'), { recursive: true })
  await mkdir(path.join(backup, 'objects'))
  for (const hash of hashes) await writeFile(path.join(backup, 'objects', hash), await objects.read(files, hash))
  await writeFile(path.join(backup, 'config.json'), JSON.stringify({ ...config, ...objects.config(files) }), { mode: 0o600 })
  observations.push({ event: 'backup', milliseconds: performance.now() - backupStarted, databaseBytes: dump.length })
  await drop(source)
  for (const hash of hashes) await objects.remove(files, hash)
  await rm(files, { recursive: true })
  const restoreStarted = performance.now()
  await create(restored)
  // execFile has no input option; stream the dump into pg_restore and await both
  // transport and process completion so a partial restore never counts as success.
  const child = spawn('docker', ['exec', '-i', container, 'pg_restore', '-U',
    decodeURIComponent(database.username), '--exit-on-error', '--no-owner', '--no-acl', '-d', restored],
  { stdio: ['pipe', 'ignore', 'pipe'] })
  let restoreError = ''
  child.stderr.on('data', data => { restoreError = (restoreError + data).slice(-4096) })
  const exited = new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', resolve) })
  child.stdin.on('error', () => {}) // exit status remains authoritative after EPIPE
  child.stdin.end(await readFile(path.join(backup, 'database.dump')))
  const timeout = setTimeout(() => child.kill('SIGKILL'), 90_000)
  try { assert.equal(await exited, 0, 'pg_restore failed: ' + restoreError) } finally { clearTimeout(timeout) }
  await cp(path.join(backup, 'files'), recovered, { recursive: true })
  for (const hash of hashes) await objects.write(recovered, hash, await readFile(path.join(backup, 'objects', hash)))
  const secrets = JSON.parse(await readFile(path.join(backup, 'config.json'), 'utf8'))
  assert.deepEqual(await snapshot(restored), expected, 'all public table rows must survive exactly')
  assert.equal(await inventory(restored, recovered), 2)
  assert.equal(await readFile(path.join(recovered, 'repositories', 'restore-marker'), 'utf8'), 'mutable working state\n')
  await objects.remove(recovered, hashes[1])
  await assert.rejects(inventory(restored, recovered), { code: 'ENOENT' })
  await objects.write(recovered, hashes[1], Buffer.alloc(payloads[1].length))
  await assert.rejects(inventory(restored, recovered), /hash mismatch/)
  await objects.write(recovered, hashes[1], payloads[1])
  await start(restored, recovered, 'development', secrets)
  await smoke(session)
  await smoke(token(randomBytes(32).toString('hex')), 401)
  observations.push({ event: 'restored-ready-and-smoked', milliseconds: performance.now() - restoreStarted,
    tables: expected.length, referencedObjects: 2, missingAndCorruptObjectsRejected: true, wrongSecretRejected: true })
  await stop()
} catch (error) { failure = error }
finally {
  try { await stop() } catch (error) { failure ??= error }
  try { await objects?.cleanup() } catch (error) { failure ??= error }
  for (const name of owned) { try { await drop(name) } catch (error) { failure ??= error } }
  await mkdir(path.dirname(reportPath), { recursive: true })
  if (failure) {
    let diagnostic = await readFile(path.join(directory, 'server.log'), 'utf8').catch(() => '')
    for (const value of [database.href, decodeURIComponent(database.password),
      config.RSCTF_JWT_SECRET, config.RSCTF_IDENTITY_HASH_KEY, config.RSCTF_EVENT_VPN_CREDENTIAL_KEY,
      process.env.RSCTF_S3_ACCESS_KEY, process.env.RSCTF_S3_SECRET_KEY]) {
      if (value) diagnostic = diagnostic.replaceAll(value, '[redacted]')
    }
    await writeFile(reportPath + '.log', diagnostic, { mode: 0o600 })
  }
  await writeFile(reportPath, JSON.stringify({
    date: new Date().toISOString(), node: process.version, kernel: release(),
    sourceRevision: process.env.GITHUB_SHA || (await execute('git', ['rev-parse', 'HEAD'])).stdout.trim(),
    binarySourceRevision: process.env.ARCHITECTURE_RUNTIME_SOURCE_REVISION || process.env.GITHUB_SHA || null,
    harnessSha256: createHash('sha256').update(await readFile(new URL(import.meta.url))).digest('hex'),
    seedSha256: createHash('sha256').update(await readFile(new URL('./restore-drill/seed.sql', import.meta.url))).digest('hex'),
    blobAdapterSha256: createHash('sha256').update(await readFile(new URL('./restore-drill/blobs.mjs', import.meta.url))).digest('hex'),
    binarySha256: createHash('sha256').update(await readFile(binary)).digest('hex'),
    backend,
    scope: 'Stopped-writer PostgreSQL dump/restore, referenced blobs, mutable files and configuration. No multi-host or live-workload recovery.',
    observations, passed: !failure,
  }, null, 2) + '\n')
  await rm(directory, { recursive: true, force: true })
}
if (failure) throw failure
assert.ok((await stat(reportPath)).size > 0)
console.log('Disposable database/files/configuration restore drill passed')
