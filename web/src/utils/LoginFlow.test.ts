import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const login = readFileSync('src/pages/account/Login.tsx', 'utf8')

test('password login seeds the authenticated profile before navigating', () => {
  const loginRequest = login.indexOf('await api.account.accountLogIn(')
  const profileRequest = login.indexOf('const profile = await api.account.accountProfile()')
  const cachePublish = login.indexOf('await mutate(profile.data, { revalidate: false })')
  const navigation = login.indexOf("navigate(params.get('from') ?? '/', { replace: true })")

  assert.ok(loginRequest >= 0)
  assert.ok(profileRequest > loginRequest)
  assert.ok(cachePublish > profileRequest)
  assert.ok(navigation > cachePublish)
  assert.doesNotMatch(login, /setNeedRedirect|if \(needRedirect && user\)/)
})
