// Component measurements only; this does not claim an application HTTP speedup.
import { createHash } from 'node:crypto'
import { spawn, execFileSync } from 'node:child_process'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { build } from '../../web/node_modules/esbuild/lib/main.js'
import { setTimeout as pause } from 'node:timers/promises'
import { PG } from './lib.mjs'

const root = resolve(fileURLToPath(new URL('../..', import.meta.url)))
const output = resolve(
  process.env.SUMMARY_DIR || '/tmp/rsctf-client-retention-audit'
)
mkdirSync(output, { recursive: true })
const run = (command, args, options = {}) =>
  new Promise((accept, reject) => {
    const child = spawn(command, args, {
      cwd: root,
      env: process.env,
      ...options
    })
    let stdout = '',
      stderr = ''
    child.stdout.on('data', (data) => {
      stdout += data
    })
    child.stderr.on('data', (data) => {
      stderr += data
    })
    child.on('error', reject)
    child.on('exit', (code) =>
      code === 0 ? accept(stdout) : reject(new Error(stderr + '\n' + stdout))
    )
  })
const summary = (values) => {
  const xs = [...values].sort((a, b) => a - b),
    quantile = (q) => xs[Math.min(xs.length - 1, Math.ceil(q * xs.length) - 1)]
  return {
    count: xs.length,
    avg: xs.reduce((a, b) => a + b, 0) / xs.length,
    p50: quantile(0.5),
    p90: quantile(0.9),
    p95: quantile(0.95),
    p99: quantile(0.99),
    max: xs.at(-1)
  }
}
const dockerEnv = { ...process.env }
for (const key of [
  'DOCKER_HOST',
  'DOCKER_CONTEXT',
  'DOCKER_TLS',
  'DOCKER_TLS_VERIFY',
  'DOCKER_CERT_PATH'
])
  delete dockerEnv[key]
const measuredRun = async (binary, mode) => {
  let running = true
  const resources = [],
    resourceErrors = []
  const sampling = (async () => {
    while (running && process.env.PG_CONTAINER) {
      try {
        const sample = JSON.parse(
          await run(
            'docker',
            [
              '--host=unix:///var/run/docker.sock',
              'stats',
              '--no-stream',
              '--format',
              '{{json .}}',
              PG
            ],
            { env: dockerEnv }
          )
        )
        if (running)
          resources.push({
            at: new Date().toISOString(),
            cpuPercent: sample.CPUPerc,
            memory: sample.MemUsage
          })
      } catch (error) {
        resourceErrors.push(error.message)
      }
      if (running) await pause(500)
    }
  })()
  try {
    return {
      raw: JSON.parse(await run(binary, [mode])),
      resources,
      resourceErrors
    }
  } finally {
    running = false
    await sampling
  }
}
const report = {
  sourceSha: execFileSync('git', ['rev-parse', 'HEAD'], {
    cwd: root,
    encoding: 'utf8'
  }).trim(),
  baselineSha: '9ebf91ba957cd6cfe28d9aa88a4d4c13200f92b0',
  createdAt: new Date().toISOString(),
  cache: [],
  retention: []
}
report.candidateSources = Object.fromEntries(
  [
    'web/src/utils/Cache.ts',
    'web/src/utils/CacheWeight.ts',
    'src/services/mutation_retention/cleanup.rs'
  ].map((path) => [
    path,
    createHash('sha256')
      .update(readFileSync(resolve(root, path)))
      .digest('hex')
  ])
)
const baseline = resolve(output, 'cache-baseline.ts')
writeFileSync(
  baseline,
  execFileSync(
    'git',
    ['show', report.baselineSha + ':web/src/utils/Cache.ts'],
    { cwd: root, encoding: 'utf8' }
  )
)
for (const [mode, source] of [
  ['baseline', baseline],
  ['candidate', resolve(root, 'web/src/utils/Cache.ts')]
]) {
  const outfile = resolve(output, 'cache-' + mode + '.mjs')
  await build({
    entryPoints: [source],
    outfile,
    bundle: true,
    minify: true,
    platform: 'node',
    format: 'esm',
    logLevel: 'warning'
  })
}
// AB/BA ordering reduces the risk of attributing warmup or host drift to a mode.
let trial = 0
for (const mode of ['baseline', 'candidate', 'candidate', 'baseline']) {
  const raw = JSON.parse(
    await run(process.execPath, [
      '--expose-gc',
      resolve(root, 'tests/load/client-cache-audit-worker.mjs'),
      resolve(output, 'cache-' + mode + '.mjs')
    ])
  )
  writeFileSync(
    resolve(output, `cache-${trial++}-${mode}-raw.json`),
    JSON.stringify(raw)
  )
  const sample = {
    mode,
    ...raw,
    writeLatencyMs: summary(raw.writeLatencyMs),
    readLatencyMs: summary(raw.readLatencyMs),
    scheduleLatenessMs: summary(raw.scheduleLatenessMs)
  }
  report.cache.push(sample)
  console.log(JSON.stringify({ cache: sample }))
}
if (process.env.RSCTF_RETENTION_AUDIT_BIN) {
  const binary = resolve(process.env.RSCTF_RETENTION_AUDIT_BIN)
  report.retentionBinarySha256 = createHash('sha256')
    .update(readFileSync(binary))
    .digest('hex')
  for (const mode of ['baseline', 'candidate', 'candidate', 'baseline']) {
    const { raw, resources, resourceErrors } = await measuredRun(binary, mode)
    if (resourceErrors.length)
      throw new Error('Resource sampling failed: ' + resourceErrors.join('; '))
    writeFileSync(
      resolve(output, `retention-${trial++}-${mode}-raw.json`),
      JSON.stringify({ raw, resources })
    )
    const sample = { ...raw, latencyMs: summary(raw.latencyMs), resources }
    report.retention.push(sample)
    console.log(JSON.stringify({ retention: sample }))
  }
}
writeFileSync(
  resolve(output, 'summary.json'),
  JSON.stringify(report, null, 2) + '\n'
)
console.log('Results: ' + resolve(output, 'summary.json'))
