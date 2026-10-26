import assert from 'node:assert/strict'
import test from 'node:test'
import { commitAuthSession, setAuthSession } from './AuthState'
import { readProfile } from './ProfileRead'

test('an old anonymous response cannot overwrite a profile committed during that read', async () => {
  const cache = new Map()
  let reject!: (error: unknown) => void
  const pending = new Promise<never>((_, fail) => {
    reject = fail
  })
  const read = readProfile(cache, () => pending)
  const profile = { userId: 'new-account' }
  cache.set('/api/account/profile', { data: profile, error: undefined })
  commitAuthSession()
  reject({ status: 401 })
  assert.equal(await read, profile)
  setAuthSession(false)
})

test('current-account rejection, eviction, and service failure remain errors', async () => {
  for (const variant of ['same-account', 'profile-edit', 'cleared', 'terminal-error', 'service-error']) {
    const profile = { userId: 'original-account' }
    const cache = new Map<string, any>([['/api/account/profile', { data: profile }]])
    let reject!: (error: unknown) => void
    const pending = new Promise<never>((_, fail) => {
      reject = fail
    })
    const read = readProfile(cache, () => pending)
    const error = { status: variant === 'service-error' ? 503 : 401 }
    if (variant !== 'profile-edit') commitAuthSession()
    if (variant === 'profile-edit')
      cache.set('/api/account/profile', { data: { userId: 'original-account', userName: 'Updated profile' } })
    if (variant === 'cleared') cache.delete('/api/account/profile')
    if (variant === 'terminal-error')
      cache.set('/api/account/profile', { data: { userId: 'other-account' }, error: { status: 401 } })
    if (variant === 'service-error') cache.set('/api/account/profile', { data: { userId: 'other-account' } })
    reject(error)
    await assert.rejects(read, (actual) => actual === error, variant)
    setAuthSession(false)
  }
})
