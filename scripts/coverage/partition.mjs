import assert from 'node:assert/strict'
import { isAbsolute, relative, resolve } from 'node:path'
function listedTests(text) {
  const lines = text.split(/\r?\n/).filter(Boolean)
  const summary = lines.pop()?.match(/^(\d+) tests?, (\d+) benchmarks?$/)
  assert.ok(summary, 'missing libtest discovery summary')
  assert.equal(Number(summary[2]), 0, 'unexpected benchmark in database selection')
  const names = lines.map((line) => {
    assert.ok(line.endsWith(': test'), 'unexpected test discovery output')
    return line.slice(0, -6)
  })
  assert.equal(names.length, Number(summary[1]))
  assert.ok(names.length > 0)
  assert.equal(new Set(names).size, names.length, 'duplicate discovered tests')
  return names.sort()
}


function verifyResults(text, expected) {
  const actual = [...text.matchAll(/^test (.+) \.\.\. ok$/gm)].map((match) => match[1]).sort()
  assert.deepEqual(actual, [...expected].sort(), 'completed tests differ from the exact partition')
  const summaries = [...text.matchAll(/^test result: ok\. (\d+) passed; 0 failed; 0 ignored;/gm)]
  assert.equal(summaries.length, 1)
  assert.equal(Number(summaries[0][1]), expected.length)
  return summaries[0][0]
}


// Exactly the current coverage job's environment-only exclusions, not a new
// classification of which database or authorization regressions are important.
export const databaseExclusions = Object.freeze([
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

export function inside(path, directory) {
  if (!isAbsolute(path) || !isAbsolute(directory)) return false
  const child = relative(resolve(directory), resolve(path))
  return child !== '' && child !== '..' && !child.startsWith('../') && !isAbsolute(child)
}

export function testArtifacts(text, targetDirectory) {
  assert.ok(isAbsolute(targetDirectory), 'coverage target must be absolute')
  const messages = text.split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line))
  const finished = messages.filter((item) => item.reason === 'build-finished')
  assert.equal(finished.length, 1, 'expected one completed Cargo build')
  assert.equal(messages.at(-1), finished[0], 'truncated or appended Cargo build stream')
  assert.equal(finished[0].success, true, 'Cargo build did not succeed')
  const artifacts = []
  for (const item of messages) {
    if (item.reason === 'compiler-message') {
      assert.ok(!['error', 'warning'].includes(item.message?.level), 'build diagnostics are not clean')
    }
    if (item.reason !== 'compiler-artifact' || !item.profile?.test || !item.executable) continue
    assert.ok(typeof item.package_id === 'string' && item.package_id.length > 0)
    assert.ok(typeof item.target?.name === 'string' && item.target.name.length > 0)
    assert.ok(Array.isArray(item.target.kind) && item.target.kind.length > 0)
    assert.ok(item.target.kind.every((kind) => ['lib', 'bin', 'test', 'example', 'bench'].includes(kind)),
      'unsupported executable target kind')
    assert.ok(inside(item.executable, targetDirectory), 'executable is outside the selected coverage target')
    artifacts.push({ id: JSON.stringify([item.package_id, item.target.kind, item.target.name]),
      executable: item.executable })
  }
  assert.ok(artifacts.length > 0, 'Cargo output contains no test executables')
  assert.equal(new Set(artifacts.map((item) => item.id)).size, artifacts.length, 'duplicate target identity')
  assert.equal(new Set(artifacts.map((item) => item.executable)).size, artifacts.length, 'duplicate executable')
  return artifacts.sort((a, b) => a.id < b.id ? -1 : a.id > b.id ? 1 : 0)
}

export function ignoredTests(text) {
  // Zero-case targets still have to be discovered and recorded. An entirely
  // empty suite is rejected by the complete-plan check below, not silently run.
  if (text.trim() === '0 tests, 0 benchmarks') return []
  const names = listedTests(text)
  for (const name of names) {
    assert.ok(name.length > 0 && !name.startsWith('-') && !/[\r\n\0]/.test(name), 'unsafe test filter')
  }
  return names
}

export function partitionTargets(discovered, count = 2) {
  assert.ok(count === 1 || count === 2, 'only measured partition counts are supported')
  assert.ok(discovered.length > 0, 'no discovered test targets')
  assert.equal(new Set(discovered.map(({ id }) => id)).size, discovered.length, 'duplicate discovery target')
  const all = discovered.flatMap((artifact) => {
    assert.ok(typeof artifact.id === 'string' && artifact.id.length > 0)
    assert.equal(new Set(artifact.names).size, artifact.names.length, 'duplicate discovered test')
    return artifact.names.map((name) => ({ artifact, name, key: JSON.stringify([artifact.id, name]) }))
  }).sort((a, b) => a.key < b.key ? -1 : a.key > b.key ? 1 : 0)
  assert.ok(all.length >= count, 'empty database coverage partition')
  const groups = Array.from({ length: count }, () => new Map())
  for (const [index, item] of all.entries()) {
    const group = groups[index % count]
    if (!group.has(item.artifact.id)) group.set(item.artifact.id, { artifact: item.artifact, names: [] })
    group.get(item.artifact.id).names.push(item.name)
  }
  const partitions = groups.map((group) => [...group.values()])
  const union = partitions.flatMap((group) => group.flatMap(({ artifact, names }) =>
    names.map((name) => JSON.stringify([artifact.id, name])))).sort()
  assert.deepEqual(union, all.map(({ key }) => key))
  return partitions
}

export function verifyInvocation(text, names) {
  assert.ok(names.length > 0, 'refuse an empty exact filter, which would run all tests')
  assert.equal(new Set(names).size, names.length, 'duplicate expected result')
  return verifyResults(text, names)
}

export function verifyProfileInputs(before, after, invocations) {
  assert.ok(before.length > 0, 'default coverage profiles are missing')
  assert.ok(invocations.length > 0, 'no database coverage invocations')
  for (const files of [before, after]) {
    assert.equal(new Set(files.map(({ name }) => name)).size, files.length, 'duplicate profile path')
    for (const item of files) {
      assert.ok(/^[^/\\]+\.profraw$/.test(item.name) && item.name !== '..', 'invalid profile filename')
      assert.ok(Number.isSafeInteger(item.bytes) && item.bytes > 0, 'empty raw profile')
      assert.ok(/^[a-f0-9]{64}$/.test(item.sha256), 'raw profile has no content identity')
    }
  }
  const found = new Map(after.map((item) => [item.name, item]))
  for (const item of before) assert.deepEqual(found.get(item.name), item, 'default coverage changed or disappeared')
  assert.equal(new Set(invocations).size, invocations.length, 'duplicate profile prefix')
  for (const prefix of invocations) {
    assert.ok(/^db-shard-[01]-target-\d+-$/.test(prefix), 'invalid invocation profile prefix')
    assert.ok(!before.some(({ name }) => name.startsWith(prefix)), 'stale database profile already exists')
    assert.ok(after.some(({ name }) => name.startsWith(prefix)), 'one invocation has no coverage profile')
  }
  const oldNames = new Set(before.map(({ name }) => name))
  for (const item of after) {
    if (!oldNames.has(item.name)) {
      assert.equal(invocations.filter((prefix) => item.name.startsWith(prefix)).length, 1,
        'unaccounted raw profile in candidate report')
    }
  }
  return { retainedDefaultProfiles: before.length, newProfiles: after.length - before.length }
}

export function fixtureEndpoints(partitions) {
  assert.equal(partitions.length, 2, 'two independent service pairs are required')
  const endpoints = []
  for (const pair of partitions) {
    for (const kind of ['postgres', 'redis']) {
      let url
      try { url = new URL(pair[kind]) } catch { throw new Error('invalid disposable service URL') }
      assert.ok(url.hostname === '127.0.0.1' && /^\d+$/.test(url.port), 'service must use an explicit loopback port')
      assert.ok(Number(url.port) >= 1 && Number(url.port) <= 65535, 'invalid disposable service port')
      assert.ok(!url.hash && !url.search, 'service URL cannot override connection settings')
      if (kind === 'postgres') {
        assert.ok(['postgres:', 'postgresql:'].includes(url.protocol), 'invalid PostgreSQL protocol')
        assert.ok(url.username === 'postgres' && url.password === 'postgres', 'expected disposable test credentials')
        assert.ok(url.pathname === '/rsctf_test', 'refuse a non-test PostgreSQL database')
      } else {
        assert.ok(url.protocol === 'redis:' && !url.username && !url.password, 'expected disposable Redis service')
        assert.ok(['', '/', '/0'].includes(url.pathname), 'separate Redis databases do not isolate Pub/Sub clients')
      }
      endpoints.push(`${url.hostname}:${url.port}`)
    }
  }
  assert.equal(new Set(endpoints).size, 4, 'partitions must use physically separate services')
  return partitions
}
