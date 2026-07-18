import assert from 'node:assert/strict'
import test from 'node:test'
import type { WorkloadSpec } from '@Api'
import { createDefaultJeopardyWorkloadSpec, formatWorkloadSpec, parseJeopardyWorkloadSpec } from './WorkloadSpec'

const parseValid = (spec: WorkloadSpec) => {
  const result = parseJeopardyWorkloadSpec(formatWorkloadSpec(spec))
  assert.equal(result.ok, true, result.ok ? undefined : result.error)
  return result.value
}

test('the Linux example uses the camelCase trusted-worker wire format', () => {
  const parsed = parseValid(createDefaultJeopardyWorkloadSpec())

  assert.equal(parsed.gameKind, 'jeopardy')
  assert.deepEqual(parsed.platform, { operatingSystem: 'linux', architecture: 'amd64' })
  assert.equal(parsed.services.length, 1)
  assert.equal(parsed.services[0].stateless, true)
  assert.equal(parsed.services[0].image.type, 'registryDigest')
  assert.equal(parsed.services[0].resources.cpuMillis, 500)
  assert.equal(parsed.services[0].resources.memoryBytes, 134_217_728)
  assert.deepEqual(parsed.primaryEndpoint, { service: 'challenge', port: 'service' })
  assert.deepEqual(parsed.flagTarget, { service: 'challenge', path: '/flag' })
  assert.equal(formatWorkloadSpec(parsed).includes('game_kind'), false)
})

test('malformed JSON is rejected locally', () => {
  const result = parseJeopardyWorkloadSpec('{"gameKind":')

  assert.equal(result.ok, false)
  if (!result.ok) assert.match(result.error, /JSON|position|end/i)
})

test('the Jeopardy editor rejects competitive game kinds', () => {
  const spec = createDefaultJeopardyWorkloadSpec()
  spec.gameKind = 'attackDefense'

  const result = parseJeopardyWorkloadSpec(formatWorkloadSpec(spec))
  assert.deepEqual(result, { ok: false, error: 'gameKind must be "jeopardy"' })
})

test('multiple replicas require an explicitly stateless service', () => {
  const spec = createDefaultJeopardyWorkloadSpec()
  spec.services[0].replicas = 2
  spec.services[0].stateless = false

  let result = parseJeopardyWorkloadSpec(formatWorkloadSpec(spec))
  assert.equal(result.ok, false)
  if (!result.ok) assert.match(result.error, /stateless must be true/)

  spec.services[0].stateless = true
  result = parseJeopardyWorkloadSpec(formatWorkloadSpec(spec))
  assert.equal(result.ok, true, result.ok ? undefined : result.error)
})

test('the primary endpoint must reference a declared named port', () => {
  const spec = createDefaultJeopardyWorkloadSpec()
  spec.primaryEndpoint.port = 'missing'

  const result = parseJeopardyWorkloadSpec(formatWorkloadSpec(spec))
  assert.equal(result.ok, false)
  if (!result.ok) assert.match(result.error, /primaryEndpoint/)
})

test('a container workload requires an explicit flag target', () => {
  const spec = createDefaultJeopardyWorkloadSpec()
  delete (spec as Partial<WorkloadSpec>).flagTarget

  const result = parseJeopardyWorkloadSpec(formatWorkloadSpec(spec))
  assert.equal(result.ok, false)
  if (!result.ok) assert.match(result.error, /flagTarget is required/)
})

test('registry repositories use the same canonical grammar as the worker protocol', () => {
  const accepted = [
    'library/redis',
    'registry.example/ctf/web_app',
    'localhost:5000/team/service',
    '[2001:db8::1]:5000/team/service',
  ]
  for (const repository of accepted) {
    const spec = createDefaultJeopardyWorkloadSpec()
    if (spec.services[0].image.type !== 'registryDigest') assert.fail('default image type changed')
    spec.services[0].image.repository = repository
    assert.equal(parseJeopardyWorkloadSpec(formatWorkloadSpec(spec)).ok, true, repository)
  }

  const rejected = [
    'https://registry.example/team/service',
    'registry.example/Team/service',
    'registry.example/team:latest',
    'registry.example/team@sha256:deadbeef',
    'registry.example/team/../service',
    'registry.example:70000/team/service',
    'registry.example/téam/service',
  ]
  for (const repository of rejected) {
    const spec = createDefaultJeopardyWorkloadSpec()
    if (spec.services[0].image.type !== 'registryDigest') assert.fail('default image type changed')
    spec.services[0].image.repository = repository
    const result = parseJeopardyWorkloadSpec(formatWorkloadSpec(spec))
    assert.equal(result.ok, false, `${repository} was accepted`)
    if (!result.ok) assert.match(result.error, /canonical registry repository/)
  }
})

test('worker-local images accept a canonical UUID and immutable image ID', () => {
  const spec = createDefaultJeopardyWorkloadSpec()
  spec.services[0].image = {
    type: 'workerLocal',
    workerId: '00000000-0000-0000-0000-000000000000',
    imageId: `sha256:${'a'.repeat(64)}`,
  }

  const parsed = parseValid(spec)
  assert.equal(parsed.services[0].image.type, 'workerLocal')
})

test('unknown top-level workload keys are rejected', () => {
  const spec = createDefaultJeopardyWorkloadSpec() as WorkloadSpec & { gameKnd?: string }
  spec.gameKnd = 'jeopardy'

  const result = parseJeopardyWorkloadSpec(formatWorkloadSpec(spec))
  assert.deepEqual(result, { ok: false, error: 'workload.gameKnd is not supported' })
})

test('unknown keys are rejected at every nested object boundary', () => {
  const mutations: Array<[string, (spec: WorkloadSpec) => Record<string, unknown>]> = [
    ['platform', (spec) => spec.platform as unknown as Record<string, unknown>],
    ['service', (spec) => spec.services[0] as unknown as Record<string, unknown>],
    ['registry image', (spec) => spec.services[0].image as unknown as Record<string, unknown>],
    ['resources', (spec) => spec.services[0].resources as unknown as Record<string, unknown>],
    ['port', (spec) => spec.services[0].ports[0] as unknown as Record<string, unknown>],
    ['primary endpoint', (spec) => spec.primaryEndpoint as unknown as Record<string, unknown>],
    ['flag target', (spec) => spec.flagTarget as unknown as Record<string, unknown>],
  ]

  for (const [location, select] of mutations) {
    const spec = createDefaultJeopardyWorkloadSpec()
    select(spec).cpuMilis = 500
    const result = parseJeopardyWorkloadSpec(formatWorkloadSpec(spec))
    assert.equal(result.ok, false, `${location} typo was accepted`)
    if (!result.ok) assert.match(result.error, /cpuMilis is not supported/)
  }

  const workerLocal = createDefaultJeopardyWorkloadSpec()
  workerLocal.services[0].image = {
    type: 'workerLocal',
    workerId: '00000000-0000-0000-0000-000000000000',
    imageId: `sha256:${'a'.repeat(64)}`,
  }
  ;(workerLocal.services[0].image as unknown as Record<string, unknown>).repository = 'unexpected'
  const workerLocalResult = parseJeopardyWorkloadSpec(formatWorkloadSpec(workerLocal))
  assert.equal(workerLocalResult.ok, false)
  if (!workerLocalResult.ok) assert.match(workerLocalResult.error, /repository is not supported/)
})

test('environment remains an arbitrary string map, not a fixed-shape object', () => {
  const spec = createDefaultJeopardyWorkloadSpec()
  spec.services[0].environment = { TEAM_SERVICE_URL: 'http://service', CACHE_PORT: '6379' }
  assert.equal(parseJeopardyWorkloadSpec(formatWorkloadSpec(spec)).ok, true)

  const invalid = JSON.parse(formatWorkloadSpec(spec)) as Record<string, unknown>
  ;((invalid.services as Array<Record<string, unknown>>)[0].environment as Record<string, unknown>).NESTED = {
    value: 'not-a-string',
  }
  const result = parseJeopardyWorkloadSpec(JSON.stringify(invalid))
  assert.equal(result.ok, false)
  if (!result.ok) assert.match(result.error, /NESTED must be a bounded string/)
})
