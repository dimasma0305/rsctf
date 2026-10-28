import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import test from 'node:test'
import { fileURLToPath } from 'node:url'
import { blobFixture } from '../restore-drill/blobs.mjs'

const script = fileURLToPath(new URL('../restore-drill.mjs', import.meta.url))
const env = {
  PATH: process.env.PATH,
  ARCHITECTURE_RUNTIME_BINARY: '/nonexistent/rsctf-restore-fixture',
  RESTORE_PG_CONTAINER: 'disposable-fixture',
  RSCTF_TEST_DATABASE_URL: 'postgres://fixture:fixture@127.0.0.1:5432/postgres',
  RSCTF_RESTORE_DISPOSABLE: '1',
}
test('restore object operations cannot address foreign roots, traversal hashes or remote S3', async () => {
  const store = await blobFixture('/unused', '1234567890abcdef', ['/owned'], 'local')
  await assert.rejects(store.remove('/foreign', 'a'.repeat(64)), /only owned storage roots/)
  await assert.rejects(store.read('/owned', '../outside'), /match/)
  await assert.rejects(blobFixture('/unused', '1234567890abcdef', ['/owned'], 's3',
    { RSCTF_S3_ENDPOINT: 'http://production.invalid' }), /must use loopback/)
})
for (const [name, overrides, expected] of [
  ['explicit disposable acknowledgement', { RSCTF_RESTORE_DISPOSABLE: '' }, /explicit disposable fixture acknowledgement/],
  ['loopback database', { RSCTF_TEST_DATABASE_URL: 'postgres://fixture:fixture@production.invalid/db' }, /only loopback disposable PostgreSQL/],
  ['container argument validation', { RESTORE_PG_CONTAINER: '--privileged' }, /disposable PostgreSQL container/],
  ['absolute binary', { ARCHITECTURE_RUNTIME_BINARY: 'rsctf' }, /absolute tested server binary/],
]) {
  test(`restore drill refuses unsafe setup: ${name}`, () => {
    const result = spawnSync(process.execPath, [script], { env: { ...env, ...overrides }, encoding: 'utf8', timeout: 5000 })
    assert.equal(result.status, 1)
    assert.match(result.stderr, expected)
  })
}
