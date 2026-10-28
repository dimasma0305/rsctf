import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import test from 'node:test'
import { fileURLToPath } from 'node:url'

const script = fileURLToPath(new URL('../control-job-shutdown.mjs', import.meta.url))
const env = {
  PATH: process.env.PATH,
  ARCHITECTURE_RUNTIME_BINARY: '/nonexistent/shutdown-fixture',
  SHUTDOWN_PG_CONTAINER: 'disposable-fixture',
  RSCTF_TEST_DATABASE_URL: 'postgres://fixture:fixture@127.0.0.1:5432/postgres',
  RSCTF_SHUTDOWN_DISPOSABLE: '1',
}
for (const [name, overrides, expected] of [
  ['acknowledgement', { RSCTF_SHUTDOWN_DISPOSABLE: '' }, /explicit disposable acknowledgement/],
  ['remote database', { RSCTF_TEST_DATABASE_URL: 'postgres://fixture@production.invalid/db' }, /only loopback PostgreSQL/],
  ['container option injection', { SHUTDOWN_PG_CONTAINER: '--privileged' }, /disposable PostgreSQL container/],
  ['relative binary', { ARCHITECTURE_RUNTIME_BINARY: 'rsctf' }, /absolute tested server binary/],
]) {
  test(`shutdown harness refuses unsafe setup: ${name}`, () => {
    const result = spawnSync(process.execPath, [script], { env: { ...env, ...overrides }, encoding: 'utf8', timeout: 5000 })
    assert.equal(result.status, 1)
    assert.match(result.stderr, expected)
  })
}
