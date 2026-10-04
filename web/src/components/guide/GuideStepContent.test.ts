import { HeadlessMantineProvider } from '@mantine/core'
import { Window } from 'happy-dom'
import i18next from 'i18next'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { I18nextProvider } from 'react-i18next'
import { installTestDom } from '../../test/installDom'
import { GuideStepContent } from './GuideStepContent'

test('guide step details stay optional, reset for a new step, and announce only the instruction', async () => {
  const browser = new Window({ url: 'https://rsctf.test/guide' })
  const restore = installTestDom(browser)
  const i18n = i18next.createInstance()
  await i18n.init({ lng: 'en', resources: { en: { translation: {} } } })
  const { createRoot } = await import('react-dom/client')
  const root = createRoot(browser.document.body)
  const render = (stepId: string, command?: string) =>
    act(async () =>
      root.render(
        createElement(
          HeadlessMantineProvider,
          {},
          createElement(
            I18nextProvider,
            { i18n },
            createElement(GuideStepContent, {
              stepId,
              body: `Instruction ${stepId}`,
              note: 'Optional explanation',
              command,
            })
          )
        )
      )
    )
  try {
    await render('material')
    const details = browser.document.querySelector('details')!
    assert.equal(details.open, false)
    assert.equal(details.querySelector('summary')?.textContent, 'More detail')
    assert.equal(browser.document.querySelector('[role="status"]')?.textContent, 'Instruction material')
    details.open = true
    await render('material')
    assert.equal(browser.document.querySelector('details'), details)
    assert.equal(details.open, true, 'normal rerenders do not collapse the player’s explanation')
    await render('connect', 'nc <host> <port>')
    assert.equal(browser.document.querySelector('details')?.open, false)
    assert.equal(browser.document.querySelector('code')?.textContent, 'nc <host> <port>')
    assert.equal(browser.document.querySelector('[role="status"]')?.textContent, 'Instruction connect')
    assert.equal(
      browser.document.querySelectorAll('button, form, input').length,
      0,
      'instructions do not impersonate challenge actions'
    )
  } finally {
    await act(async () => root.unmount())
    restore()
    await browser.happyDOM.close()
  }
})
