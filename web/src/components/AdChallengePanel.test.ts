import { HeadlessMantineProvider } from '@mantine/core'
import { Window } from 'happy-dom'
import i18next from 'i18next'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { I18nextProvider } from 'react-i18next'
import { SWRConfig } from 'swr'
import api, { AdServiceDeliveryState, type AdStateModel } from '../Api'
import { installTestDom } from '../test/installDom'
import { AdChallengePanel, adServicePresentationState } from './AdChallengePanel'

const state: AdStateModel = {
  currentRound: 7,
  epochTicks: 12,
  startRound: 1,
  flagsReady: true,
  flagDeliveryFailures: 0,
  scoringPaused: false,
  services: [
    {
      adTeamServiceId: 91,
      challengeId: 7,
      challengeTitle: 'Service',
      canReset: true,
      snapshotAvailable: true,
      deliveryState: AdServiceDeliveryState.Managed,
    },
  ],
}

const flush = async () => {
  for (let index = 0; index < 8; index += 1) await Promise.resolve()
}

test('A&D presentation states keep managed and BYOC lifecycle guidance distinct', () => {
  const base = state.services[0]
  assert.equal(adServicePresentationState(undefined, false), 'managed-absent')
  assert.equal(adServicePresentationState(base, false), 'managed')
  assert.equal(adServicePresentationState(undefined, true), 'byoc-absent')
  assert.equal(
    adServicePresentationState(
      {
        ...base,
        selfHosted: true,
        deliveryState: AdServiceDeliveryState.ByocConnecting,
        containerIp: '10.13.0.7',
        containerPort: 31337,
        lastCheckStatus: null,
      },
      true
    ),
    'byoc-connecting'
  )
  assert.equal(
    adServicePresentationState(
      {
        ...base,
        selfHosted: true,
        deliveryState: AdServiceDeliveryState.ByocHealthy,
        containerIp: '10.13.0.7',
        containerPort: 31337,
        lastCheckStatus: 'Ok',
      },
      true
    ),
    'byoc-healthy'
  )
  assert.equal(
    adServicePresentationState(
      {
        ...base,
        selfHosted: true,
        deliveryState: AdServiceDeliveryState.ByocStale,
        containerIp: '10.13.0.9',
        containerPort: 31339,
        lastCheckStatus: 'Offline',
      },
      true
    ),
    'byoc-stale'
  )
})

test('A&D failures stay retryable and a missing BYOC row never requests managed provisioning', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/41/challenges' })
  const restoreDom = installTestDom(browser)
  const i18n = i18next.createInstance()
  await i18n.init({ lng: 'en', fallbackLng: 'en', resources: { en: { translation: {} } } })
  const permanentError = { response: { status: 403 } }
  const originalState = api.game.gameAdState
  const originalSshKey = api.game.adGameGetSshKey
  const gameApi = api.game as typeof api.game & {
    gameAdState: typeof api.game.gameAdState
    adGameGetSshKey: typeof api.game.adGameGetSshKey
  }
  let stateReads = 0
  gameApi.gameAdState = (async () => {
    stateReads += 1
    if (stateReads !== 3) throw permanentError
    return {
      status: 200,
      data: state,
      headers: { 'content-type': 'application/json' },
    }
  }) as typeof api.game.gameAdState
  gameApi.adGameGetSshKey = (async () => ({
    status: 200,
    data: { exists: false, algorithm: '', fingerprint: '', platformGenerated: false },
    headers: { 'content-type': 'application/json' },
  })) as typeof api.game.adGameGetSshKey

  const { createRoot } = await import('react-dom/client')
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  const mount = async (
    snapshotOnly: boolean,
    fallbackState?: AdStateModel,
    selfHosted = false,
    gameId = 41,
    challengeId = 7
  ) => {
    const container = browser.document.createElement('div')
    browser.document.body.append(container)
    const root = createRoot(container)
    await act(async () => {
      root.render(
        createElement(
          HeadlessMantineProvider,
          null,
          createElement(
            I18nextProvider,
            { i18n },
            createElement(
              SWRConfig,
              {
                value: {
                  provider: () => new Map(),
                  dedupingInterval: 0,
                  fallback: fallbackState ? { [`/api/Game/${gameId}/Ad/State`]: fallbackState } : {},
                },
              },
              createElement(AdChallengePanel, {
                gameId,
                challengeId,
                active: true,
                selfHosted,
                snapshotOnly,
              })
            )
          )
        )
      )
      await flush()
    })
    return { container, root }
  }

  let mounted = await mount(false)
  try {
    assert.ok(mounted.container.querySelector('[role="alert"]'))
    assert.match(mounted.container.textContent ?? '', /access was revoked/i)
    assert.ok(mounted.container.querySelector('button[aria-label="Retry A&D state"]'))

    await act(async () => mounted.root.unmount())
    mounted = await mount(true)
    assert.ok(mounted.container.querySelector('[role="alert"]'), 'snapshot-only failures must not render nothing')
    const retry = mounted.container.querySelector<HTMLButtonElement>('button[aria-label="Retry A&D state"]')
    assert.ok(retry)

    await act(async () => {
      retry.click()
      await flush()
    })
    assert.equal(stateReads, 3, 'explicit Retry performs one fresh state read')
    assert.equal(mounted.container.querySelector('[role="alert"]'), null)
    assert.match(mounted.container.querySelector('a[download]')?.textContent ?? '', /Download \.tar\.gz/)

    await act(async () => mounted.root.unmount())
    mounted = await mount(false, { ...state, services: [] })
    assert.match(mounted.container.textContent ?? '', /could not be refreshed/i)
    assert.match(mounted.container.textContent ?? '', /No service for your team yet/i)
    assert.ok(mounted.container.querySelector('button[aria-label="Retry A&D state"]'))

    await act(async () => mounted.root.unmount())
    // Exact reported dev regression: game 13, challenge 50 is BYOC and has no
    // service row before its first agent enrollment.
    mounted = await mount(false, { ...state, services: [] }, true, 13, 50)
    const byocText = mounted.container.textContent ?? ''
    assert.match(byocText, /self-hosted BYOC challenge/i)
    assert.doesNotMatch(byocText, /No service for your team yet|Ensure containers/i)
    assert.ok(mounted.container.querySelector('a[href="/api/Game/13/Ad/Byoc/Setup/50"][download]'))
    assert.ok(mounted.container.querySelector('a[href="/api/Game/13/Ad/Byoc/Compose/50"][download]'))

    await act(async () => mounted.root.unmount())
    mounted = await mount(
      false,
      {
        ...state,
        services: [
          {
            ...state.services[0],
            selfHosted: true,
            deliveryState: AdServiceDeliveryState.ByocHealthy,
            containerIp: '10.13.0.7',
            containerPort: 31337,
            lastCheckStatus: 'Ok',
          },
        ],
      },
      true
    )
    assert.match(mounted.container.textContent ?? '', /BYOC service is online/i)

    await act(async () => mounted.root.unmount())
    mounted = await mount(
      false,
      {
        ...state,
        services: [
          {
            ...state.services[0],
            selfHosted: true,
            deliveryState: AdServiceDeliveryState.ByocConnecting,
            containerIp: '10.13.0.7',
            containerPort: 31337,
            lastCheckStatus: null,
          },
        ],
      },
      true
    )
    assert.match(mounted.container.textContent ?? '', /BYOC agent is connecting/i)

    await act(async () => mounted.root.unmount())
    mounted = await mount(
      false,
      {
        ...state,
        services: [
          {
            ...state.services[0],
            selfHosted: true,
            deliveryState: AdServiceDeliveryState.ByocStale,
            containerIp: '10.13.0.9',
            containerPort: 31339,
            lastCheckStatus: 'Offline',
          },
        ],
      },
      true
    )
    assert.match(mounted.container.textContent ?? '', /BYOC service needs attention/i)
    assert.doesNotMatch(mounted.container.textContent ?? '', /Ensure containers/i)

    await act(async () => mounted.root.unmount())
    mounted = await mount(false, { ...state, services: [] }, false)
    assert.doesNotMatch(mounted.container.textContent ?? '', /BYOC (?:agent|service)/i)
    assert.equal(mounted.container.querySelector('a[href*="/Ad/Byoc/"]'), null)
    assert.match(mounted.container.textContent ?? '', /No service for your team yet/i)
    assert.match(mounted.container.textContent ?? '', /Ensure containers/i)
  } finally {
    await act(async () => mounted.root.unmount())
    gameApi.gameAdState = originalState
    gameApi.adGameGetSshKey = originalSshKey
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
