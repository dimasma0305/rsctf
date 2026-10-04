import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { databaseExclusions, fixtureEndpoints, ignoredTests, inside, partitionTargets,
  testArtifacts, verifyInvocation, verifyProfileInputs } from '../../../scripts/coverage/partition.mjs'

const artifact = (name = 'library', kind = 'lib') => ({ reason: 'compiler-artifact', package_id: 'rsctf 0.1.137',
  target: { name, kind: [kind] }, profile: { test: true }, executable: `/coverage/debug/deps/${name}-123` })
const finished = { reason: 'build-finished', success: true }
const stream = (...items) => items.map((item) => JSON.stringify(item)).join('\n')
const profile = (name, value = 'a') => ({ name, bytes: 10, sha256: value.repeat(64) })
const services = () => [{ postgres: 'postgresql://postgres:postgres@127.0.0.1:15432/rsctf_test',
  redis: 'redis://127.0.0.1:16379' }, { postgres: 'postgresql://postgres:postgres@127.0.0.1:15433/rsctf_test',
  redis: 'redis://127.0.0.1:16380' }]

test('Cargo discovery requires successful complete output and every distinct executable', () => {
  const items = testArtifacts(stream(artifact(), artifact('routes', 'test'), finished), '/coverage')
  assert.equal(items.length, 2)
  for (const invalid of [stream(artifact()), stream(finished), stream(artifact(), finished, finished),
    stream(artifact(), { ...finished, success: false }), stream(artifact(), finished, artifact('late')),
    stream(artifact(), artifact(), finished), stream({ ...artifact(), executable: '/production/server' }, finished),
    stream({ ...artifact(), executable: '/coverage-other/test' }, finished),
    stream({ ...artifact(), executable: '/coverage/../production/server' }, finished),
    stream({ ...artifact(), target: { name: 'library', kind: ['proc-macro'] } }, finished),
    stream(artifact(), { reason: 'compiler-message', message: { level: 'warning' } }, finished),
    `${stream(artifact(), finished)}\ntruncated{`]) {
    assert.throws(() => testArtifacts(invalid, '/coverage'))
  }
  assert.equal(inside('/coverage/deps/test', '/coverage'), true)
  assert.equal(inside('/coverage', '/coverage'), false)
  assert.equal(inside('relative/test', '/coverage'), false)
})

test('zero-ignored-case targets are represented without allowing an empty total selection', () => {
  assert.deepEqual(ignoredTests('\n0 tests, 0 benchmarks\n'), [])
  assert.deepEqual(ignoredTests('z: test\na: test\n2 tests, 0 benchmarks'), ['a', 'z'])
  for (const value of ['', '0 tests', 'a: test\n0 tests, 0 benchmarks',
    '--skip: test\n1 test, 0 benchmarks', ': test\n1 test, 0 benchmarks']) {
    assert.throws(() => ignoredTests(value))
  }
  assert.throws(() => partitionTargets([{ id: 'empty', names: [] }]))
  assert.throws(() => partitionTargets([{ id: 'small', names: ['a'] }]))
})

test('partitioning is complete across library and integration targets, even with shared test names', () => {
  const discovered = [{ id: 'lib', names: ['one', 'two', 'three'] },
    { id: 'routes', names: ['one', 'two'] }, { id: 'no-ignored', names: [] }]
  const partition = partitionTargets(discovered)
  assert.deepEqual(partition.map((group) => group.reduce((sum, item) => sum + item.names.length, 0)), [3, 2])
  assert.deepEqual(partitionTargets([...discovered].reverse()), partition)
  assert.equal(partitionTargets(discovered, 1).flatMap((group) => group.flatMap(({ names }) => names)).length, 5)
  assert.throws(() => partitionTargets([...discovered, discovered[0]]))
  assert.throws(() => partitionTargets([{ id: 'lib', names: ['one', 'one'] }]))
})

test('an empty exact filter, duplicate results, ignored cases and missing cases fail closed', () => {
  const value = 'test one ... ok\ntest two ... ok\ntest result: ok. 2 passed; 0 failed; 0 ignored;\n'
  assert.ok(verifyInvocation(value, ['one', 'two']))
  for (const names of [[], ['one'], ['one', 'one'], ['one', 'two', 'three']]) {
    assert.throws(() => verifyInvocation(value, names))
  }
  assert.throws(() => verifyInvocation(value.replace('0 ignored', '1 ignored'), ['one', 'two']))
})

test('every default profile stays identical and every invocation supplies a new nonempty profile', () => {
  const before = [profile('default-1.profraw')]
  const prefixes = ['db-shard-0-target-0-', 'db-shard-1-target-0-']
  const first = profile(`${prefixes[0]}123.profraw`, 'b')
  const second = profile(`${prefixes[1]}124.profraw`, 'c')
  const after = [...before, first, second]
  assert.deepEqual(verifyProfileInputs(before, after, prefixes), { retainedDefaultProfiles: 1, newProfiles: 2 })
  for (const invalid of [[first, second], [profile(before[0].name, 'd'), first, second],
    [...before, first], [...after, first], [...before, first, { ...second, bytes: 0 }],
    [...before, first, { ...second, sha256: 'missing' }], [...after, profile('unaccounted.profraw')],
    [...after, profile('../outside.profraw')]]) {
    assert.throws(() => verifyProfileInputs(before, invalid, prefixes))
  }
  assert.throws(() => verifyProfileInputs([], [first, second], prefixes))
  assert.throws(() => verifyProfileInputs(before, after, [prefixes[0], prefixes[0]]))
  assert.throws(() => verifyProfileInputs(after, after, prefixes))
})

test('database/Redis fixtures must be independent disposable loopback services', () => {
  assert.deepEqual(fixtureEndpoints(services()), services())
  for (const [kind, value] of [
    ['postgres', 'postgresql://postgres:postgres@127.0.0.1:15432/production'],
    ['postgres', 'postgresql://postgres:secret@127.0.0.1:15432/rsctf_test'],
    ['postgres', 'postgresql://postgres:postgres@database.example:15432/rsctf_test'],
    ['postgres', 'postgresql://postgres:postgres@127.0.0.1/rsctf_test'],
    ['postgres', 'postgresql://postgres:postgres@127.0.0.1:15432/rsctf_test?options=other'],
    ['redis', 'redis://127.0.0.1:16379/1'], ['redis', 'rediss://127.0.0.1:16379'],
    ['redis', 'redis://:secret@127.0.0.1:16379'], ['redis', 'not a URL'],
  ]) {
    const pairs = services()
    pairs[0][kind] = value
    assert.throws(() => fixtureEndpoints(pairs), (error) => !error.message.includes('secret'))
  }
  for (const kind of ['postgres', 'redis']) {
    const pairs = services()
    pairs[1][kind] = pairs[0][kind]
    assert.throws(() => fixtureEndpoints(pairs))
  }
})

test('coverage keeps the established environment-only exclusions and complete instrumented target selection', () => {
  const workflow = readFileSync(new URL('../../../.github/workflows/ci.yml', import.meta.url), 'utf8')
  const job = workflow.split('  coverage:\n')[1].split('\n  byoc-agent:\n')[0]
  assert.deepEqual(databaseExclusions, [
    's3_round_trip',
    'stale_conditional_delete_cannot_destroy_a_real_replacement_runtime',
    'real_docker_large_file_and_fifo_are_bounded_without_exec',
    'live_daemon_response_is_streamed_and_decoded',
    'target_fk_deletes_scoped_tokens',
    'ownership_constraints_are_validated_cascades',
    'postgres_stamps_follow_commit_order_without_cross_source_deadlock',
    'postgres_sealed_source_six_and_nine_writes_do_not_redirty_the_game',
    'postgres_two_operations_share_active_and_completed_generation',
    'postgres_coalesces_retries_and_recovers_one_expired_lease',
  ])
  assert.match(job, /cargo llvm-cov --all-targets --all-features --locked --no-report/)
  assert.match(job, /export CARGO_TARGET_DIR="\$GITHUB_WORKSPACE\/target\/llvm-cov-target"/)
  assert.match(job, /cargo llvm-cov show-env --export-prefix/)
  assert.match(job, /source "\$RUNNER_TEMP\/coverage.env"[\s\S]*cargo test --all-targets --all-features --locked --no-run --message-format=json/)
  assert.match(job, /node scripts\/coverage\/run.mjs[\s\S]*cargo llvm-cov report --summary-only/)
  assert.doesNotMatch(job, /cargo llvm-cov report[^\n]*(?:--all-features|--locked)/)
  assert.doesNotMatch(job, /continue-on-error|--ignore-run-fail|cargo llvm-cov clean/)
  assert.match(job, /--ignore-filename-regex '\(\^\|\/\)\(target\|migrations\)\/'/)
  assert.match(job, /--fail-under-lines 40/)
  assert.match(job, /shared-key: coverage/)
})

test('CI supplies separate physical PostgreSQL and Redis services for both partitions', () => {
  const workflow = readFileSync(new URL('../../../.github/workflows/ci.yml', import.meta.url), 'utf8')
  const job = workflow.split('  coverage:\n')[1].split('\n  byoc-agent:\n')[0]
  const pairs = fixtureEndpoints(JSON.parse(readFileSync(new URL('../../../scripts/coverage/services.json', import.meta.url))))
  assert.equal([...job.matchAll(/image: postgres:18-alpine/g)].length, 2)
  assert.equal([...job.matchAll(/image: redis:8-alpine/g)].length, 2)
  for (const pair of pairs) for (const [kind, address] of Object.entries(pair)) {
    const port = new URL(address).port
    assert.match(job, new RegExp(`- ${port}:${kind === 'postgres' ? 5432 : 6379}`))
  }
})
