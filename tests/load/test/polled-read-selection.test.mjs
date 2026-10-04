import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { polledReadSelection } from '../polled-read-model.js'

test('every polling endpoint uses the whole cohort even when sizes share factors', () => {
  for (const endpoints of [5, 7, 8]) for (const users of [100, 200, 400, 500]) {
    const pairs = new Set()
    const populations = Array.from({ length: endpoints }, () => new Set())
    for (let sequence = 0; sequence < endpoints * users; sequence++) {
      const { endpointIndex, tokenIndex } = polledReadSelection(sequence, endpoints, users)
      pairs.add(`${endpointIndex}:${tokenIndex}`)
      populations[endpointIndex].add(tokenIndex)
    }
    assert.equal(pairs.size, endpoints * users)
    assert.ok(populations.every((population) => population.size === users))
    assert.deepEqual(polledReadSelection(endpoints * users, endpoints, users), { endpointIndex: 0, tokenIndex: 0 })
  }
})

test('held-rate endpoints are balanced independently of VU scheduling and iteration order', () => {
  const count = 27000
  const sequential = Array.from({ length: count }, (_, sequence) => polledReadSelection(sequence, 7, 400))
  const reordered = new Map([...Array(count).keys()].reverse().map((sequence) => [sequence, polledReadSelection(sequence, 7, 400)]))
  for (let sequence = 0; sequence < count; sequence++) assert.deepEqual(reordered.get(sequence), sequential[sequence])
  const hits = Array(7).fill(0)
  for (const { endpointIndex } of sequential) hits[endpointIndex]++
  assert.ok(Math.max(...hits) - Math.min(...hits) <= 1)
})

test('invalid selection inputs fail before choosing a token', () => {
  for (const args of [[-1, 7, 400], [NaN, 7, 400], [0.1, 7, 400], [0, 0, 400],
    [0, 7, 0], [0, 7, 1.5], [Number.MAX_SAFE_INTEGER + 1, 7, 400]]) {
    assert.throws(() => polledReadSelection(...args))
  }
})

test('the actual k6 scenario uses its global scenario iteration and retains rate-limit/error gates', () => {
  const source = readFileSync(new URL('../k6/polled-read.js', import.meta.url), 'utf8')
  assert.match(source, /polledReadSelection\(exec.scenario.iterationInTest, endpoints.length, TOKENS.length\)/)
  assert.doesNotMatch(source, /__VU|__ITER/)
  assert.match(source, /executor: 'constant-arrival-rate'/)
  assert.match(source, /rate_limited: \['rate==0'\]/)
  assert.match(source, /dropped_iterations: \['count==0'\]/)
})
