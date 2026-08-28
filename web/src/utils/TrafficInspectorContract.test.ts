import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const inspector = readFileSync('src/components/traffic/FlowInspector.tsx', 'utf8')
const detail = readFileSync('src/components/traffic/FlowDetail.tsx', 'utf8')
const api = readFileSync('src/Api.ts', 'utf8')

test('traffic inspector aborts superseded work and preserves its last good result', () => {
  assert.match(inspector, /new AbortController\(\)/)
  assert.match(inspector, /signal: abort\.signal/)
  assert.match(inspector, /setLoadError\(true\)/)
  assert.doesNotMatch(inspector, /\.catch\(\(\) => \{\s*if \([^)]*\) setFlows\(\[\]\)/s)
  assert.match(detail, /new AbortController\(\)/)
})

test('traffic wire contract uses real numeric timestamps, filters, and pagination', () => {
  assert.match(api, /firstSeenUtc: number/)
  assert.match(api, /timestampUtc: number/)
  assert.match(api, /regexPattern\?: string/)
  assert.match(api, /pageSize\?: number/)
  assert.match(api, /query: filter/)
})
