#!/usr/bin/env node
import assert from 'node:assert/strict'
import { appendFileSync, readFileSync } from 'node:fs'
import { basename } from 'node:path'
import { setTimeout } from 'node:timers/promises'

const args = process.argv.slice(2)
const artifact = basename(args[2] ?? '')
assert.deepEqual(args, ['attestation', 'verify', `dist/${artifact}`,
  '--bundle', 'dist/rsctf-worker-agent-attestation.json',
  '--hostname', 'github.com', '--repo', 'example/rsctf',
  '--signer-workflow', 'example/rsctf/.github/workflows/worker-agent-release.yml',
  '--source-ref', 'refs/tags/v0.1.999', '--deny-self-hosted-runners'])
const event = (phase) => appendFileSync(process.env.ATTESTATION_TEST_EVENTS,
  `${JSON.stringify({ phase, artifact, pid: process.pid })}\n`)
event('start')
await setTimeout(100)
try {
  assert.equal(readFileSync(args[2], 'utf8'), `fixture:${artifact}`)
  assert.equal(readFileSync(args[4], 'utf8'), 'signed-bundle-fixture')
} catch {
  process.exitCode = 1
}
event('end')
