import { HeadlessMantineProvider } from '@mantine/core'
import { Window } from 'happy-dom'
import i18next from 'i18next'
import assert from 'node:assert/strict'
import test from 'node:test'
import { FC, useState } from 'react'
import { act, createElement } from 'react'
import { I18nextProvider } from 'react-i18next'
import api from '../Api'
import { installTestDom } from '../test/installDom'
import { TeamJoinModal } from './TeamJoinModal'

const validInvite = 'team:7:01234567890123456789012345678901'

const flush = async () => {
  await Promise.resolve()
  await Promise.resolve()
  await Promise.resolve()
  await new Promise((resolve) => setTimeout(resolve, 0))
}

const createI18n = async () => {
  const i18n = i18next.createInstance()
  await i18n.init({
    lng: 'en',
    fallbackLng: 'en',
    resources: {
      en: {
        translation: {
          common: {
            button: { close: 'Close' },
            error: { encountered: 'Join failed', check_input: 'Check this code', unknown: 'Unknown error' },
          },
          team: {
            button: { join: 'Join' },
            content: { join: 'Join a team', join_code_hint: 'Paste the complete invite code.' },
            label: { invite_code: 'Invite code' },
            notification: {
              join: { success: 'Joined', wrong_invite_code: 'Incorrect team invitation code' },
              updated: 'Team updated',
            },
          },
        },
      },
    },
  })
  return i18n
}

test('team join retains input on validation and server failures and admits one success', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/teams' })
  const restoreDom = installTestDom(browser)
  const i18n = await createI18n()

  const originalAccept = api.team.teamAccept
  const teamApi = api.team as typeof api.team & { teamAccept: typeof api.team.teamAccept }
  const warning = context.mock.method(console, 'warn', () => undefined)
  let acceptAttempts = 0
  let acceptMode: 'failure' | 'pending' = 'failure'
  let resolveAccept: (() => void) | undefined

  teamApi.teamAccept = (async () => {
    acceptAttempts += 1
    if (acceptMode === 'failure') throw new Error('Temporary enrollment failure')
    await new Promise<void>((resolve) => {
      resolveAccept = resolve
    })
    return { data: undefined }
  }) as typeof api.team.teamAccept

  const actions: string[] = []
  const Harness: FC<{ initialCode: string }> = ({ initialCode }) => {
    const [code, setCode] = useState(initialCode)
    return createElement(TeamJoinModal, {
      opened: true,
      title: 'Join team',
      code,
      onCodeChange: setCode,
      onClose: () => actions.push('close'),
      mutate: () => actions.push('mutate'),
      onTeamReady: () => actions.push('ready'),
      enableBrowserFingerprint: false,
    })
  }

  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  const render = async (initialCode: string, key: string) => {
    await act(async () => {
      root.render(
        createElement(
          HeadlessMantineProvider,
          null,
          createElement(I18nextProvider, { i18n }, createElement(Harness, { key, initialCode }))
        )
      )
      await flush()
    })
  }
  const form = () => browser.document.querySelector<HTMLFormElement>('[data-guide="team-join-workflow"]')
  const input = () => form()?.querySelector<HTMLInputElement>('input[type="text"]')
  const submit = () => form()?.querySelector<HTMLButtonElement>('button[type="submit"]')
  const submitForm = async (twice = false) => {
    await act(async () => {
      form()?.dispatchEvent(new browser.Event('submit', { bubbles: true, cancelable: true }))
      if (twice) form()?.dispatchEvent(new browser.Event('submit', { bubbles: true, cancelable: true }))
      await flush()
    })
  }

  try {
    await render('not-an-invite', 'invalid')
    await submitForm()
    assert.equal(input()?.value, 'not-an-invite')
    assert.equal(browser.document.activeElement, input())
    assert.match(browser.document.querySelector('[role="alert"]')?.textContent ?? '', /Incorrect team invitation code/)
    assert.deepEqual(actions, [])

    await render(validInvite, 'server')
    await submitForm(true)
    assert.equal(acceptAttempts, 1, 'same-tick duplicate enrollment requests must be suppressed')
    assert.equal(input()?.value, validInvite)
    assert.equal(browser.document.activeElement, input())
    assert.match(browser.document.querySelector('[role="alert"]')?.textContent ?? '', /Temporary enrollment failure/)
    assert.deepEqual(actions, [])

    acceptMode = 'pending'
    act(() => {
      form()?.dispatchEvent(new browser.Event('submit', { bubbles: true, cancelable: true }))
      form()?.dispatchEvent(new browser.Event('submit', { bubbles: true, cancelable: true }))
    })
    assert.equal(acceptAttempts, 2)
    assert.equal(submit()?.disabled, true)
    await act(async () => {
      resolveAccept?.()
      await flush()
    })
    assert.equal(input()?.value, '')
    assert.deepEqual(actions, ['ready', 'mutate', 'close'])
    assert.equal(warning.mock.callCount(), 1)
  } finally {
    await act(async () => root.unmount())
    teamApi.teamAccept = originalAccept
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('team join clears a failed or pending attempt only when the player intentionally closes it', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/teams' })
  const restoreDom = installTestDom(browser)
  const i18n = await createI18n()
  const originalAccept = api.team.teamAccept
  const teamApi = api.team as typeof api.team & { teamAccept: typeof api.team.teamAccept }
  context.mock.method(console, 'warn', () => undefined)

  let acceptMode: 'failure' | 'pending' = 'failure'
  let acceptAttempts = 0
  let resolveAccept: (() => void) | undefined
  teamApi.teamAccept = (async () => {
    acceptAttempts += 1
    if (acceptMode === 'failure') throw new Error('Temporary enrollment failure')
    await new Promise<void>((resolve) => {
      resolveAccept = resolve
    })
    return { data: undefined }
  }) as typeof api.team.teamAccept

  const actions: string[] = []
  const Harness: FC = () => {
    const [opened, setOpened] = useState(true)
    const [code, setCode] = useState(validInvite)
    return createElement(
      'div',
      null,
      createElement(
        'button',
        { type: 'button', onClick: () => setOpened(true), 'data-testid': 'open-team-join' },
        'Open join'
      ),
      createElement(TeamJoinModal, {
        opened,
        title: 'Join team',
        code,
        onCodeChange: setCode,
        onClose: () => {
          actions.push('close')
          setCode('')
          setOpened(false)
        },
        mutate: () => actions.push('mutate'),
        onTeamReady: () => actions.push('ready'),
        enableBrowserFingerprint: false,
      })
    )
  }

  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  const form = () => browser.document.querySelector<HTMLFormElement>('[data-guide="team-join-workflow"]')
  const input = () => form()?.querySelector<HTMLInputElement>('input[type="text"]')
  const submit = async () => {
    await act(async () => {
      form()?.dispatchEvent(new browser.Event('submit', { bubbles: true, cancelable: true }))
      await flush()
    })
  }
  const close = async () => {
    await act(async () => {
      browser.document.querySelector<HTMLButtonElement>('[role="dialog"] button[aria-label="Close"]')?.click()
      await flush()
    })
  }
  const reopen = async () => {
    await act(async () => {
      browser.document.querySelector<HTMLButtonElement>('[data-testid="open-team-join"]')?.click()
      await flush()
    })
  }

  try {
    await act(async () => {
      root.render(
        createElement(HeadlessMantineProvider, null, createElement(I18nextProvider, { i18n }, createElement(Harness)))
      )
      await flush()
    })

    await submit()
    assert.equal(input()?.value, validInvite)
    assert.match(browser.document.querySelector('[role="alert"]')?.textContent ?? '', /Temporary enrollment failure/)

    await close()
    await reopen()
    assert.equal(input()?.value, '')
    assert.equal(browser.document.querySelector('[role="alert"]'), null)

    await act(async () => {
      const codeInput = input()
      const setValue = Object.getOwnPropertyDescriptor(browser.HTMLInputElement.prototype, 'value')?.set
      assert.ok(codeInput)
      assert.ok(setValue)
      setValue.call(codeInput, validInvite)
      codeInput.dispatchEvent(new browser.Event('input', { bubbles: true }))
      codeInput.dispatchEvent(new browser.Event('change', { bubbles: true }))
      await flush()
    })

    acceptMode = 'pending'
    act(() => {
      form()?.dispatchEvent(new browser.Event('submit', { bubbles: true, cancelable: true }))
    })
    assert.equal(acceptAttempts, 2)
    await close()
    await reopen()
    assert.equal(input()?.value, '')
    assert.equal(browser.document.querySelector('[role="alert"]'), null)

    await act(async () => {
      resolveAccept?.()
      await flush()
    })
    assert.deepEqual(actions, ['close', 'close'], 'a closed generation must ignore its eventual success')
  } finally {
    await act(async () => root.unmount())
    teamApi.teamAccept = originalAccept
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('team join keeps the invite and focus when the real fingerprint challenge request fails', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/teams' })
  const restoreDom = installTestDom(browser)
  const i18n = await createI18n()
  const originalChallenge = api.account.accountFingerprintChallenge
  const originalAccept = api.team.teamAccept
  const accountApi = api.account as typeof api.account & {
    accountFingerprintChallenge: typeof api.account.accountFingerprintChallenge
  }
  const teamApi = api.team as typeof api.team & { teamAccept: typeof api.team.teamAccept }
  context.mock.method(console, 'warn', () => undefined)

  let challengeAttempts = 0
  let acceptAttempts = 0
  accountApi.accountFingerprintChallenge = (async () => {
    challengeAttempts += 1
    throw new Error('Fingerprint challenge unavailable')
  }) as typeof api.account.accountFingerprintChallenge
  teamApi.teamAccept = (async () => {
    acceptAttempts += 1
    return { data: undefined }
  }) as typeof api.team.teamAccept

  const actions: string[] = []
  const Harness: FC = () => {
    const [code, setCode] = useState(validInvite)
    return createElement(TeamJoinModal, {
      opened: true,
      title: 'Join team',
      code,
      onCodeChange: setCode,
      onClose: () => actions.push('close'),
      mutate: () => actions.push('mutate'),
      onTeamReady: () => actions.push('ready'),
      enableBrowserFingerprint: true,
    })
  }

  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => {
      root.render(
        createElement(HeadlessMantineProvider, null, createElement(I18nextProvider, { i18n }, createElement(Harness)))
      )
      await flush()
    })

    const form = browser.document.querySelector<HTMLFormElement>('[data-guide="team-join-workflow"]')
    const input = form?.querySelector<HTMLInputElement>('input[type="text"]')
    assert.ok(form)
    assert.ok(input)
    await act(async () => {
      form.dispatchEvent(new browser.Event('submit', { bubbles: true, cancelable: true }))
      await flush()
    })

    assert.equal(challengeAttempts, 1)
    assert.equal(acceptAttempts, 0)
    assert.equal(input.value, validInvite)
    assert.equal(browser.document.activeElement, input)
    assert.match(
      browser.document.querySelector('[role="alert"]')?.textContent ?? '',
      /Fingerprint challenge unavailable/
    )
    assert.deepEqual(actions, [])
  } finally {
    await act(async () => root.unmount())
    accountApi.accountFingerprintChallenge = originalChallenge
    teamApi.teamAccept = originalAccept
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
