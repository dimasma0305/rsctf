import { HeadlessMantineProvider } from '@mantine/core'
import { Window } from 'happy-dom'
import i18next from 'i18next'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { I18nextProvider } from 'react-i18next'
import api from '../Api'
import { installTestDom } from '../test/installDom'
import { LanguageProvider } from '../utils/I18n'
import { isWriteupDeadlineError, WriteupSubmitModal } from './WriteupSubmitModal'

const deadlineError = {
  response: { status: 400, data: { status: 400, title: 'Writeup deadline has passed' } },
}
const transactionalDeadlineError = {
  response: { status: 409, data: { status: 409, title: 'Writeup submission is no longer eligible' } },
}

test('writeup deadline errors have a stable client classification', () => {
  assert.equal(isWriteupDeadlineError(deadlineError), true)
  assert.equal(isWriteupDeadlineError(transactionalDeadlineError), true)
  assert.equal(
    isWriteupDeadlineError({
      response: { status: 409, data: { title: 'Writeup submission is no longer eligible' } },
    }),
    true
  )
  assert.equal(isWriteupDeadlineError({ response: { data: { status: 400, title: 'Invalid PDF' } } }), false)
})

test('writeup upload locks at the live deadline and stays locked after authoritative rejection', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/1/challenges' })
  const restoreDom = installTestDom(browser)
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
          game: {
            button: { writeup: { upload: 'Upload PDF', uploading: 'Uploading' } },
            content: {
              writeup: {
                title: 'Writeup',
                submitted: 'Submitted',
                unsubmitted: 'Not submitted',
                unsubmitted_note: 'No writeup submitted',
                current: 'Current writeup',
                deadline_exceeded: 'Deadline exceeded',
                instructions: {
                  title: 'Instructions',
                  deadline: 'Deadline: {{datetime}}',
                  file_format: 'PDF only',
                },
              },
            },
          },
        },
      },
    },
  })

  const originalHook = api.game.useGameGetWriteup
  const originalSubmit = api.game.gameSubmitWriteup
  const gameApi = api.game as typeof api.game & {
    useGameGetWriteup: typeof api.game.useGameGetWriteup
    gameSubmitWriteup: typeof api.game.gameSubmitWriteup
  }
  gameApi.useGameGetWriteup = (() => ({
    data: { submitted: false },
    mutate: async () => undefined,
  })) as typeof api.game.useGameGetWriteup
  gameApi.gameSubmitWriteup = (async () => {
    throw transactionalDeadlineError
  }) as typeof api.game.gameSubmitWriteup
  const warning = context.mock.method(console, 'warn', () => undefined)
  const { createRoot } = await import('react-dom/client')
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  const renderModal = async (deadline: number) => {
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
              LanguageProvider,
              null,
              createElement(WriteupSubmitModal, {
                gameId: 1,
                writeupDeadline: deadline,
                opened: true,
                onClose: () => undefined,
              })
            )
          )
        )
      )
    })
    return root
  }
  const uploadButton = () =>
    Array.from(browser.document.querySelectorAll('button')).find((button) =>
      /Upload PDF|Deadline exceeded/.test(button.textContent ?? '')
    )

  let root = await renderModal(startedAt + 1_250)
  try {
    assert.equal(uploadButton()?.disabled, false)
    await act(async () => context.mock.timers.tick(2_500))
    assert.equal(uploadButton()?.disabled, true)
    assert.match(uploadButton()?.textContent ?? '', /Deadline exceeded/)

    await act(async () => root.unmount())
    context.mock.timers.tick(5_000)
    root = await renderModal(Date.now() - 1_000)
    assert.equal(uploadButton()?.disabled, true, 'a newly mounted modal must sample the current time immediately')
    assert.match(uploadButton()?.textContent ?? '', /Deadline exceeded/)

    await act(async () => root.unmount())
    root = await renderModal(startedAt + 60_000)
    const input = browser.document.querySelector<HTMLInputElement>('input[type="file"]')
    assert.ok(input)
    Object.defineProperty(input, 'files', {
      configurable: true,
      value: [new browser.File(['%PDF-1.7'], 'writeup.pdf', { type: 'application/pdf' })],
    })
    await act(async () => {
      input.dispatchEvent(new browser.Event('change', { bubbles: true }))
      await Promise.resolve()
      await Promise.resolve()
    })
    assert.equal(uploadButton()?.disabled, true)
    assert.match(uploadButton()?.textContent ?? '', /Deadline exceeded/)
    assert.equal(warning.mock.callCount(), 1)
  } finally {
    await act(async () => root.unmount())
    gameApi.useGameGetWriteup = originalHook
    gameApi.gameSubmitWriteup = originalSubmit
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
