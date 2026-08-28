import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const component = readFileSync('src/components/HashPow.tsx', 'utf8')
const worker = readFileSync('src/utils/PowWorker.ts', 'utf8')

test('HashPoW owns one abortable request and worker generation without browser persistence', () => {
  assert.doesNotMatch(component, /useLocalStorage|localStorage|sessionStorage/)
  assert.match(component, /const controller = new AbortController\(\)/)
  assert.match(component, /signal: controller\.signal/)
  assert.match(component, /generationRef\.current/)
  assert.match(component, /requestRef\.current\?\.abort\(\)/)
  assert.match(component, /workerRef\.current\?\.terminate\(\)/)
})

test('HashPoW expires proofs before submission and fetch failures expose explicit Retry', () => {
  assert.match(component, /challenge\.expiresAt > Date\.now\(\) \+ EXPIRY_SAFETY_MS/)
  assert.match(component, /challenge\.expiresAt - Date\.now\(\) - EXPIRY_SAFETY_MS/)
  assert.match(component, /status === 'error'/)
  assert.match(component, /common\.button\.retry/)
  assert.doesNotMatch(component, /setInterval\([^]*infoPowChallenge/)
})

test('the worker wraps a 32-bit nonce and terminates an exhausted search', () => {
  assert.match(worker, /trials < 0x1_0000_0000/)
  assert.match(worker, /nonce = \(nonce \+ 1\) >>> 0/)
  assert.match(worker, /return \{ nonce: null,/)
})
