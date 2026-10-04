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

test('writeup selection is explicit, retains failed uploads, and confirms saving only after acknowledgement', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/1/challenges' })
  const restoreDom = installTestDom(browser)
  const i18n = i18next.createInstance()
  await i18n.init({ lng: 'en', resources: { en: { translation: {} } } })
  const originalHook = api.game.useGameGetWriteup
  const originalSubmit = api.game.gameSubmitWriteup
  const calls: Parameters<typeof api.game.gameSubmitWriteup>[] = []
  let resolveUpload: () => void = () => undefined
  let rejectUpload: (reason: unknown) => void = () => undefined
  api.game.useGameGetWriteup = (() => ({
    data: { submitted: false },
    // Refresh failures must not erase a successful upload acknowledgement.
    mutate: async () => {
      throw new Error('status refresh unavailable')
    },
  })) as typeof originalHook
  api.game.gameSubmitWriteup = ((...args: Parameters<typeof originalSubmit>) => {
    calls.push(args)
    return new Promise<void>((resolve, reject) => {
      resolveUpload = resolve
      rejectUpload = reject
    })
  }) as typeof originalSubmit
  const { createRoot } = await import('react-dom/client')
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  try {
    await act(async () =>
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
                writeupDeadline: Date.now() + 60_000,
                opened: true,
                onClose: () => undefined,
              })
            )
          )
        )
      )
    )
    const submit = () => browser.document.querySelector<HTMLButtonElement>('button[type="submit"]')
    const select = async (name: string, type: string) => {
      const input = browser.document.querySelector<HTMLInputElement>('input[type="file"]')!
      Object.defineProperty(input, 'files', {
        configurable: true,
        value: [new browser.File(['%PDF-1.7'], name, { type })],
      })
      await act(async () => input.dispatchEvent(new browser.Event('change', { bubbles: true })))
    }
    const send = async () =>
      act(async () => {
        browser.document
          .querySelector('form')
          ?.dispatchEvent(new browser.Event('submit', { bubbles: true, cancelable: true }))
      })
    assert.equal(submit()?.disabled, true)
    await select('notes.txt', 'text/plain')
    assert.equal(submit()?.disabled, true)
    assert.match(browser.document.body.textContent, /Choose a PDF/)
    await select('team-writeup.pdf', 'application/pdf')
    assert.equal(calls.length, 0, 'file selection is not a submission')
    assert.equal(submit()?.disabled, false)
    await send()
    await send()
    assert.equal(calls.length, 1, 'duplicate submit events are single-flight')
    await act(async () => rejectUpload({ response: { status: 502 } }))
    assert.match(browser.document.body.textContent, /could not confirm/)
    assert.match(browser.document.body.textContent, /team-writeup.pdf/)
    assert.equal(submit()?.disabled, false)
    await send()
    assert.equal(calls[1][2], calls[0][2], 'retry uses the same stable operation identity')
    const progress = calls[1][3]?.onUploadProgress
    assert.ok(progress)
    await act(async () => progress({ loaded: 8, total: 8 } as Parameters<NonNullable<typeof progress>>[0]))
    assert.match(browser.document.body.textContent, /Saving your writeup/)
    assert.doesNotMatch(browser.document.body.textContent, /Your writeup has been saved/)
    await act(async () => resolveUpload())
    assert.match(browser.document.body.textContent, /Your writeup has been saved/)
    assert.match(browser.document.body.textContent, /team-writeup.pdf/)
    assert.equal(submit()?.disabled, true)
    await select('revised-writeup.pdf', 'application/pdf')
    await send()
    assert.notEqual(calls[2][2], calls[1][2])
    const signal = calls[2][3]?.signal
    await act(async () => root.unmount())
    assert.equal(signal?.aborted, true, 'leaving the event cancels the obsolete client request')
    await act(async () => resolveUpload())
  } finally {
    await act(async () => root.unmount())
    api.game.useGameGetWriteup = originalHook
    api.game.gameSubmitWriteup = originalSubmit
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
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

  const selectPdf = async () => {
    const input = browser.document.querySelector<HTMLInputElement>('input[type="file"]')
    assert.ok(input)
    Object.defineProperty(input, 'files', {
      configurable: true,
      value: [new browser.File(['%PDF-1.7'], 'writeup.pdf', { type: 'application/pdf' })],
    })
    await act(async () => input.dispatchEvent(new browser.Event('change', { bubbles: true })))
  }

  let root = await renderModal(startedAt + 1_250)
  try {
    assert.equal(uploadButton()?.disabled, true, 'choosing a file must precede submission')
    await selectPdf()
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
    await selectPdf()
    await act(async () => {
      browser.document
        .querySelector('form')
        ?.dispatchEvent(new browser.Event('submit', { bubbles: true, cancelable: true }))
      await Promise.resolve()
      await Promise.resolve()
    })
    assert.equal(uploadButton()?.disabled, true)
    assert.match(uploadButton()?.textContent ?? '', /Deadline exceeded/)
    assert.match(browser.document.querySelector('[role="alert"]')?.textContent ?? '', /Deadline exceeded/)
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
