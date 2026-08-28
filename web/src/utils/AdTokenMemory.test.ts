import assert from 'node:assert/strict'
import test from 'node:test'
import { clearLegacyAdTokenStorage } from './AdTokenMemory'

test('legacy A&D bearer cleanup removes every game token and preserves unrelated settings', () => {
  const values = new Map([
    ['ad-api-token-1', '"ad_account_a"'],
    ['ad-api-token-999', '"ad_account_b"'],
    ['guide-disabled', 'true'],
  ])
  const storage = {
    get length() {
      return values.size
    },
    key(index: number) {
      return Array.from(values.keys())[index] ?? null
    },
    removeItem(key: string) {
      values.delete(key)
    },
  }

  assert.equal(clearLegacyAdTokenStorage(storage), 2)
  assert.deepEqual(Array.from(values.entries()), [['guide-disabled', 'true']])
})

test('legacy bearer cleanup is failure isolated when browser storage is unavailable', () => {
  const storage = {
    get length(): number {
      throw new DOMException('storage denied', 'SecurityError')
    },
    key: () => null,
    removeItem: () => undefined,
  }
  assert.equal(clearLegacyAdTokenStorage(storage), 0)
})
