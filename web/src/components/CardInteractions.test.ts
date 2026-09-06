import { HeadlessMantineProvider } from '@mantine/core'
import { Window } from 'happy-dom'
import i18next from 'i18next'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, type ReactElement } from 'react'
import { I18nextProvider } from 'react-i18next'
import { ChallengeCategory, ChallengeType, type ChallengeInfo } from '../Api'
import { installTestDom } from '../test/installDom'
import { LanguageProvider } from '../utils/I18n'
import { ChallengeCard } from './ChallengeCard'

const mountCards = async () => {
  const browser = new Window({ url: 'https://rsctf.test/' })
  const restoreDom = installTestDom(browser)
  const i18n = i18next.createInstance()
  await i18n.init({ lng: 'en-US', fallbackLng: 'en-US', resources: {} })
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const { createRoot } = await import('react-dom/client')
  const root = createRoot(container)
  const globals = globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }
  const previousActEnvironment = globals.IS_REACT_ACT_ENVIRONMENT
  globals.IS_REACT_ACT_ENVIRONMENT = true
  return {
    browser,
    container,
    render: async (card: ReactElement) => {
      await act(async () => {
        root.render(
          createElement(
            HeadlessMantineProvider,
            null,
            createElement(I18nextProvider, { i18n }, createElement(LanguageProvider, null, card))
          )
        )
      })
    },
    close: async () => {
      await act(async () => root.unmount())
      globals.IS_REACT_ACT_ENVIRONMENT = previousActEnvironment
      await browser.happyDOM.close()
      restoreDom()
    },
  }
}

test('challenge cards show real metrics and use one named native action for every scoring mode', async () => {
  const view = await mountCards()
  const challenge: ChallengeInfo = {
    id: 17,
    title: 'A long challenge title that must remain fully available to every player',
    category: ChallengeCategory.Web,
    type: ChallengeType.StaticAttachment,
    score: 1500,
    solved: 0,
    bloods: [],
    disableBloodBonus: true,
  }
  let opened = 0
  const card = (value = challenge, solved = false) =>
    createElement(ChallengeCard, {
      challenge: value,
      solved,
      iconMap: new Map(),
      colorMap: new Map(),
      onClick: () => opened++,
    })
  try {
    await view.render(card())
    assert.deepEqual(
      Array.from(view.container.querySelectorAll('dt'), (label) => label.textContent),
      ['pts', 'solves']
    )
    assert.deepEqual(
      Array.from(view.container.querySelectorAll('dd'), (value) => value.textContent),
      ['1,500', '0']
    )
    const button = view.container.querySelector('button')!
    assert.equal(view.container.querySelectorAll('button').length, 1)
    assert.equal(button.textContent, challenge.title)
    assert.equal(button.getAttribute('aria-label'), `Open challenge: ${challenge.title}`)
    assert.equal(button.getAttribute('aria-haspopup'), 'dialog')
    assert.equal(view.container.querySelector('article > svg')?.getAttribute('aria-hidden'), 'true')
    assert.doesNotMatch(view.container.textContent, /Open challenge →/)
    await act(async () => button.click())
    assert.equal(opened, 1)

    await view.render(card(challenge, true))
    assert.equal(view.container.querySelector('article')?.getAttribute('data-state'), 'solved')
    assert.match(view.container.textContent, /Solved/)

    const bloods = [
      'A long first-blood team name that should remain readable',
      'Second team',
      'Third team',
      'Fourth team',
    ].map((name, index) => ({ id: index + 1, name, submitTimeUtc: Date.now() }))
    await view.render(card({ ...challenge, bloods }))
    const bloodLabels = view.container.querySelectorAll('[tabindex="0"]')
    assert.equal(bloodLabels.length, 3)
    assert.match(bloodLabels[0].textContent, /A long first-blood team name/)
    assert.match(bloodLabels[0].getAttribute('aria-label')!, /1\. A long first-blood team name/)

    for (const type of [ChallengeType.AttackDefense, ChallengeType.KingOfTheHill]) {
      await view.render(card({ ...challenge, type }))
      assert.equal(view.container.querySelector('dl'), null, 'continuous scoring must not display Jeopardy totals')
      assert.match(view.container.textContent, /Live scoring/)
      assert.match(view.container.textContent, /Scored during play/)
      assert.doesNotMatch(view.container.textContent, /1,500/)
      assert.equal(view.container.querySelectorAll('button').length, 1)
    }
  } finally {
    await view.close()
  }
})
