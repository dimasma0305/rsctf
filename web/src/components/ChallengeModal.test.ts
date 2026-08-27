import { HeadlessMantineProvider } from '@mantine/core'
import { mdiHelpCircleOutline } from '@mdi/js'
import { Window } from 'happy-dom'
import i18next from 'i18next'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, Profiler } from 'react'
import { I18nextProvider } from 'react-i18next'
import { ChallengeCategory, ChallengeType, type ChallengeDetailModel } from '../Api'
import { installTestDom } from '../test/installDom'
import { LanguageProvider } from '../utils/I18n'
import type { ChallengeCategoryItemProps } from '../utils/Shared'
import { ChallengeModal } from './ChallengeModal'

test('challenge modal ticks only while an open deadline needs updates', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/' })
  const restoreDom = installTestDom(browser)
  const startedAt = 2_000_000_000_100
  context.mock.timers.enable({
    apis: ['Date', 'setInterval', 'setTimeout'],
    now: new Date(startedAt),
  })

  const i18n = i18next.createInstance()
  await i18n.init({
    lng: 'en-US',
    fallbackLng: 'en-US',
    resources: {
      'en-US': {
        translation: {
          challenge: {
            button: { submit_flag: 'Submit flag' },
            content: {
              deadline: { label: 'Deadline', remaining: 'Remaining' },
              flag_placeholders: ['flag{answer}'],
            },
            label: { flag: 'Flag' },
          },
          common: { button: { close: 'Close' } },
        },
      },
    },
  })

  const category: ChallengeCategoryItemProps = {
    name: ChallengeCategory.Misc,
    desrc: 'Miscellaneous',
    icon: mdiHelpCircleOutline,
    color: 'gray',
    colors: Array(10).fill('#868e96') as ChallengeCategoryItemProps['colors'],
  }
  const baseChallenge: ChallengeDetailModel = {
    id: 7,
    title: 'Clock boundary',
    content: 'Read the challenge.',
    category: ChallengeCategory.Misc,
    type: ChallengeType.StaticAttachment,
    score: 100,
    attempts: 0,
  }
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const { createRoot } = await import('react-dom/client')
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  let commits = 0

  const renderModal = (opened: boolean, deadline?: number) =>
    createElement(
      HeadlessMantineProvider,
      null,
      createElement(
        I18nextProvider,
        { i18n },
        createElement(
          LanguageProvider,
          null,
          createElement(
            Profiler,
            { id: 'challenge-modal', onRender: () => commits++ },
            createElement(ChallengeModal, {
              opened,
              onClose: () => undefined,
              transitionProps: { duration: 0 },
              challenge: { ...baseChallenge, deadline },
              cateData: category,
              flag: '',
              setFlag: () => undefined,
              receiptProof: '',
              setReceiptProof: () => undefined,
              onCreate: () => undefined,
              onDestroy: () => undefined,
              onSubmitFlag: () => undefined,
            })
          )
        )
      )
    )

  try {
    await act(async () => root.render(renderModal(false, startedAt + 1_250)))
    const closedCommits = commits
    await act(async () => context.mock.timers.tick(3_000))
    assert.equal(commits, closedCommits, 'a retained closed modal must not subscribe to the ticker')

    await act(async () => root.render(renderModal(true)))
    const noDeadlineCommits = commits
    await act(async () => context.mock.timers.tick(3_000))
    assert.equal(commits, noDeadlineCommits, 'an open modal without a deadline must not subscribe to the ticker')

    const liveDeadline = Date.now() + 1_250
    await act(async () => root.render(renderModal(true, liveDeadline)))
    const flagInput = browser.document.querySelector<HTMLInputElement>('form[data-guide="flag-submit"] input')
    assert.ok(flagInput)
    assert.equal(flagInput.disabled, false)
    const deadlineCommits = commits

    await act(async () => context.mock.timers.tick(2_500))
    assert.ok(commits > deadlineCommits, 'an open deadline must remain reactive')
    assert.equal(flagInput.disabled, true)
  } finally {
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
