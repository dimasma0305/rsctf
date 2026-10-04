import assert from 'node:assert/strict'
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'
import { processGroupRunning } from '../process-control.mjs'
import { runCoveragePartitions } from '../../../scripts/coverage/run.mjs'

function fixture(context, fault) {
  const root = mkdtempSync(join(tmpdir(), 'rsctf-coverage-runner-'))
  context.after(() => rmSync(root, { recursive: true }))
  const targetDirectory = join(root, 'target')
  mkdirSync(targetDirectory)
  const artifacts = ['library', 'integration', 'empty'].map((name) => {
    const executable = join(targetDirectory, name)
    copyFileSync(new URL('./fixtures/coverage-test.mjs', import.meta.url), executable)
    chmodSync(executable, 0o700)
    return { reason: 'compiler-artifact', package_id: 'rsctf-test-fixture',
      target: { name, kind: [name === 'library' ? 'lib' : 'test'] }, profile: { test: true }, executable }
  })
  const artifactFile = join(root, 'artifacts.jsonl')
  writeFileSync(artifactFile, [...artifacts, { reason: 'build-finished', success: true }]
    .map((item) => JSON.stringify(item)).join('\n'))
  writeFileSync(join(targetDirectory, 'default.profraw'), 'unit default profile, not LLVM\n')
  return { artifactFile, targetDirectory, evidenceDirectory: join(root, 'evidence'),
    services: [{ postgres: 'postgresql://postgres:postgres@127.0.0.1:15432/rsctf_test', redis: 'redis://127.0.0.1:16379' },
      { postgres: 'postgresql://postgres:postgres@127.0.0.1:15433/rsctf_test', redis: 'redis://127.0.0.1:16380' }],
    env: { ...process.env, CARGO_LLVM_COV: '1', CARGO_LLVM_COV_TARGET_DIR: targetDirectory,
      RSCTF_COVERAGE_FIXTURE_FAULT: fault || '' } }
}

test('orchestration executes the complete all-target union and retains every profile', async (context) => {
  const options = fixture(context)
  const result = await runCoveragePartitions(options)
  assert.equal(result.status, 'passed', result.error)
  assert.equal(result.selectedTests, 5)
  assert.equal(result.targets.length, 3)
  assert.ok(result.targets.some(({ names }) => names.length === 0))
  assert.equal(result.profileVerification.retainedDefaultProfiles, 1)
  assert.equal(result.profileVerification.newProfiles, 4)
  assert.equal(result.processes.length, 7) // Three discoveries and four target/shard invocations.
  assert.ok(result.processes.every(({ pid }) => !processGroupRunning(pid)))
  const persisted = JSON.parse(readFileSync(join(options.evidenceDirectory, 'result.json'), 'utf8'))
  assert.equal(persisted.status, 'passed')
  await assert.rejects(runCoveragePartitions(options), /overwrite coverage evidence/)
})

for (const fault of ['missing-profile', 'missing-test', 'exit-failure']) {
  test(`orchestration rejects ${fault} and returns only after its processes exit`, async (context) => {
    const result = await runCoveragePartitions(fixture(context, fault))
    assert.equal(result.status, 'failed')
    assert.ok(result.error)
    assert.ok(result.processes.every(({ pid }) => !processGroupRunning(pid)))
  })
}

test('an already-cancelled run starts no test process', async (context) => {
  const options = fixture(context)
  const controller = new AbortController()
  controller.abort()
  const result = await runCoveragePartitions({ ...options, signal: controller.signal })
  assert.equal(result.status, 'failed')
  assert.match(result.error, /interrupted/)
  assert.equal(result.processes.length, 0)
})

test('incomplete Cargo output is rejected before invoking any target', async (context) => {
  const options = fixture(context)
  writeFileSync(options.artifactFile, '{"reason":"compiler-artifact"}\n')
  const result = await runCoveragePartitions(options)
  assert.equal(result.status, 'failed')
  assert.equal(result.processes.length, 0)
})
