import { HeadlessMantineProvider } from '@mantine/core'
import { Window } from 'happy-dom'
import i18next from 'i18next'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { I18nextProvider } from 'react-i18next'
import { MemoryRouter } from 'react-router'
import { SWRConfig } from 'swr'
import { installTestDom } from '../../test/installDom'
import Reset from './Reset'

test('password reset composes AccountView as one semantic form', async () => {
  const browser = new Window({
    url: 'https://rsctf.test/account/reset?token=reset-token&email=player%40example.com',
  })
  const restoreDom = installTestDom(browser)
  const i18n = i18next.createInstance()
  await i18n.init({ lng: 'en-US', fallbackLng: 'en-US', resources: { 'en-US': { translation: {} } } })
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
            createElement(
              SWRConfig,
              {
                value: {
                  provider: () => new Map(),
                  revalidateOnMount: false,
                  fallback: {
                    '/api/config': {
                      title: 'RS',
                      slogan: 'Capture. Compete. Conquer.',
                    },
                  },
                },
              },
              createElement(
                MemoryRouter,
                {
                  initialEntries: ['/account/reset?token=reset-token&email=player%40example.com'],
                },
                createElement(Reset)
              )
            )
          )
        )
      )
      await Promise.resolve()
    })

    const forms = container.querySelectorAll('form')
    assert.equal(forms.length, 1, "AccountView must be the reset page's only form owner")
    const submit = container.querySelector<HTMLButtonElement>('button[type="submit"]')
    assert.ok(submit)
    assert.ok(submit.closest('form'), 'the reset action must submit the AccountView form')
    assert.equal(container.querySelectorAll('form form').length, 0)
  } finally {
    await act(async () => root.unmount())
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
