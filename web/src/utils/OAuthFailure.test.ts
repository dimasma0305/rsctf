import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { oauthFailure } from './OAuthFailure'

const read = (language: string) => JSON.parse(readFileSync(`src/locales/${language}/account.json`, 'utf8'))

test('every server OAuth failure has specific English and Indonesian guidance', () => {
  const backend = readFileSync('../src/controllers/oauth_response.rs', 'utf8')
  const codes = [...new Set(backend.match(/oauth_[a-z_]+/g))].filter((code) => code !== 'oauth_state_cookie')
  const english = read('en-US').oauth.error
  const indonesian = read('id-ID').oauth.error
  for (const code of codes) {
    const failure = oauthFailure(code)
    assert.ok(failure, code)
    const key = failure.messageKey.split('.').at(-1)!
    assert.ok(typeof english[key] === 'string' && english[key].length > 20, code)
    assert.ok(typeof indonesian[key] === 'string' && indonesian[key].length > 20, code)
    assert.notEqual(english[key], indonesian[key], code)
    if (code !== 'oauth_failed') assert.notEqual(key, 'generic', code)
  }
})

test('only allowlisted OAuth codes select messages and pending approval is distinguished', () => {
  assert.equal(oauthFailure(null), null)
  assert.equal(oauthFailure('<script>'), null)
  assert.equal(oauthFailure('oauth_unknown_payload')?.messageKey, 'account.oauth.error.generic')
  assert.equal(oauthFailure('oauth_state_expired')?.messageKey, 'account.oauth.error.state_expired')
  assert.equal(oauthFailure('oauth_await_approval')?.pending, true)
  assert.equal(oauthFailure('oauth_email_unverified')?.pending, false)
})
