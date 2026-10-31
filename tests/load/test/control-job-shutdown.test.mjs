import assert from 'node:assert/strict'
import { execFile, spawnSync } from 'node:child_process'
import test from 'node:test'
import { fileURLToPath } from 'node:url'
import { promisify } from 'node:util'
import { removeAnonymousVolume } from '../control-job-shutdown/cleanup.mjs'

const script = fileURLToPath(new URL('../control-job-shutdown.mjs', import.meta.url))

test('shutdown cleanup accepts only the exact already-removed anonymous volume', async () => {
  const name = 'a'.repeat(64)
  const remove = stderr => async () => { throw Object.assign(new Error('Docker failed'), { stderr }) }
  assert.equal(await removeAnonymousVolume(remove(`Error response from daemon: get ${name}: no such volume\n`), name), false)
  for (const message of ['permission denied', 'volume is in use', 'daemon unavailable',
    `Error response from daemon: get ${'b'.repeat(64)}: no such volume`]) {
    await assert.rejects(removeAnonymousVolume(remove(message), name), /Docker failed/)
  }
  await assert.rejects(removeAnonymousVolume(async () => assert.fail('must not call Docker'), 'named-production-data'), /only tracked anonymous/)
})

test('real Docker cleanup handles volumes removed by container teardown and retains in-use errors', {
  skip: !process.env.RSCTF_REAL_DOCKER_RETRY_IMAGE,
}, async () => {
  const execute = promisify(execFile)
  const docker = args => execute('docker', ['--host=unix:///var/run/docker.sock', ...args], { timeout: 10000 })
  const { stdout } = await docker(['create', '--volume', '/audit-cleanup', '--entrypoint', '/bin/true', process.env.RSCTF_REAL_DOCKER_RETRY_IMAGE])
  const id = stdout.trim()
  try {
    const inspect = JSON.parse((await docker(['inspect', id])).stdout)[0]
    const volume = inspect.Mounts.find(mount => mount.Destination === '/audit-cleanup').Name
    await assert.rejects(removeAnonymousVolume(docker, volume), /in use/)
    await docker(['rm', '--volumes', id])
    assert.equal(await removeAnonymousVolume(docker, volume), false)
    const loose = (await docker(['volume', 'create'])).stdout.trim()
    assert.equal(await removeAnonymousVolume(docker, loose), true)
    assert.equal(await removeAnonymousVolume(docker, loose), false)
  } finally {
    await docker(['rm', '--force', '--volumes', id]).catch(() => {})
  }
})
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
  ['CI development fallback', { CI: 'true', SHUTDOWN_ROLE: 'development', SHUTDOWN_SCENARIO: 'rollout' }, /CI requires production confinement/],
]) {
  test(`shutdown harness refuses unsafe setup: ${name}`, () => {
    const result = spawnSync(process.execPath, [script], { env: { ...env, ...overrides }, encoding: 'utf8', timeout: 5000 })
    assert.equal(result.status, 1)
    assert.match(result.stderr, expected)
  })
}
