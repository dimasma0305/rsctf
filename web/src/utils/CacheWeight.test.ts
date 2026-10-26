import assert from 'node:assert/strict'
import test from 'node:test'
import { estimateCacheBytes } from './CacheWeight'

test('cache weights include Unicode strings and retained backing buffers', () => {
  const text = '漢字🙂'.repeat(1000)
  assert.ok(estimateCacheBytes({ text }, 1_000_000) >= text.length * 2)
  const buffer = new ArrayBuffer(10_000)
  assert.ok(estimateCacheBytes(new Uint8Array(buffer, 0, 1), 1_000_000) >= 10_000)
})

test('cache weighing is bounded for cycles, shared objects, and oversized graphs', () => {
  const value: { self?: unknown; text: string } = { text: 'small' }
  value.self = value
  assert.ok(estimateCacheBytes(value, 1024) < 1024)
  assert.ok(estimateCacheBytes({ a: value, b: value }, 1024) < 1024)
  assert.ok(estimateCacheBytes(new Array(262_145), 1024) > 1024)
  assert.ok(estimateCacheBytes({ text: 'x'.repeat(10_000) }, 1024) > 1024)
})
