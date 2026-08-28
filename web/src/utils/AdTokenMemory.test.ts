import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { adTokenRequestOwnerKey, clearLegacyAdTokenStorage, visibleAdToken } from './AdTokenMemory'

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

test('plaintext bearer rendering is fenced synchronously by account and participation', () => {
  const token = {
    accountId: 'account-a',
    token: 'ad_plaintext_secret',
    viewerScope: '41:7',
  }

  assert.equal(visibleAdToken(token, 'account-a', '41:7'), token.token)
  assert.equal(visibleAdToken(token, 'account-b', '41:7'), null, 'same-team account switch must hide before effects run')
  assert.equal(visibleAdToken(token, 'account-a', '42:7'), null)
  assert.equal(visibleAdToken(token, 'account-a', null), null)
})

test('rotation single-flight owners cannot cross authenticated accounts', () => {
  const accountA = adTokenRequestOwnerKey(17, 'account-a', '41:7')
  assert.notEqual(accountA, adTokenRequestOwnerKey(17, 'account-b', '41:7'))
  assert.notEqual(accountA, adTokenRequestOwnerKey(17, 'account-a', '42:7'))
  assert.equal(accountA, adTokenRequestOwnerKey(17, 'account-a', '41:7'))

  const toolkit = readFileSync('src/components/AdToolkitSections.tsx', 'utf8')
  assert.match(toolkit, /adTokenOperationStorageKey\(gameId, requestOwner\)/)
  assert.match(toolkit, /claimAdTokenOperation\(key, revision\)/)
})
