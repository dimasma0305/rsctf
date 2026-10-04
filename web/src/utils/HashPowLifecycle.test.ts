import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const source = readFileSync('src/components/HashPow.tsx', 'utf8')
const api = readFileSync('src/Api.ts', 'utf8')

test('HashPoW is tab-scoped and owns one abortable fetch and worker generation', () => {
  assert.doesNotMatch(source, /useLocalStorage|localStorage|pow-chall/)
  assert.match(source, /fetchRef = useRef<AbortController/)
  assert.match(source, /generationRef = useRef\(0\)/)
  assert.match(source, /workerRef\.current\?\.terminate\(\)/)
  assert.match(source, /controller\.signal/)
})

test('HashPoW expires proactively and persistent issuance failure needs explicit retry', () => {
  const challengeLifecycle = source.slice(
    source.indexOf('export const usePowChallenge'),
    source.indexOf('interface PowBoxProps')
  )
  assert.match(api, /expiresAt\?: number/)
  assert.match(source, /chall\.expiresAt - Date\.now\(\) - 5_000/)
  assert.match(source, /chall\.expiresAt > Date\.now\(\)/)
  assert.match(source, /Retry challenge/)
  assert.doesNotMatch(challengeLifecycle, /setInterval/)
})
