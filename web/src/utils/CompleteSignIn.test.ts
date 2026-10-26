import assert from 'node:assert/strict'
import test from 'node:test'
import type { ProfileUserInfoModel } from '@Api'
import { hasAuthSession, setAuthSession } from './AuthState'
import { completeSignIn } from './CompleteSignIn'

const profile: ProfileUserInfoModel = { userId: 'fixture-user', userName: 'Player' }

test('sign-in publishes a fresh profile before navigation even when that publication unmounts the form', async () => {
  setAuthSession(false)
  const controller = new AbortController()
  const events: string[] = []
  let release!: () => void
  const published = new Promise<void>((resolve) => {
    release = resolve
  })
  const completion = completeSignIn({
    signal: controller.signal,
    isCurrent: () => true,
    loadProfile: async (signal) => {
      assert.equal(signal, controller.signal)
      return profile
    },
    publishProfile: (actual) => {
      assert.equal(actual, profile)
      events.push('published')
      controller.abort() // A successful identity replacement retires the old form.
      return published
    },
    navigate: () => {
      assert.equal(hasAuthSession(), true)
      events.push('navigated')
    },
  })
  await Promise.resolve()
  assert.deepEqual(events, ['published', 'navigated'])
  release()
  assert.equal(await completion, true)
  setAuthSession(false)
})

test('leaving before profile completion prevents stale publication and navigation', async () => {
  setAuthSession(false)
  const controller = new AbortController()
  let resolve!: (profile: ProfileUserInfoModel) => void
  const pending = new Promise<ProfileUserInfoModel>((accept) => {
    resolve = accept
  })
  const effects: string[] = []
  const completion = completeSignIn({
    signal: controller.signal,
    isCurrent: () => true,
    loadProfile: () => pending,
    publishProfile: async () => {
      effects.push('published')
    },
    navigate: () => {
      effects.push('navigated')
    },
  })
  controller.abort()
  resolve(profile)
  assert.equal(await completion, false)
  assert.deepEqual(effects, [])
  assert.equal(hasAuthSession(), false)
})

test('a canceled sign-in starts no profile read and a failed read starts no session', async () => {
  setAuthSession(false)
  const controller = new AbortController()
  controller.abort()
  const unexpected = () => {
    throw new Error('Unexpected side effect')
  }
  assert.equal(
    await completeSignIn({
      signal: controller.signal,
      isCurrent: () => true,
      loadProfile: unexpected,
      publishProfile: unexpected,
      navigate: unexpected,
    }),
    false
  )
  await assert.rejects(
    completeSignIn({
      signal: new AbortController().signal,
      isCurrent: () => true,
      loadProfile: async () => {
        throw new Error('Profile unavailable')
      },
      publishProfile: unexpected,
      navigate: unexpected,
    }),
    /Profile unavailable/
  )
  assert.equal(hasAuthSession(), false)
})

test('URL ownership prevents a late redirect before React has unmounted the old route', async () => {
  setAuthSession(false)
  let current = true
  let resolve!: (profile: ProfileUserInfoModel) => void
  const pending = new Promise<ProfileUserInfoModel>((accept) => {
    resolve = accept
  })
  const unexpected = () => {
    throw new Error('Stale route committed sign-in')
  }
  const completion = completeSignIn({
    signal: new AbortController().signal,
    isCurrent: () => current,
    loadProfile: () => pending,
    publishProfile: unexpected,
    navigate: unexpected,
  })
  current = false
  resolve(profile)
  assert.equal(await completion, false)
  assert.equal(hasAuthSession(), false)
})
