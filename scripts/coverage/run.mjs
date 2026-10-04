import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { existsSync, mkdirSync, readFileSync, readdirSync, realpathSync, statSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { pathToFileURL } from 'node:url'
import { CoverageProcesses } from './processes.mjs'
import { databaseExclusions, fixtureEndpoints, ignoredTests, inside, partitionTargets,
  testArtifacts, verifyInvocation, verifyProfileInputs } from './partition.mjs'

const hash = (path) => createHash('sha256').update(readFileSync(path)).digest('hex')
const inventory = (directory) => readdirSync(directory).filter((name) => name.endsWith('.profraw')).sort()
  .map((name) => ({ name, bytes: statSync(`${directory}/${name}`).size, sha256: hash(`${directory}/${name}`) }))

export async function runCoveragePartitions({ artifactFile, targetDirectory, services, evidenceDirectory,
  env = process.env, commandFor = (executable, args) => [executable, args], signal }) {
  const pairs = fixtureEndpoints(services)
  const target = realpathSync(targetDirectory)
  assert.equal(env.CARGO_LLVM_COV_TARGET_DIR, target, 'must retain the instrumented build environment')
  assert.equal(env.CARGO_LLVM_COV, '1', 'coverage instrumentation environment is absent')
  assert.ok(!existsSync(evidenceDirectory), 'refuse to overwrite coverage evidence')
  mkdirSync(evidenceDirectory)
  const evidence = realpathSync(evidenceDirectory)
  assert.ok(!inside(evidence, target), 'discovery profiles must stay outside the report target')
  const runner = new CoverageProcesses()
  const record = { status: 'starting', startedAt: new Date().toISOString(), artifactSha256: hash(artifactFile),
    target, exclusions: databaseExclusions, targets: [], invocations: [] }
  const save = () => writeFileSync(`${evidence}/result.json`, `${JSON.stringify(record, null, 2)}\n`)
  const interrupt = () => { void runner.cancel(new Error('coverage partitions interrupted')) }
  signal?.addEventListener('abort', interrupt, { once: true })
  if (signal?.aborted) interrupt()
  save()
  try {
    const artifacts = testArtifacts(readFileSync(artifactFile, 'utf8'), target)
    record.defaultProfiles = inventory(target)
    assert.ok(record.defaultProfiles.length > 0, 'default suite did not produce raw coverage')
    assert.ok(record.defaultProfiles.every(({ name }) => !name.startsWith('db-shard-')),
      'stale partition profiles must not be counted as default coverage')
    for (const [index, artifact] of artifacts.entries()) {
      assert.ok(inside(realpathSync(artifact.executable), target), 'test executable escapes the coverage target')
      assert.ok(statSync(artifact.executable).isFile(), 'test executable is not a regular file')
      const names = ignoredTests(await runner.run(artifact.executable,
        ['--ignored', '--list', ...databaseExclusions.flatMap((name) => ['--skip', name])],
        { label: `discover-${index}`, logPath: `${evidence}/discover-${index}.log`,
          env: { ...env, LLVM_PROFILE_FILE: `${evidence}/discovery-${index}-%p-%m.profraw` }, timeoutMs: 30000 }))
      record.targets.push({ ...artifact, index, sha256: hash(artifact.executable), names })
    }
    const groups = partitionTargets(record.targets)
    record.selectedTests = record.targets.reduce((sum, item) => sum + item.names.length, 0)
    record.groups = groups.map((group) => group.map(({ artifact, names }) => ({ id: artifact.id, names })))
    record.status = 'testing'
    save()
    const outcomes = await Promise.allSettled(groups.map(async (group, shard) => {
      try {
        for (const { artifact, names } of group) {
          const label = `db-shard-${shard}-target-${artifact.index}`
          const args = ['--ignored', '--test-threads=1', '--exact', ...names]
          const [binary, commandArgs] = commandFor(artifact.executable, args, { shard, target: artifact.index })
          const stdout = await runner.run(binary, commandArgs, { label, logPath: `${evidence}/${label}.log`,
            env: { ...env, RSCTF_TEST_DATABASE_URL: pairs[shard].postgres,
              RSCTF_TEST_REDIS_URL: pairs[shard].redis, RUST_TEST_THREADS: '1',
              LLVM_PROFILE_FILE: `${target}/${label}-%p-%m.profraw` } })
          verifyInvocation(stdout, names)
          record.invocations.push({ label, shard, target: artifact.index, names })
        }
      } catch (error) {
        await runner.cancel(error)
        throw error
      }
    }))
    // All processes and their output have settled before profile validation or
    // returning control to the caller that owns service cleanup/reporting.
    const failed = outcomes.filter((result) => result.status === 'rejected')
    assert.equal(failed.length, 0, failed.map((result) => result.reason.message).join('; '))
    record.profiles = inventory(target)
    record.profileVerification = verifyProfileInputs(record.defaultProfiles, record.profiles,
      record.invocations.map(({ label }) => `${label}-`))
    for (const artifact of record.targets) assert.equal(hash(artifact.executable), artifact.sha256,
      'test executable changed during coverage execution')
    const completed = record.invocations.flatMap(({ target: index, names }) =>
      names.map((name) => JSON.stringify([record.targets[index].id, name]))).sort()
    const selected = record.targets.flatMap(({ id, names }) => names.map((name) => JSON.stringify([id, name]))).sort()
    assert.deepEqual(completed, selected, 'the complete all-target test union was not executed')
    record.status = 'passed'
  } catch (error) {
    await runner.cancel(error)
    record.status = 'failed'
    record.error = runner.error.message
  } finally {
    signal?.removeEventListener('abort', interrupt)
    record.processes = runner.results
    record.completedAt = new Date().toISOString()
    save()
  }
  return record
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const [artifactFile, targetDirectory, serviceFile, evidenceDirectory, localBounds] = process.argv.slice(2)
  assert.ok(artifactFile && targetDirectory && serviceFile && evidenceDirectory,
    'usage: node scripts/coverage/run.mjs ARTIFACTS TARGET SERVICES_JSON NEW_EVIDENCE_DIRECTORY')
  const controller = new AbortController()
  const interrupt = () => controller.abort()
  process.on('SIGINT', interrupt)
  process.on('SIGTERM', interrupt)
  try {
    assert.ok(!localBounds || localBounds === '--bounded-local')
    const commandFor = localBounds ? (executable, args) => ['systemd-run', [
      '--quiet', '--scope', '--collect', '--property', 'CPUQuota=100%', '--property', 'CPUWeight=20',
      '--property', 'MemoryMax=2G', '--property', 'TasksMax=512', '--property', 'OOMPolicy=stop', executable, ...args,
    ]] : undefined
    const result = await runCoveragePartitions({ artifactFile, targetDirectory, evidenceDirectory,
      services: JSON.parse(readFileSync(serviceFile, 'utf8')), signal: controller.signal, commandFor })
    console.log(JSON.stringify({ status: result.status, selectedTests: result.selectedTests,
      profileVerification: result.profileVerification, error: result.error }))
    process.exitCode = result.status === 'passed' ? 0 : 1
  } finally {
    process.off('SIGINT', interrupt)
    process.off('SIGTERM', interrupt)
  }
}
