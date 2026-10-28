#!/usr/bin/env node
// Read-only lookup experiment; this does not measure human/agent task completion.
import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { arch, cpus, platform, release } from 'node:os'
import { resolve, relative, sep } from 'node:path'
import { performance } from 'node:perf_hooks'
import { fileURLToPath } from 'node:url'

const skillRoot = fileURLToPath(new URL('../', import.meta.url))
const root = resolve(skillRoot, '../../..')
const trials = 11
// Curated owner oracles, checked against source. Row selection is supplied;
// interpreting a user's question and reading the map are outside the timings.
const cases = [
  ['OAuth cookie', 'application-map.md', '| Discord / OAuth callback', 'OAuthState', 'src/controllers/oauth_response.rs'],
  ['Client cache weight', 'frontend-map.md', '| SWR retention and weight', 'estimateCacheBytes', 'web/src/utils/CacheWeight.ts'],
  ['CTFtime scoreboard', 'application-map.md', '| Scoreboards, standings', 'ctftime', 'src/controllers/game/scoreboard_page.rs'],
  ['CSRF origin', 'backend-map.md', '| Cookies, CSRF', 'same_origin', 'src/middlewares/request_security.rs'],
  ['Durable job claims', 'backend-map.md', '| Durable jobs and mutation receipts', 'claim_next', 'src/services/control_jobs.rs'],
  ['Checker publication lock', 'backend-map.md', '| Repositories, checker publication', 'acquire_checker_artifact_guard', 'src/services/git_sync/checker_gc.rs'],
  ['Capture owner lock', 'runtime-map.md', '| Capture ownership, evidence', 'advisory_lock', 'src/services/traffic/capture/owner.rs'],
  ['Worker certificates', 'runtime-map.md', '| Trusted worker server', 'certificate', 'src/services/worker_pki.rs'],
  ['Kubernetes network policy', 'runtime-map.md', '| Kubernetes workloads', 'NetworkPolicy', 'src/services/k8s/network.rs'],
  ['Game workspace UI', 'frontend-map.md', '| `/games/:id/*`', 'WorkspaceBar', 'web/src/components/WorkspaceBar.tsx'],
  ['Expired-record cleanup', 'backend-map.md', '| Durable jobs and mutation receipts', 'SKIP LOCKED', 'src/services/mutation_retention/cleanup.rs'],
]
const broadRoots = ['src', 'web/src', 'agents', 'lib', 'tests', 'scripts', 'deploy', '.github']
const globs = ['rs', 'ts', 'tsx', 'mjs', 'js', 'sh', 'yml'].flatMap(ext => ['-g', `*.${ext}`])
const run = args => {
  try {
    return execFileSync('rg', ['--no-config', '--threads', '1', ...args], {
      cwd: root, encoding: 'utf8', maxBuffer: 16 * 1024 * 1024,
    })
  } catch (error) {
    if (error.status === 1) return ''
    throw error
  }
}
const files = text => text.split('\0').filter(Boolean).map(path => path.replaceAll('\\', '/'))
const projectPath = path => relative(root, path).split(sep).join('/')
const median = values => [...values].sort((a, b) => a - b)[Math.floor(values.length / 2)]
const hashes = {}
const results = []
for (const [task, document, row, pattern, owner] of cases) {
  const file = resolve(skillRoot, 'references', document)
  const source = readFileSync(file, 'utf8')
  hashes[projectPath(file)] = createHash('sha256').update(source).digest('hex')
  const line = source.split('\n').find(line => line.startsWith(row))
  if (!line) throw new Error(`Missing map row: ${task}`)
  const scopes = [...line.matchAll(/\]\(([^)]+)\)/g)]
    .map(match => projectPath(resolve(skillRoot, 'references', decodeURIComponent(match[1]))))
    .filter(path => /^(src|web|agents|lib|tests|scripts|deploy)\//.test(path))
  if (!scopes.length) throw new Error(`Missing source scopes: ${task}`)
  const baseline = { roots: broadRoots, times: [] }
  const mapped = { roots: scopes, times: [] }
  for (let trial = -1; trial < trials; trial++) {
    for (const mode of trial % 2 ? [mapped, baseline] : [baseline, mapped]) {
      const start = performance.now()
      mode.matches = files(run(['-l', '--null', ...globs, '--', pattern, ...mode.roots]))
      const duration = performance.now() - start
      if (trial >= 0) mode.times.push(duration)
    }
  }
  for (const mode of [baseline, mapped]) {
    if (!mode.matches.includes(owner)) throw new Error(`${task}: expected owner missing: ${owner}`)
  }
  results.push({ task, document, row, pattern, expectedOwner: owner, scopes,
    baselineCandidates: baseline.matches.length, mappedCandidates: mapped.matches.length,
    baselineMedianMs: median(baseline.times), mappedMedianMs: median(mapped.times),
    baselineSamplesMs: baseline.times, mappedSamplesMs: mapped.times,
    ownerPreserved: true })
}
console.log(JSON.stringify({
  scope: 'Curated first-owner lookup; preselected map rows; not end-to-end development speed',
  sourceRevision: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim(),
  sourceHashes: hashes,
  benchmarkSha256: createHash('sha256').update(readFileSync(fileURLToPath(import.meta.url))).digest('hex'),
  node: process.version, platform: platform(), kernel: release(), arch: arch(), cpu: cpus()[0]?.model,
  trials, warmupsPerCase: 1, order: 'alternating baseline/mapped pairs; single-thread rg; same patterns',
  searchUniverseFiles: files(run(['--files', '--null', ...globs, ...broadRoots])).length,
  results,
}, null, 2))
