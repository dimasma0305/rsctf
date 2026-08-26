import { HeadlessMantineProvider } from '@mantine/core'
import { Window } from 'happy-dom'
import i18next from 'i18next'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { I18nextProvider } from 'react-i18next'
import api, { DetailedGameInfoModel, GameJoinCheckInfoModel, GameJoinModel, TeamInfoModel } from '../Api'
import { installTestDom } from '../test/installDom'
import { submitGameEnrollment } from '../utils/EnrollmentFlow'
import { GameJoinModal } from './GameJoinModal'

const flush = async () => {
  await Promise.resolve()
  await Promise.resolve()
  await Promise.resolve()
  await new Promise((resolve) => setTimeout(resolve, 0))
}

const game = (id: number, divisionId: number, inviteCodeRequired = false): DetailedGameInfoModel => ({
  id,
  title: `Game ${id}`,
  divisions: [{ id: divisionId, name: `Division ${divisionId}`, inviteCodeRequired }],
})

const team = (id: number): TeamInfoModel => ({ id, name: `Team ${id}` })
const check = (divisionId: number): GameJoinCheckInfoModel => ({
  joinedTeams: [],
  joinableDivisions: [divisionId],
})

const createI18n = async () => {
  const i18n = i18next.createInstance()
  await i18n.init({
    lng: 'en',
    fallbackLng: 'en',
    resources: {
      en: {
        translation: {
          common: { button: { retry: 'Retry' }, error: { encountered: 'Join failed', unknown: 'Unknown error' } },
          game: {
            button: { join: 'Join event' },
            content: {
              join: {
                check_failed: 'Could not verify your current teams.',
                team: { label: 'Team', description: 'Choose a team' },
                division: { label: 'Division', description: 'Choose a division' },
                invite_code: { label: 'Invite code', description: 'Enter the event invite code' },
              },
            },
            notification: {
              no_team: 'Choose a current team',
              no_division: 'Choose a current division',
              no_invite_code: 'Enter an invite code',
            },
          },
        },
      },
    },
  })
  return i18n
}

test('event join retains choices across recoverable failures and closes only after one successful request', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/1' })
  const restoreDom = installTestDom(browser)
  const i18n = await createI18n()
  const originalCheck = api.game.gameGetGameJoinCheckInfo
  const gameApi = api.game as typeof api.game & {
    gameGetGameJoinCheckInfo: typeof api.game.gameGetGameJoinCheckInfo
  }
  gameApi.gameGetGameJoinCheckInfo = (async () => ({ data: check(10) })) as typeof api.game.gameGetGameJoinCheckInfo
  const warning = context.mock.method(console, 'warn', () => undefined)

  const submitted: GameJoinModel[] = []
  let resolveSuccess: (() => void) | undefined
  const onSubmitJoin = async (model: GameJoinModel) => {
    submitted.push(model)
    if (submitted.length === 1) throw new Error('Incorrect event invite code')
    if (submitted.length === 2) throw new Error('Fingerprint probe unavailable')
    if (submitted.length === 3) throw new Error('Temporary enrollment failure')
    await new Promise<void>((resolve) => {
      resolveSuccess = resolve
    })
  }
  const actions: string[] = []
  const currentTeams = [team(1)]
  const refreshTeams = async () => currentTeams

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
            createElement(GameJoinModal, {
              opened: true,
              title: 'Join event',
              accountId: 'account-a',
              gameId: 1,
              game: game(1, 10, true),
              teams: currentTeams,
              refreshTeams,
              onSubmitJoin,
              onClose: () => actions.push('close'),
            })
          )
        )
      )
      await flush()
      await flush()
    })

    const form = browser.document.querySelector<HTMLFormElement>('form')
    const inviteInput = browser.document.querySelector<HTMLInputElement>('[data-guide="event-join-code"]')
    const submit = form?.querySelector<HTMLButtonElement>('button[type="submit"]')
    assert.ok(form)
    assert.ok(inviteInput)
    assert.ok(submit)

    await act(async () => {
      const setValue = Object.getOwnPropertyDescriptor(browser.HTMLInputElement.prototype, 'value')?.set
      assert.ok(setValue)
      setValue.call(inviteInput, 'retry-this-code')
      inviteInput.dispatchEvent(new browser.Event('input', { bubbles: true }))
      inviteInput.dispatchEvent(new browser.Event('change', { bubbles: true }))
      await flush()
    })
    assert.equal(submit.disabled, false)

    const submitForm = async (twice = false) => {
      await act(async () => {
        form.dispatchEvent(new browser.Event('submit', { bubbles: true, cancelable: true }))
        if (twice) form.dispatchEvent(new browser.Event('submit', { bubbles: true, cancelable: true }))
        await flush()
      })
    }

    for (const message of [
      'Incorrect event invite code',
      'Fingerprint probe unavailable',
      'Temporary enrollment failure',
    ]) {
      await submitForm()
      assert.equal(inviteInput.value, 'retry-this-code')
      assert.equal(browser.document.activeElement, inviteInput)
      assert.match(browser.document.querySelector('[role="alert"]')?.textContent ?? '', new RegExp(message))
      assert.deepEqual(actions, [])
    }

    act(() => {
      form.dispatchEvent(new browser.Event('submit', { bubbles: true, cancelable: true }))
      form.dispatchEvent(new browser.Event('submit', { bubbles: true, cancelable: true }))
    })
    assert.equal(submitted.length, 4, 'same-tick duplicate submissions must share one in-flight request')
    assert.equal(submit.disabled, true)
    await act(async () => {
      resolveSuccess?.()
      await flush()
    })

    assert.deepEqual(submitted[3], { teamId: 1, divisionId: 10, inviteCode: 'retry-this-code' })
    const resetInvite = browser.document.querySelector<HTMLInputElement>('[data-guide="event-join-code"]')
    assert.ok(!resetInvite || resetInvite.value === '')
    assert.deepEqual(actions, ['close'])
    assert.equal(warning.mock.callCount(), 3)
  } finally {
    await act(async () => root.unmount())
    gameApi.gameGetGameJoinCheckInfo = originalCheck
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('event join keeps the invite and focus when the real fingerprint challenge request fails', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/1' })
  const restoreDom = installTestDom(browser)
  const i18n = await createI18n()
  const originalCheck = api.game.gameGetGameJoinCheckInfo
  const originalChallenge = api.account.accountFingerprintChallenge
  const originalJoin = api.game.gameJoinGame
  const gameApi = api.game as typeof api.game & {
    gameGetGameJoinCheckInfo: typeof api.game.gameGetGameJoinCheckInfo
    gameJoinGame: typeof api.game.gameJoinGame
  }
  const accountApi = api.account as typeof api.account & {
    accountFingerprintChallenge: typeof api.account.accountFingerprintChallenge
  }
  context.mock.method(console, 'warn', () => undefined)

  let challengeAttempts = 0
  let joinAttempts = 0
  gameApi.gameGetGameJoinCheckInfo = (async () => ({ data: check(10) })) as typeof api.game.gameGetGameJoinCheckInfo
  accountApi.accountFingerprintChallenge = (async () => {
    challengeAttempts += 1
    throw new Error('Fingerprint challenge unavailable')
  }) as typeof api.account.accountFingerprintChallenge
  gameApi.gameJoinGame = (async () => {
    joinAttempts += 1
    return { data: undefined }
  }) as typeof api.game.gameJoinGame

  const actions: string[] = []
  const currentTeams = [team(1)]
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
            createElement(GameJoinModal, {
              opened: true,
              title: 'Join event',
              accountId: 'account-a',
              gameId: 1,
              game: game(1, 10, true),
              teams: currentTeams,
              refreshTeams: async () => currentTeams,
              onSubmitJoin: (info) =>
                submitGameEnrollment({
                  gameId: 1,
                  info,
                  enableBrowserFingerprint: true,
                  t: i18n.t,
                }),
              onClose: () => actions.push('close'),
            })
          )
        )
      )
      await flush()
      await flush()
    })

    const form = browser.document.querySelector<HTMLFormElement>('form')
    const inviteInput = browser.document.querySelector<HTMLInputElement>('[data-guide="event-join-code"]')
    assert.ok(form)
    assert.ok(inviteInput)
    await act(async () => {
      const setValue = Object.getOwnPropertyDescriptor(browser.HTMLInputElement.prototype, 'value')?.set
      assert.ok(setValue)
      setValue.call(inviteInput, 'fingerprint-retry-code')
      inviteInput.dispatchEvent(new browser.Event('input', { bubbles: true }))
      inviteInput.dispatchEvent(new browser.Event('change', { bubbles: true }))
      await flush()
      form.dispatchEvent(new browser.Event('submit', { bubbles: true, cancelable: true }))
      await flush()
    })

    assert.equal(challengeAttempts, 1)
    assert.equal(joinAttempts, 0)
    assert.equal(inviteInput.value, 'fingerprint-retry-code')
    assert.equal(browser.document.activeElement, inviteInput)
    assert.match(
      browser.document.querySelector('[role="alert"]')?.textContent ?? '',
      /Fingerprint challenge unavailable/
    )
    assert.deepEqual(actions, [])
  } finally {
    await act(async () => root.unmount())
    gameApi.gameGetGameJoinCheckInfo = originalCheck
    accountApi.accountFingerprintChallenge = originalChallenge
    gameApi.gameJoinGame = originalJoin
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('event join rejects a slow game-A context after game/account and team-list transitions', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/1' })
  const restoreDom = installTestDom(browser)
  const i18n = await createI18n()
  const originalCheck = api.game.gameGetGameJoinCheckInfo
  const gameApi = api.game as typeof api.game & {
    gameGetGameJoinCheckInfo: typeof api.game.gameGetGameJoinCheckInfo
  }
  let resolveGameA: ((value: { data: GameJoinCheckInfoModel }) => void) | undefined
  const requested: number[] = []
  gameApi.gameGetGameJoinCheckInfo = (async (id: number) => {
    requested.push(id)
    if (id === 1) {
      return await new Promise<{ data: GameJoinCheckInfoModel }>((resolve) => {
        resolveGameA = resolve
      })
    }
    return { data: check(id * 11) }
  }) as typeof api.game.gameGetGameJoinCheckInfo

  const submitted: GameJoinModel[] = []
  const submitJoin = async (model: GameJoinModel) => {
    submitted.push(model)
  }
  const refreshA = async () => [team(1)]
  const refreshB = async () => [team(2), team(3)]
  const refreshD = async () => [team(4)]

  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  const render = async (
    key: string,
    props: {
      opened: boolean
      accountId: string
      gameId: number
      game: DetailedGameInfoModel
      teams: TeamInfoModel[]
      refreshTeams: () => Promise<TeamInfoModel[] | undefined>
    }
  ) => {
    await act(async () => {
      root.render(
        createElement(
          HeadlessMantineProvider,
          null,
          createElement(
            I18nextProvider,
            { i18n },
            createElement(GameJoinModal, {
              key,
              title: 'Join event',
              onSubmitJoin: submitJoin,
              onClose: () => undefined,
              ...props,
            })
          )
        )
      )
      await flush()
      await flush()
    })
  }

  try {
    act(() => {
      root.render(
        createElement(
          HeadlessMantineProvider,
          null,
          createElement(
            I18nextProvider,
            { i18n },
            createElement(GameJoinModal, {
              key: '1:account-a',
              title: 'Join event',
              opened: true,
              accountId: 'account-a',
              gameId: 1,
              game: game(1, 11),
              teams: [team(1)],
              refreshTeams: refreshA,
              onSubmitJoin: submitJoin,
              onClose: () => undefined,
            })
          )
        )
      )
    })
    assert.deepEqual(requested, [1])

    act(() => {
      root.render(
        createElement(
          HeadlessMantineProvider,
          null,
          createElement(
            I18nextProvider,
            { i18n },
            createElement(GameJoinModal, {
              key: '2:account-b',
              title: 'Join event',
              opened: true,
              accountId: 'account-b',
              gameId: 2,
              game: game(2, 22),
              teams: [team(1)],
              refreshTeams: refreshB,
              onSubmitJoin: submitJoin,
              onClose: () => undefined,
            })
          )
        )
      )
    })
    assert.deepEqual(requested, [1, 2])

    await act(async () => {
      resolveGameA?.({ data: check(11) })
      await flush()
      await flush()
    })
    assert.equal(browser.document.querySelector<HTMLButtonElement>('button[type="submit"]')?.disabled, false)

    await render('2:account-b', {
      opened: true,
      accountId: 'account-b',
      gameId: 2,
      game: game(2, 22),
      teams: [team(2), team(3)],
      refreshTeams: refreshB,
    })
    await render('2:account-b', {
      opened: true,
      accountId: 'account-b',
      gameId: 2,
      game: game(2, 22),
      teams: [team(3)],
      refreshTeams: refreshB,
    })
    assert.equal(requested.filter((id) => id === 2).length, 1)
    await act(async () => {
      browser.document
        .querySelector<HTMLFormElement>('form')
        ?.dispatchEvent(new browser.Event('submit', { bubbles: true, cancelable: true }))
      await flush()
    })
    assert.deepEqual(submitted, [{ teamId: 3, divisionId: 22, inviteCode: undefined }])

    await render('3:account-b', {
      opened: false,
      accountId: 'account-b',
      gameId: 3,
      game: game(3, 33),
      teams: [team(3)],
      refreshTeams: refreshB,
    })
    assert.equal(requested.includes(3), false, 'a closed route must not start a join-check generation')

    await render('4:account-b', {
      opened: true,
      accountId: 'account-b',
      gameId: 4,
      game: game(4, 44),
      teams: [team(4)],
      refreshTeams: refreshD,
    })
    assert.equal(requested.filter((id) => id === 4).length, 1)
  } finally {
    await act(async () => root.unmount())
    gameApi.gameGetGameJoinCheckInfo = originalCheck
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
