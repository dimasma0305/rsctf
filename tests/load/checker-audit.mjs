// Real checker-preparation component benchmark. No HTTP throughput claim.
import { execFile, spawn } from 'node:child_process'
import { promisify } from 'node:util'
import { readFile, writeFile } from 'node:fs/promises'
import { createHash } from 'node:crypto'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { setTimeout as pause } from 'node:timers/promises'

const execute = promisify(execFile)
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..')
const { stdout: common } = await execute('git', ['rev-parse', '--path-format=absolute', '--git-common-dir'], { cwd: root })
if (process.env.RSCTF_CHECKER_AUDIT_LOCKED !== '1') {
  const child = spawn('flock', [path.join(common.trim(), 'rsctf-build.lock'), process.execPath, fileURLToPath(import.meta.url)], {
    cwd: root, stdio: 'inherit', env: { ...process.env, RSCTF_CHECKER_AUDIT_LOCKED: '1' },
  })
  const code = await new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', resolve) })
  process.exit(code ?? 1)
}
const binary = process.env.CHECKER_AUDIT_BIN ?? path.join(common.trim(), 'rsctf-target/release/rsctf-checker-audit')
const pgContainer = process.env.CHECKER_AUDIT_PG_CONTAINER
if (!pgContainer) throw new Error('Set CHECKER_AUDIT_PG_CONTAINER to the disposable PostgreSQL container')
const { stdout: pgImage } = await execute('docker', ['inspect', '--format', '{{.Image}}', pgContainer])
const status = await readFile('/proc/self/status', 'utf8')
const allowed = status.match(/^Cpus_allowed_list:\s*(.*)$/m)[1].split(',').flatMap(part => {
  const [start, end = start] = part.split('-').map(Number)
  return Array.from({ length: end - start + 1 }, (_, index) => start + index)
})
const cpus = allowed.slice(0, 2).join(',')
const samples = []
for (const order of [['before', 'after'], ['after', 'before'], ['before', 'after']]) {
  for (const mode of order) {
    let output = ''
    let done = false
    const databaseSamples = []
    const child = spawn('taskset', ['-c', cpus, binary, mode], {
      cwd: root, env: process.env, stdio: ['ignore', 'pipe', 'inherit'],
    })
    child.stdout.setEncoding('utf8')
    child.stdout.on('data', chunk => { output += chunk })
    const result = new Promise((resolve, reject) => {
      child.once('error', reject)
      child.once('exit', (code, signal) => { done = true; resolve({ code, signal }) })
    })
    const observer = (async () => {
      while (!done) {
        const { stdout } = await execute('docker', ['stats', '--no-stream', '--format', '{{json .}}', pgContainer])
        databaseSamples.push(JSON.parse(stdout))
        if (!done) await pause(250)
      }
    })()
    const timer = setTimeout(() => child.kill('SIGTERM'), 180_000)
    let exit
    try { exit = await result } finally { clearTimeout(timer); done = true; await observer }
    if (exit.code !== 0) throw new Error(`Checker ${mode} failed: ${JSON.stringify(exit)}`)
    const sample = JSON.parse(output)
    if (sample.integrity_errors !== 0) throw new Error('Checker integrity failed')
    samples.push({ ...sample, databaseSamples })
    console.error(`${mode}: p95 arrival ${sample.arrival_latency.p95_ms.toFixed(1)} ms, lock hold ${sample.lock_hold.p95_ms.toFixed(1)} ms, mean occupied connections ${sample.pool_occupied_avg.toFixed(3)}`)
  }
}
const files = ['src/services/git_sync/checker.rs', 'src/services/git_sync/checker_gc.rs',
  'src/services/git_sync/checker_gc_scan.rs', 'src/utils/single_flight.rs', 'src/utils/database.rs',
  'src/utils/archive.rs', 'tests/load/checker-audit/src/main.rs', 'tests/load/checker-audit/src/support.rs',
  'tests/load/checker-audit/Cargo.toml', 'tests/load/checker-audit/Cargo.lock', 'tests/load/checker-audit.mjs']
const hashes = {}
for (const file of files) hashes[file] = createHash('sha256').update(await readFile(path.join(root, file))).digest('hex')
const { stdout: sourceSha } = await execute('git', ['rev-parse', 'HEAD'], { cwd: root })
const { stdout: python } = await execute('/usr/bin/python3', ['--version'])
const result = { date: new Date().toISOString(), sourceSha: sourceSha.trim(), hashes,
  binarySha256: createHash('sha256').update(await readFile(binary)).digest('hex'),
  cpu: os.cpus()[0]?.model, affinity: cpus, kernel: os.release(), python: python.trim(),
  postgresImage: pgImage.trim(), poolSize: 4, concurrentPreparations: 4,
  fixture: '16 KiB file plus run.py; real Python venv with bundled pip, no external requirements',
  scope: 'production preparation/publication/locks; before acquires the guard before preparation; excludes HTTP, full import mutation, dependency downloads, and app metric atomics',
  samples }
await writeFile(path.join(root, 'tests/load/results/architecture-checker.json'), `${JSON.stringify(result, null, 2)}\n`)
