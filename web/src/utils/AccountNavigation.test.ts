import assert from 'node:assert/strict'
import test from 'node:test'
import { accountPath, accountReturnPath } from './AccountNavigation'

test('account detours preserve the entire same-origin destination', () => {
  const destination = '/games/19/challenges?division=2#7-cookie-jar&category=Web'
  for (const route of ['/account/login', '/account/register', '/account/recovery', '/account/pending']) {
    const link = new URL(accountPath(route, destination), 'https://rsctf.test')
    assert.equal(link.pathname, route)
    assert.equal(accountReturnPath(link.searchParams.get('from')), destination)
    assert.equal(accountPath(route, '/'), route)
  }
})

test('account return paths reject external URLs, browser slash normalization and controls', () => {
  for (const invalid of [
    null,
    undefined,
    '',
    'https://evil.test',
    '//evil.test',
    '/\\evil.test',
    'relative',
    '/a b',
    '/a\nnext',
    '/a\u0085next',
  ]) {
    assert.equal(accountReturnPath(invalid), '/', String(invalid))
    assert.equal(accountPath('/account/login', invalid), '/account/login')
  }
  assert.equal(accountReturnPath('/posts/hello%20world'), '/posts/hello%20world')
})
