import assert from 'node:assert/strict'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'
import { processGroupRunning } from '../process-control.mjs'
import { CoverageProcesses } from '../../../scripts/coverage/processes.mjs'

function fixture(context) {
  const directory = mkdtempSync(join(tmpdir(), 'rsctf-coverage-process-'))
  const runner = new CoverageProcesses()
  context.after(async () => {
    await runner.cancel(new Error('fixture cleanup'))
    assert.equal(runner.children.size, 0)
    assert.ok(runner.results.every(({ pid }) => !processGroupRunning(pid)))
    rmSync(directory, { recursive: true })
  })
  const options = (label, extra = {}) => ({ label, logPath: join(directory, `${label}.log`), ...extra })
  return { runner, options }
}

test('completed test outputs are retained separately and both children are reaped', async (context) => {
  const { runner, options } = fixture(context)
  const output = await Promise.all(['one', 'two'].map((label) =>
    runner.run(process.execPath, ['-e', `console.log('${label}')`], options(label))))
  assert.deepEqual(output, ['one\n', 'two\n'])
  assert.equal(runner.results.length, 2)
  assert.ok(runner.results.every((result) => result.code === 0))
  for (const result of runner.results) assert.equal(readFileSync(result.logPath, 'utf8'), `${result.label}\n`)
})

test('one failed child cancels its peer and no later invocation can start', async (context) => {
  const { runner, options } = fixture(context)
  const outcome = await Promise.allSettled([
    runner.run(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], options('peer')),
    runner.run(process.execPath, ['-e', 'setTimeout(() => process.exit(7), 100)'], options('failure')),
  ])
  assert.ok(outcome.every(({ status }) => status === 'rejected'))
  assert.ok(runner.results.some(({ code }) => code === 7))
  assert.equal(runner.children.size, 0)
  await assert.rejects(runner.run(process.execPath, ['-e', 'process.exit(0)'], options('never')), /process failed/)
  assert.equal(runner.results.length, 2)
})

test('explicit cancellation stops a real process group before returning', async (context) => {
  const { runner, options } = fixture(context)
  const pending = runner.run(process.execPath, ['-e',
    'const{spawn}=require("node:child_process"); spawn(process.execPath,["-e","setInterval(()=>{},1000)"],{stdio:"ignore"}); setInterval(()=>{},1000);'],
  options('group'))
  const outcome = assert.rejects(pending, /interrupted/)
  await new Promise((resolve) => setTimeout(resolve, 100))
  await runner.cancel(new Error('interrupted'))
  await outcome
  assert.equal(runner.children.size, 0)
})

test('timeout and output overflow fail rather than yielding partial successful results', async (context) => {
  const { runner, options } = fixture(context)
  await assert.rejects(runner.run(process.execPath, ['-e', 'setInterval(()=>{},1000)'],
    options('timeout', { timeoutMs: 50 })), /timed out/)
})

test('excessive output is bounded and its process is stopped', async (context) => {
  const { runner, options } = fixture(context)
  await assert.rejects(runner.run(process.execPath, ['-e', 'console.log("x".repeat(1000)); setInterval(()=>{},1000)'],
    options('overflow', { maxOutputBytes: 100 })), /output limit/)
  assert.equal(readFileSync(runner.results[0].logPath).length, 100)
})

test('a missing executable records the spawn failure without hanging cleanup', async (context) => {
  const { runner, options } = fixture(context)
  await assert.rejects(runner.run('/rsctf-test-does-not-exist', [], options('missing')), /ENOENT/)
  assert.equal(runner.results[0].spawnError, 'ENOENT')
})

test('log creation failure cancels an already-running peer', async (context) => {
  const { runner, options } = fixture(context)
  const waiting = runner.run(process.execPath, ['-e', 'setInterval(()=>{},1000)'], options('peer', { timeoutMs: 500 }))
  const peerOutcome = assert.rejects(waiting)
  await new Promise((resolve) => setTimeout(resolve, 30))
  const invalid = { ...options('invalid'), logPath: '/rsctf-log-test-does-not-exist/output.log' }
  await assert.rejects(runner.run(process.execPath, ['-e', 'process.exit(0)'], invalid))
  await peerOutcome
  assert.match(runner.error.message, /log creation failed/)
  assert.equal(runner.results.length, 1)
})
