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

const flush = async () => {
  await Promise.resolve()
  await Promise.resolve()
  await new Promise((resolve) => setTimeout(resolve, 0))
}

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

test('challenge review keeps its draft on failure and closes only after one committed retry', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/4/challenges#7' })
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
            content: { already_solved: 'Already solved', flag_placeholders: ['flag{answer}'] },
            label: { flag: 'Flag' },
            review: {
              label: 'Rate this challenge',
              comment: 'Comment',
              placeholder: 'Leave a comment...',
              required_notice: 'Please rate this challenge',
              submit_and_close: 'Submit & Close',
            },
          },
          common: {
            button: { close: 'Close' },
            label: { like: 'Recommended', dislike: 'Not Recommended' },
          },
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
  const challenge: ChallengeDetailModel = {
    id: 7,
    title: 'Review boundary',
    content: 'Keep the draft.',
    category: ChallengeCategory.Misc,
    type: ChallengeType.StaticAttachment,
    score: 100,
    attempts: 1,
  }
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const { createRoot } = await import('react-dom/client')
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  let requests = 0
  let closeCount = 0
  let flag = 'flag{answer}'
  let resolveRetry: (() => void) | undefined
  const failures = [
    Object.assign(new Error('bad review'), { response: { status: 400 } }),
    Object.assign(new Error('forbidden review'), { response: { status: 403 } }),
    Object.assign(new Error('limited review'), { response: { status: 429 } }),
    Object.assign(new Error('review service unavailable'), { response: { status: 503 } }),
    new TypeError('network disconnected'),
  ]
  const submitReview = async () => {
    requests += 1
    if (requests <= failures.length) throw failures[requests - 1]
    await new Promise<void>((resolve) => {
      resolveRetry = resolve
    })
  }
  const render = () =>
    createElement(
      HeadlessMantineProvider,
      null,
      createElement(
        I18nextProvider,
        { i18n },
        createElement(
          LanguageProvider,
          null,
          createElement(ChallengeModal, {
            opened: true,
            onClose: () => {
              closeCount += 1
            },
            transitionProps: { duration: 0 },
            challenge,
            cateData: category,
            solved: true,
            justSolved: true,
            flag,
            setFlag: (value) => {
              if (typeof value === 'string') flag = value
            },
            receiptProof: '',
            setReceiptProof: () => undefined,
            onCreate: () => undefined,
            onDestroy: () => undefined,
            onSubmitFlag: () => undefined,
            onReviewSubmit: submitReview,
          })
        )
      )
    )

  try {
    await act(async () => {
      root.render(render())
      await flush()
    })
    const buttons = () => Array.from(browser.document.querySelectorAll<HTMLButtonElement>('button'))
    const recommended = buttons().find((button) => button.textContent?.trim() === 'Recommended')
    assert.ok(recommended)
    await act(async () => recommended.click())

    const textarea = browser.document.querySelector<HTMLTextAreaElement>('textarea')
    assert.ok(textarea)
    await act(async () => {
      const setTextareaValue = Object.getOwnPropertyDescriptor(browser.HTMLTextAreaElement.prototype, 'value')?.set
      assert.ok(setTextareaValue)
      setTextareaValue.call(textarea, 'Please keep this exact draft')
      textarea.dispatchEvent(new browser.Event('input', { bubbles: true }))
      await flush()
    })
    const submit = () => buttons().find((button) => button.textContent?.includes('Submit & Close'))
    assert.ok(submit())

    for (let index = 0; index < failures.length; index += 1) {
      await act(async () => {
        submit()?.click()
        await flush()
      })
      assert.equal(requests, index + 1)
      assert.equal(closeCount, 0)
      assert.equal(textarea.value, 'Please keep this exact draft')
      assert.equal(browser.document.activeElement, textarea)
    }

    await act(async () => {
      submit()?.click()
      submit()?.click()
      await Promise.resolve()
    })
    assert.equal(requests, failures.length + 1, 'the synchronous request owner must collapse the duplicate click')
    assert.equal(closeCount, 0)

    await act(async () => {
      resolveRetry?.()
      await flush()
    })
    assert.equal(closeCount, 1)
    assert.equal(flag, '')
  } finally {
    await act(async () => root.unmount())
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('a late challenge-A review cannot commit or clear challenge-B review state', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/4/challenges#7' })
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
            content: { already_solved: 'Already solved', flag_placeholders: ['flag{answer}'] },
            label: { flag: 'Flag' },
            review: { label: 'Rate this challenge', comment: 'Comment', save: 'Save review', update: 'Update review' },
          },
          common: { button: { close: 'Close' }, label: { like: 'Recommended', dislike: 'Not Recommended' } },
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
  const challenge = (id: number, title: string): ChallengeDetailModel => ({
    id,
    title,
    content: title,
    category: ChallengeCategory.Misc,
    type: ChallengeType.StaticAttachment,
    score: 100,
    attempts: 1,
  })
  const challengeA = challenge(7, 'Challenge A')
  const challengeB = Object.assign(challenge(8, 'Challenge B'), {
    userRating: 1,
    userComment: 'server draft B',
  })
  let resolveA: (() => void) | undefined
  let requestsA = 0
  let requestsB = 0
  let closeCount = 0
  const submitA = async () => {
    requestsA += 1
    await new Promise<void>((resolve) => {
      resolveA = resolve
    })
  }
  const submitB = async () => {
    requestsB += 1
  }
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const { createRoot } = await import('react-dom/client')
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  const render = async (current: ChallengeDetailModel, submit: () => Promise<void>) => {
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
              createElement(ChallengeModal, {
                opened: true,
                onClose: () => {
                  closeCount += 1
                },
                transitionProps: { duration: 0 },
                challenge: current,
                cateData: category,
                solved: true,
                justSolved: false,
                flag: '',
                setFlag: () => undefined,
                receiptProof: '',
                setReceiptProof: () => undefined,
                onCreate: () => undefined,
                onDestroy: () => undefined,
                onSubmitFlag: () => undefined,
                onReviewSubmit: submit,
              })
            )
          )
        )
      )
      await flush()
    })
  }
  const buttons = () => Array.from(browser.document.querySelectorAll<HTMLButtonElement>('button'))
  const reviewAction = () => buttons().find((button) => /Save review|Update review/.test(button.textContent ?? ''))

  try {
    await render(challengeA, submitA)
    await act(async () =>
      buttons()
        .find((button) => button.textContent?.trim() === 'Recommended')
        ?.click()
    )
    const textareaA = browser.document.querySelector<HTMLTextAreaElement>('textarea')
    const setValue = Object.getOwnPropertyDescriptor(browser.HTMLTextAreaElement.prototype, 'value')?.set
    assert.ok(textareaA)
    assert.ok(setValue)
    await act(async () => {
      setValue.call(textareaA, 'draft A')
      textareaA.dispatchEvent(new browser.Event('input', { bubbles: true }))
      reviewAction()?.click()
      await Promise.resolve()
    })
    assert.equal(requestsA, 1)

    await render(challengeB, submitB)
    const textareaB = browser.document.querySelector<HTMLTextAreaElement>('textarea')
    assert.ok(textareaB)
    assert.equal(textareaB.value, 'server draft B')
    await act(async () => {
      resolveA?.()
      await flush()
    })
    assert.equal(textareaB.value, 'server draft B')
    assert.equal(closeCount, 0)

    await act(async () => {
      reviewAction()?.click()
      await flush()
    })
    assert.equal(requestsB, 1)
    assert.equal(closeCount, 0)
  } finally {
    await act(async () => root.unmount())
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
