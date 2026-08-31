import { HeadlessMantineProvider } from '@mantine/core'
import { mdiHelpCircleOutline } from '@mdi/js'
import { Window } from 'happy-dom'
import i18next from 'i18next'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, Profiler, type FC, useState } from 'react'
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

test('editing and submitting a flag preserves unchanged animated Markdown DOM', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/' })
  const restoreDom = installTestDom(browser)
  const i18n = i18next.createInstance()
  await i18n.init({
    lng: 'en-US',
    fallbackLng: 'en-US',
    resources: {
      'en-US': {
        translation: {
          challenge: {
            button: { submit_flag: 'Submit flag' },
            content: { flag_placeholders: ['flag{answer}'] },
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
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const { createRoot } = await import('react-dom/client')
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  let submissions = 0
  let replaceContent: (() => void) | undefined
  let animationTimer: ReturnType<typeof setInterval> | undefined

  const Harness: FC = () => {
    const [flag, setFlag] = useState('')
    const [content, setContent] = useState('<span class="tower-animation" data-frame="initial">animated tower</span>')
    replaceContent = () => setContent('<span class="tower-animation">new animation</span>')
    return createElement(ChallengeModal, {
      opened: true,
      onClose: () => undefined,
      transitionProps: { duration: 0 },
      challenge: {
        id: 557,
        title: 'Tower of Babel',
        content,
        category: ChallengeCategory.Misc,
        type: ChallengeType.StaticAttachment,
        score: 100,
        attempts: 0,
      },
      cateData: category,
      flag,
      setFlag: (value) => {
        if (typeof value === 'string') setFlag(value)
        else if (value?.currentTarget) setFlag(value.currentTarget.value)
      },
      receiptProof: '',
      setReceiptProof: () => undefined,
      onCreate: () => undefined,
      onDestroy: () => undefined,
      onSubmitFlag: () => {
        submissions += 1
      },
    })
  }

  try {
    await act(async () => {
      root.render(
        createElement(
          HeadlessMantineProvider,
          null,
          createElement(I18nextProvider, { i18n }, createElement(LanguageProvider, null, createElement(Harness)))
        )
      )
    })
    const form = browser.document.querySelector<HTMLFormElement>('form[data-guide="flag-submit"]')
    const input = form?.querySelector<HTMLInputElement>('input')
    const initialAnimation = browser.document.querySelector<HTMLElement>('.tower-animation')
    assert.ok(form)
    assert.ok(input)
    assert.ok(initialAnimation)
    let frame = 0
    context.mock.timers.enable({ apis: ['setInterval'] })
    animationTimer = setInterval(() => {
      frame += 1
      initialAnimation.dataset.frame = `running-${frame}`
    }, 100)
    context.mock.timers.tick(300)
    assert.equal(initialAnimation.dataset.frame, 'running-3')

    await act(async () => {
      const setValue = Object.getOwnPropertyDescriptor(browser.HTMLInputElement.prototype, 'value')?.set
      assert.ok(setValue)
      setValue.call(input, 'TCP1P{still_running}')
      input.dispatchEvent(new browser.Event('input', { bubbles: true }))
      input.dispatchEvent(new browser.Event('change', { bubbles: true }))
    })
    context.mock.timers.tick(200)
    const afterTyping = browser.document.querySelector<HTMLElement>('.tower-animation')
    assert.equal(afterTyping, initialAnimation)
    assert.equal(afterTyping?.dataset.frame, 'running-5')

    await act(async () => {
      form.dispatchEvent(new browser.Event('submit', { bubbles: true, cancelable: true }))
    })
    assert.equal(submissions, 1)
    assert.equal(browser.document.querySelector('.tower-animation'), initialAnimation)

    await act(async () => replaceContent?.())
    const replacedAnimation = browser.document.querySelector<HTMLElement>('.tower-animation')
    assert.notEqual(replacedAnimation, initialAnimation)
    assert.equal(replacedAnimation?.textContent, 'new animation')
  } finally {
    if (animationTimer !== undefined) clearInterval(animationTimer)
    context.mock.timers.reset()
    await act(async () => root.unmount())
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
