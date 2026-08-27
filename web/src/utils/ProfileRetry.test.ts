import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, type FC } from 'react'
import type { BareFetcher, SWRConfiguration } from 'swr'
import type { ProfileUserInfoModel } from '../Api'
import { installTestDom } from '../test/installDom'
import {
  MAX_PROFILE_RETRIES,
  PROFILE_RECOVERY_PROBE_MS,
  createProfileRetryTimers,
  profileErrorDisposition,
  profileRetryScheduleDelay,
  profileRetryDelay,
  retryAfterMilliseconds,
} from './ProfileRetry'

test('profile errors separate terminal sessions from bounded transient recovery', () => {
  assert.equal(profileErrorDisposition(null), 'stop')
  assert.equal(profileErrorDisposition(undefined), 'stop')
  assert.equal(profileErrorDisposition({ response: { status: 401 } }), 'anonymous')
  assert.equal(profileErrorDisposition({ status: 403 }), 'banned')
  for (const status of [400, 404, 422, 499]) {
    assert.equal(profileErrorDisposition({ response: { status } }), 'stop')
  }
  for (const status of [500, 503, 507, 520, 524, 599]) {
    assert.equal(profileErrorDisposition({ response: { status } }), 'retry')
  }
  assert.equal(profileErrorDisposition(new TypeError('offline')), 'retry')

  const limited = { response: { status: 429, headers: { 'retry-after': '12' } } }
  assert.equal(retryAfterMilliseconds(limited, 1_000), 12_000)
  assert.equal(
    profileRetryDelay(limited, 0, () => 0, 1_000),
    12_000
  )
  assert.equal(
    profileRetryDelay(limited, MAX_PROFILE_RETRIES, () => 0, 1_000),
    null
  )

  const excessive = { response: { status: 429, headers: { 'retry-after': '3600' } } }
  assert.equal(
    profileRetryDelay(excessive, 0, () => 0, 1_000),
    null
  )
  assert.equal(
    profileRetryScheduleDelay(limited, MAX_PROFILE_RETRIES, () => 0, 1_000),
    PROFILE_RECOVERY_PROBE_MS
  )
  assert.equal(
    profileRetryScheduleDelay(excessive, 0, () => 0, 1_000),
    60 * 60_000
  )
  assert.equal(profileRetryScheduleDelay({ response: { status: 404 } }, MAX_PROFILE_RETRIES), null)

  const serverDate = Date.parse('Wed, 26 Aug 2026 20:00:00 GMT')
  const retryDate = new Date(serverDate + 30_000).toUTCString()
  const datedLimit = {
    response: {
      status: 429,
      headers: { date: new Date(serverDate).toUTCString(), 'retry-after': retryDate },
    },
  }
  assert.equal(retryAfterMilliseconds(datedLimit, serverDate + 2 * 60 * 60_000), 30_000)
  assert.equal(retryAfterMilliseconds(datedLimit, serverDate - 2 * 60 * 60_000), 30_000)
})

test('profile retry timers retain only the latest retry and cancel after recovery', (context) => {
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  const retries = createProfileRetryTimers()
  const calls: string[] = []

  try {
    retries.schedule(1_000, () => {
      calls.push('superseded')
    })
    assert.equal(retries.pending(), 1)
    retries.schedule(500, () => {
      calls.push('latest')
    })
    assert.equal(retries.pending(), 1)
    context.mock.timers.tick(1_500)
    assert.deepEqual(calls, ['latest'])
    assert.equal(retries.pending(), 0)

    // A successful same-user response cancels the retry even though the
    // account identity did not change.
    retries.schedule(1_000, () => {
      calls.push('after recovery')
    })
    retries.cancel()
    context.mock.timers.tick(2_000)
    assert.deepEqual(calls, ['latest'])
    assert.equal(retries.pending(), 0)

    const recoveryDelay = profileRetryScheduleDelay({ response: { status: 520 } }, MAX_PROFILE_RETRIES, () => 0, 0)
    assert.equal(recoveryDelay, PROFILE_RECOVERY_PROBE_MS)
    retries.schedule(recoveryDelay ?? 0, () => {
      calls.push('recovered after cap')
    })
    context.mock.timers.tick(PROFILE_RECOVERY_PROBE_MS - 1)
    assert.deepEqual(calls, ['latest'])
    context.mock.timers.tick(1)
    assert.deepEqual(calls, ['latest', 'recovered after cap'])
  } finally {
    retries.cancel()
    context.mock.timers.reset()
  }
})

test('profile hook recovers from an unlisted 5xx through one bounded retry', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/' })
  const restoreDom = installTestDom(browser)
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  const { useUser } = await import('../hooks/useUser')
  const { createInstance } = await import('i18next')
  const { I18nextProvider, initReactI18next } = await import('react-i18next')
  const { SWRConfig } = await import('swr')
  const { MemoryRouter } = await import('react-router')
  const { createRoot } = await import('react-dom/client')
  const i18n = createInstance()
  await i18n.use(initReactI18next).init({ lng: 'en', resources: { en: { translation: {} } } })
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  let reads = 0
  const fetcher: BareFetcher<ProfileUserInfoModel> = async (request) => {
    assert.equal(request, '/api/account/profile')
    reads += 1
    if (reads === 1) throw { response: { status: 520 } }
    return { userId: '00000000-0000-4000-8000-000000000520', userName: 'recovered' }
  }
  const swrConfig: SWRConfiguration = {
    provider: () => new Map(),
    fetcher,
    dedupingInterval: 0,
    isOnline: () => true,
    isVisible: () => true,
  }
  const Probe: FC = () => {
    const { user, error } = useUser()
    return createElement('output', null, user?.userName ?? (error ? 'recovering' : 'loading'))
  }
  const App: FC = () =>
    createElement(
      SWRConfig,
      { value: swrConfig },
      createElement(I18nextProvider, { i18n }, createElement(MemoryRouter, null, createElement(Probe)))
    )
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(App)))
    assert.equal(reads, 1)
    assert.equal(container.textContent, 'recovering')

    await act(async () => context.mock.timers.tick(3_000))
    assert.equal(reads, 2)
    assert.equal(container.textContent, 'recovered')

    // Recovery clears the hook-owned timer; no stale retry survives it.
    await act(async () => context.mock.timers.tick(PROFILE_RECOVERY_PROBE_MS))
    assert.equal(reads, 2)
  } finally {
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    i18n.off()
    await browser.happyDOM.close()
    restoreDom()
  }
})
