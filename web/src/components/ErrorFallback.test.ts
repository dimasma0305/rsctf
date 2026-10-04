import { HeadlessMantineProvider } from '@mantine/core'
import { Window } from 'happy-dom'
import i18next from 'i18next'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { I18nextProvider } from 'react-i18next'
import { installTestDom } from '../test/installDom'
import { ErrorFallback } from './ErrorFallback'

test('error recovery presents actions first and keeps diagnostics optional and keyboard reachable', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games' })
  const restoreDom = installTestDom(browser)
  const i18n = i18next.createInstance()
  await i18n.init({ lng: 'en', resources: { en: { translation: { common: { button: { try_again: 'Try again' } } } } } })
  const { createRoot } = await import('react-dom/client')
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  let retries = 0
  try {
    await act(async () =>
      root.render(
        createElement(
          HeadlessMantineProvider,
          null,
          createElement(
            I18nextProvider,
            { i18n },
            createElement(ErrorFallback, {
              error: new Error('diagnostic fixture'),
              resetErrorBoundary: () => {
                retries++
              },
            })
          )
        )
      )
    )
    assert.equal(browser.document.querySelector('details')?.open, false)
    assert.equal(browser.document.querySelectorAll('h1').length, 1)
    assert.match(browser.document.querySelector('[role="alert"]')?.textContent ?? '', /Try again/)
    assert.equal(browser.document.querySelector('a')?.getAttribute('href'), '/games')
    const retry = Array.from(browser.document.querySelectorAll('button')).find(
      (button) => button.textContent === 'Try again'
    )
    await act(async () => retry?.click())
    assert.equal(retries, 1)
    const textarea = browser.document.querySelector('textarea')
    assert.equal(textarea?.tabIndex, 0)
    assert.match(textarea?.value ?? '', /diagnostic fixture/)
  } finally {
    await act(async () => root.unmount())
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
