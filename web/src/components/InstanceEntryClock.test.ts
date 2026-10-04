import { HeadlessMantineProvider } from '@mantine/core'
import { Window } from 'happy-dom'
import i18next from 'i18next'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { I18nextProvider } from 'react-i18next'
import { SWRConfig } from 'swr'
import { ContainerPortMappingType, type ClientConfig } from '../Api'
import { installTestDom } from '../test/installDom'
import { observeServerTime, serverClockTestApi } from '../utils/ServerClock'
import { InstanceEntry } from './InstanceEntry'
import { WsrxProvider } from './WsrxProvider'

const clientConfig: ClientConfig = {
  title: 'RS',
  slogan: 'Capture. Compete. Conquer.',
  portMapping: ContainerPortMappingType.Default,
  footerInfo: null,
  customTheme: null,
  defaultLifetime: 120,
  extensionDuration: 120,
  renewalWindow: 10,
  enableBrowserFingerprint: false,
  allowRegister: true,
  allowPasswordRegistration: true,
  emailConfirmationRequired: false,
  donationsEnabled: false,
  donationProvider: null,
  donationUrl: null,
}

const createI18n = async () => {
  const i18n = i18next.createInstance()
  await i18n.init({
    lng: 'en',
    fallbackLng: 'en',
    resources: {
      en: {
        translation: {
          challenge: {
            button: { instance: { destroy: 'Destroy', extend: 'Extend' } },
            content: {
              instance: {
                actions: { count_down: 'Remaining', note: 'Extend near expiry' },
                entry: { label: 'Connection' },
              },
            },
          },
        },
      },
    },
  })
  return i18n
}

const findExtendButton = (container: HTMLElement) => {
  const button = [...container.querySelectorAll('button')].find((candidate) => candidate.textContent === 'Extend')
  assert.ok(button instanceof window.HTMLButtonElement)
  return button
}

const renderInstance = async (container: HTMLElement, closeTime: number) => {
  const i18n = await createI18n()
  const { createRoot } = await import('react-dom/client')
  const root = createRoot(container)
  await act(async () => {
    root.render(
      createElement(
        SWRConfig,
        {
          value: {
            provider: () => new Map(),
            fallback: { '/api/config': clientConfig },
            fetcher: async () => clientConfig,
          },
        },
        createElement(
          HeadlessMantineProvider,
          null,
          createElement(
            I18nextProvider,
            { i18n },
            createElement(
              WsrxProvider,
              null,
              createElement(InstanceEntry, {
                context: { closeTime, instanceEntry: '127.0.0.1:31337' },
                onDestroy: () => undefined,
                onExtend: () => undefined,
              })
            )
          )
        )
      )
    )
  })
  return root
}

test('a late server sample disables Extend when the browser clock is ahead', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/1/challenges' })
  const restoreDom = installTestDom(browser)
  const serverNow = 2_000_000_000_000
  const localNow = serverNow + 2 * 60 * 60_000
  context.mock.timers.enable({
    apis: ['Date', 'setInterval', 'setTimeout'],
    now: new Date(localNow),
  })
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  let root: Awaited<ReturnType<typeof renderInstance>> | undefined
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    serverClockTestApi.reset()
    root = await renderInstance(container, serverNow + 30 * 60_000)
    assert.equal(findExtendButton(container).disabled, false)

    await act(async () => {
      assert.equal(observeServerTime(serverNow, localNow), true)
    })
    assert.equal(findExtendButton(container).disabled, true)
  } finally {
    if (root) await act(async () => root.unmount())
    serverClockTestApi.reset()
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('a late server sample enables Extend when the browser clock is behind', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/1/challenges' })
  const restoreDom = installTestDom(browser)
  const serverNow = 2_000_000_000_000
  const localNow = serverNow - 2 * 60 * 60_000
  context.mock.timers.enable({
    apis: ['Date', 'setInterval', 'setTimeout'],
    now: new Date(localNow),
  })
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  let root: Awaited<ReturnType<typeof renderInstance>> | undefined
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    serverClockTestApi.reset()
    root = await renderInstance(container, serverNow + 9 * 60_000)
    assert.equal(findExtendButton(container).disabled, true)

    await act(async () => {
      assert.equal(observeServerTime(serverNow, localNow), true)
    })
    assert.equal(findExtendButton(container).disabled, false)
  } finally {
    if (root) await act(async () => root.unmount())
    serverClockTestApi.reset()
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
