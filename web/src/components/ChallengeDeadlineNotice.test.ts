import { HeadlessMantineProvider } from '@mantine/core'
import dayjs from 'dayjs'
import { Window } from 'happy-dom'
import i18next from 'i18next'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { I18nextProvider } from 'react-i18next'
import { ChallengeDeadlineNotice } from './ChallengeDeadlineNotice'

const installDom = (browser: Window) => {
  const values: Record<string, unknown> = {
    window: browser,
    document: browser.document,
    navigator: browser.navigator,
    Node: browser.Node,
    Element: browser.Element,
    HTMLElement: browser.HTMLElement,
    HTMLIFrameElement: browser.HTMLIFrameElement,
    SVGElement: browser.SVGElement,
    MutationObserver: browser.MutationObserver,
    Event: browser.Event,
    getComputedStyle: browser.getComputedStyle.bind(browser),
    requestAnimationFrame: browser.requestAnimationFrame.bind(browser),
    cancelAnimationFrame: browser.cancelAnimationFrame.bind(browser),
  }
  const previous = new Map<string, PropertyDescriptor | undefined>()

  for (const [name, value] of Object.entries(values)) {
    previous.set(name, Object.getOwnPropertyDescriptor(globalThis, name))
    Object.defineProperty(globalThis, name, { configurable: true, writable: true, value })
  }

  return () => {
    for (const [name, descriptor] of previous) {
      if (descriptor) Object.defineProperty(globalThis, name, descriptor)
      else delete (globalThis as Record<string, unknown>)[name]
    }
  }
}

test('challenge deadline can expire while mounted without changing hook order', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/' })
  const restoreDom = installDom(browser)
  const startedAt = Date.now()
  context.mock.timers.enable({
    apis: ['Date', 'setInterval', 'setTimeout'],
    now: new Date(startedAt),
  })

  const i18n = i18next.createInstance()
  await i18n.init({
    lng: 'en',
    fallbackLng: 'en',
    resources: {
      en: {
        translation: {
          challenge: {
            content: {
              deadline: {
                label: 'Deadline',
                remaining: 'Remaining',
              },
            },
          },
        },
      },
    },
  })

  const expirationStates: boolean[] = []
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const { createRoot } = await import('react-dom/client')
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
            createElement(ChallengeDeadlineNotice, {
              deadline: dayjs(startedAt + 1_250),
              locale: 'en',
              onExpiredChange: (expired) => expirationStates.push(expired),
            })
          )
        )
      )
    })

    assert.match(container.textContent, /Remaining/)
    assert.deepEqual(expirationStates, [false])

    await act(async () => {
      context.mock.timers.tick(2_500)
    })

    assert.equal(container.textContent, '')
    assert.deepEqual(expirationStates, [false, true])
  } finally {
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
