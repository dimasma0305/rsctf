// Fixed-rate authenticated exporter load on an explicitly selected local process.
import assert from 'node:assert/strict'
import { execFile, spawn } from 'node:child_process'
import { promisify } from 'node:util'
import { readFile, writeFile } from 'node:fs/promises'
import { setTimeout as pause } from 'node:timers/promises'

const execute = promisify(execFile)
const target = new URL(process.env.TARGET)
assert.ok(['127.0.0.1', 'localhost'].includes(target.hostname) && target.protocol === 'http:')
const pid = Number(process.env.METRICS_APP_PID)
const pg = process.env.PG_CONTAINER
const token = process.env.RSCTF_METRICS_TOKEN
const summary = process.env.SUMMARY_JSON
assert.ok(Number.isSafeInteger(pid) && pid > 0 && pg && token?.length >= 32 && summary)
for (const headers of [{}, { Authorization: 'Bearer wrong' }]) {
  const response = await fetch(new URL('/metrics', target), { headers, signal: AbortSignal.timeout(3000) })
  assert.equal(response.status, 401)
  assert.equal(await response.text(), '')
}
const response = await fetch(new URL('/metrics', target), {
  headers: { Authorization: `Bearer ${token}` }, signal: AbortSignal.timeout(3000),
})
assert.equal(response.status, 200)
const initial = await response.text()
const { stdout } = await execute('getconf', ['CLK_TCK'])
const hz = Number(stdout.trim())
let stopped = false
let previous
const samples = []
const errors = []
const sampler = (async () => {
  while (!stopped) {
    try {
      const at = performance.now()
      const fields = (await readFile(`/proc/${pid}/stat`, 'utf8')).split(') ')[1].split(' ')
      const ticks = Number(fields[11]) + Number(fields[12])
      const status = await readFile(`/proc/${pid}/status`, 'utf8')
      const memory = Number(status.match(/^VmRSS:\s+(\d+)/m)[1]) * 1024
      const { stdout: stats } = await execute('docker', ['stats', '--no-stream', '--format', '{{json .}}', pg])
      if (previous) samples.push({ appCpuPercent: (ticks - previous.ticks) / hz / ((at - previous.at) / 1000) * 100,
        appRssBytes: memory, postgres: JSON.parse(stats) })
      previous = { at, ticks }
    } catch (error) { errors.push(error.message) }
    if (!stopped) await pause(250)
  }
})()
let code
try {
  const child = spawn('k6', ['run', '--summary-export', summary,
    new URL('./k6/metrics-scrape.js', import.meta.url).pathname], { env: process.env, stdio: 'inherit' })
  code = await new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', resolve) })
} finally { stopped = true; await sampler }
await writeFile(`${summary}.resources.json`, JSON.stringify({ samples, errors,
  responseBytes: Buffer.byteLength(initial), series: initial.split('\n').filter(line => line && !line.startsWith('#')).length }, null, 2))
assert.ok(samples.length >= 3 && errors.length === 0, JSON.stringify(errors))
assert.equal(code, 0, 'exporter load thresholds failed')
