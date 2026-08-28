import { HeadlessMantineProvider } from '@mantine/core'
import { Window } from 'happy-dom'
import i18next from 'i18next'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { I18nextProvider } from 'react-i18next'
import { SWRConfig } from 'swr'
import api, { type AdStateModel } from '../Api'
import { installTestDom } from '../test/installDom'
import { AdChallengePanel } from './AdChallengePanel'

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
    },
  ],
}

const flush = async () => {
  for (let index = 0; index < 8; index += 1) await Promise.resolve()
}

test('terminal A&D state failures expose an accessible Retry in live and snapshot-only panels', async () => {
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
  const mount = async (snapshotOnly: boolean, fallbackState?: AdStateModel) => {
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
                  fallback: fallbackState ? { '/api/Game/41/Ad/State': fallbackState } : {},
                },
              },
              createElement(AdChallengePanel, { gameId: 41, challengeId: 7, active: true, snapshotOnly })
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
    assert.match(mounted.container.textContent ?? '', /could not be loaded/i)
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
    assert.match(
      mounted.container.querySelector('button[aria-label="Download .tar.gz"]')?.textContent ?? '',
      /Download \.tar\.gz/
    )

    await act(async () => mounted.root.unmount())
    mounted = await mount(false, { ...state, services: [] })
    assert.match(mounted.container.textContent ?? '', /could not be refreshed/i)
    assert.match(mounted.container.textContent ?? '', /No service for your team yet/i)
    assert.ok(mounted.container.querySelector('button[aria-label="Retry A&D state"]'))
  } finally {
    await act(async () => mounted.root.unmount())
    gameApi.gameAdState = originalState
    gameApi.adGameGetSshKey = originalSshKey
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('a route-owned A&D snapshot prevents a modal poll and owns Retry interaction', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/41/challenges' })
  const restoreDom = installTestDom(browser)
  const i18n = i18next.createInstance()
  await i18n.init({ lng: 'en', fallbackLng: 'en', resources: { en: { translation: {} } } })
  const originalState = api.game.gameAdState
  let directReads = 0
  let ownerRetries = 0
  api.game.gameAdState = (async () => {
    directReads += 1
    throw new Error('the modal must not own this route read')
  }) as typeof api.game.gameAdState
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
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
              { value: { provider: () => new Map(), dedupingInterval: 0 } },
              createElement(AdChallengePanel, {
                gameId: 41,
                challengeId: 7,
                active: true,
                snapshotOnly: true,
                stateOwner: {
                  error: { response: { status: 503 } },
                  mutate: async () => {
                    ownerRetries += 1
                  },
                },
              })
            )
          )
        )
      )
      await flush()
    })

    assert.equal(directReads, 0)
    const retry = container.querySelector<HTMLButtonElement>('button[aria-label="Retry A&D state"]')
    assert.ok(retry)
    await act(async () => {
      retry.click()
      await flush()
    })
    assert.equal(ownerRetries, 1)
    assert.equal(directReads, 0)
  } finally {
    await act(async () => root.unmount())
    api.game.gameAdState = originalState
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
