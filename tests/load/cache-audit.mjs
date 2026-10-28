// Component benchmark; deliberately does not report HTTP/platform throughput.
import { execFile, spawn } from 'node:child_process'
import { promisify } from 'node:util'
import { readFile, writeFile } from 'node:fs/promises'
import { createHash } from 'node:crypto'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const execute = promisify(execFile)
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..')
const { stdout: common } = await execute('git', ['rev-parse', '--path-format=absolute', '--git-common-dir'], { cwd: root })
// Measurements share the repository's build slot so compilation cannot silently
// contaminate CPU/tail-latency results on this host.
if (process.env.RSCTF_CACHE_AUDIT_LOCKED !== '1') {
  const child = spawn('flock', [path.join(common.trim(), 'rsctf-build.lock'), process.execPath, fileURLToPath(import.meta.url)], {
    cwd: root, stdio: 'inherit', env: { ...process.env, RSCTF_CACHE_AUDIT_LOCKED: '1' },
  })
  const code = await new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', resolve) })
  process.exit(code ?? 1)
}
const binary = process.env.CACHE_AUDIT_BIN ?? path.join(common.trim(), 'rsctf-target/release/rsctf-cache-audit')
const { stdout: sourceSha } = await execute('git', ['rev-parse', 'HEAD'], { cwd: root })
const sourceHashes = {}
for (const name of ['src/services/cache.rs', 'src/services/cache/response.rs', 'tests/load/cache-audit/src/main.rs', 'tests/load/cache-audit/Cargo.lock']) {
  sourceHashes[name] = createHash('sha256').update(await readFile(path.join(root, name))).digest('hex')
}
const binarySha256 = createHash('sha256').update(await readFile(binary)).digest('hex')
const samples = []
for (const scenario of ['churn', 'expiry']) {
  for (const order of [ ['baseline', 'sharded', 'moka'], ['moka', 'baseline', 'sharded'], ['sharded', 'moka', 'baseline'] ]) {
    for (const mode of order) {
      const { stdout } = await execute(binary, [mode, scenario], { cwd: root, timeout: 120_000, maxBuffer: 64 * 1024 })
      const sample = JSON.parse(stdout)
      if (sample.operations !== Math.floor(sample.rate * sample.seconds / 4) * 4) throw new Error('Incomplete schedule')
      samples.push(sample)
      console.error(`${scenario}/${mode}: ${sample.operations} ops, p99 ${sample.service.p99_us.toFixed(2)} us, CPU ${sample.cpu_percent.toFixed(1)}%`)
    }
  }
}
const result = { date: new Date().toISOString(), node: process.version, kernel: os.release(), cpu: os.cpus()[0]?.model,
  sourceSha: sourceSha.trim(), sourceHashes, binarySha256, ttlMilliseconds: { churn: 60_000, expiry: 100 },
  profile: 'release opt-level=3 lto=fat codegen-units=1', workingSet: 16384, capacity: 4096, valueBytes: 1024,
  workers: 4, readsPercent: 80, writesPercent: 20, samples }
if (process.env.SUMMARY_JSON) await writeFile(process.env.SUMMARY_JSON, `${JSON.stringify(result, null, 2)}\n`)
else console.log(JSON.stringify(result, null, 2))
