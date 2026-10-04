import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { testConcurrency } from '../../../web/scripts/test-concurrency.mjs'

test('frontend workers retain their bounded local default', () => {
  const previous = process.env.RSCTF_FRONTEND_WORKERS
  delete process.env.RSCTF_FRONTEND_WORKERS
  try {
    assert.equal(testConcurrency(), 2)
    for (const value of ['1', '2', '3', '4']) {
      assert.equal(testConcurrency(value), Number(value))
      process.env.RSCTF_FRONTEND_WORKERS = value
      assert.equal(testConcurrency(), Number(value))
    }
  } finally {
    if (previous === undefined) delete process.env.RSCTF_FRONTEND_WORKERS
    else process.env.RSCTF_FRONTEND_WORKERS = previous
  }
})

test('malformed or unbounded frontend worker settings fail closed', () => {
  for (const value of ['', '0', '5', '100', '-1', '1.5', '01', '2 ', ' 2', '2\n', 'Infinity', 'NaN', null, 2]) {
    assert.throws(() => testConcurrency(value), /integer from 1 through 4/)
  }
})

test('CI uses the bounded setting without dropping test discovery or failure propagation', () => {
  const runner = readFileSync(new URL('../../../web/scripts/run-tests.mjs', import.meta.url), 'utf8')
  const workflow = readFileSync(new URL('../../../.github/workflows/ci.yml', import.meta.url), 'utf8')
  assert.ok(runner.indexOf('const concurrency = testConcurrency()') < runner.indexOf("const entries = findTests('src')"))
  assert.match(runner, /const testArgs = \['--test', `--test-concurrency=\$\{concurrency}`\]/)
  assert.match(runner, /testArgs\.push\(\.\.\.outFiles\)/)
  assert.match(runner, /if \(status !== 0\) process\.exitCode = status/)
  assert.match(workflow, /name: Run frontend tests\n\s+env:\n(?:\s+#.*\n)*\s+RSCTF_FRONTEND_WORKERS: 4\n\s+run: pnpm --dir web test/)
})
