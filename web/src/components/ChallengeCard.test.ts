import { HeadlessMantineProvider } from '@mantine/core'
import { Window } from 'happy-dom'
import i18next from 'i18next'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { I18nextProvider } from 'react-i18next'
import { ChallengeCategory, ChallengeType, type ChallengeInfo, type SubmissionType } from '../Api'
import { installTestDom } from '../test/installDom'
import { LanguageProvider } from '../utils/I18n'
import { ChallengeCard } from './ChallengeCard'

test('challenge card fades when its deadline passes while mounted', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/' })
  const restoreDom = installTestDom(browser)
  const startedAt = Date.now()
  context.mock.timers.enable({
    apis: ['Date', 'setInterval', 'setTimeout'],
    now: new Date(startedAt),
  })
  const i18n = i18next.createInstance()
  await i18n.init({ lng: 'en', fallbackLng: 'en', resources: { en: { translation: {} } } })
  const challenge: ChallengeInfo = {
    id: 7,
    title: 'Reactive deadline',
    category: ChallengeCategory.Misc,
    type: ChallengeType.StaticAttachment,
    score: 100,
    solved: 0,
    deadline: startedAt + 1_250,
    bloods: [],
    disableBloodBonus: true,
  }
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const { createRoot } = await import('react-dom/client')
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  const renderCard = (cardChallenge: ChallengeInfo) =>
    createElement(
      HeadlessMantineProvider,
      null,
      createElement(
        I18nextProvider,
        { i18n },
        createElement(
          LanguageProvider,
          null,
          createElement(ChallengeCard, {
            challenge: cardChallenge,
            iconMap: new Map<SubmissionType, undefined>(),
            colorMap: new Map<SubmissionType, undefined>(),
          })
        )
      )
    )

  try {
    await act(async () => {
      root.render(renderCard(challenge))
    })
    const card = container.querySelector('article')
    assert.ok(card)
    assert.equal(card.getAttribute('data-faded'), null)

    await act(async () => context.mock.timers.tick(2_500))
    assert.equal(card.getAttribute('data-faded'), 'true')

    let staticDeadlineReads = 0
    const staticChallenge = { ...challenge, id: 8, title: 'No deadline' }
    Object.defineProperty(staticChallenge, 'deadline', {
      configurable: true,
      enumerable: true,
      get: () => {
        staticDeadlineReads += 1
        return undefined
      },
    })
    await act(async () => root.render(renderCard(staticChallenge)))
    const readsAfterRender = staticDeadlineReads
    await act(async () => context.mock.timers.tick(3_000))
    assert.equal(staticDeadlineReads, readsAfterRender, 'static cards must not subscribe to the one-second ticker')
  } finally {
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
