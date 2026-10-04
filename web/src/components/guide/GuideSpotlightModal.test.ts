import { HeadlessMantineProvider } from '@mantine/core'
import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { installTestDom } from '../../test/installDom'
import {
  GuideSpotlightModal,
  guideTargetAcceptsKeyboardEntry,
  guideTargetHasKeyboardEntryFocus,
  guideTargetKeyboardActivation,
  guideTargetMatchesActivation,
  resolveGuideTarget,
} from './GuideSpotlightModal'

const targetFixture = () => {
  const browser = new Window({ url: 'https://rsctf.test/games', width: 1200, height: 900 })
  const restore = installTestDom(browser)
  const addTarget = (name: string, left = 100, parent: HTMLElement = document.body) => {
    const element = document.createElement('button')
    element.dataset.guide = name
    element.getBoundingClientRect = () => new browser.DOMRect(left, 100, 160, 40)
    parent.append(element)
    return element
  }
  Object.defineProperty(document, 'elementsFromPoint', {
    configurable: true,
    value: (x: number, y: number) =>
      Array.from(document.querySelectorAll<HTMLElement>('[data-guide]'))
        .filter((element) => {
          const rect = element.getBoundingClientRect()
          return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom
        })
        .reverse(),
  })
  return { browser, restore, addTarget }
}

test('guide upgrades a loading fallback and keeps the chosen card among equal-priority matches', async () => {
  const { browser, restore, addTarget } = targetFixture()
  try {
    const selector = '[data-guide="event-card"], [data-guide="games-search"]'
    const search = addTarget('games-search')
    assert.ok(resolveGuideTarget(selector) === search)
    const card = addTarget('event-card', 300)
    assert.ok(resolveGuideTarget(selector, search) === card)
    const earlierCard = addTarget('event-card', 500)
    document.body.prepend(earlierCard)
    assert.ok(resolveGuideTarget(selector, card) === card)
    card.remove()
    assert.ok(resolveGuideTarget(selector, card) === earlierCard)
    earlierCard.hidden = true
    assert.ok(resolveGuideTarget(selector, earlierCard) === search)
  } finally {
    restore()
    await browser.happyDOM.close()
  }
})

test('guide excludes disabled, inert, hidden and guide-owned targets', async () => {
  const { browser, restore, addTarget } = targetFixture()
  try {
    for (const [attribute, value] of [
      ['aria-hidden', 'true'],
      ['aria-disabled', 'true'],
      ['hidden', ''],
      ['inert', ''],
      ['data-guide-surface', 'coachmark'],
      ['data-guide-layer', 'spotlight'],
    ]) {
      const parent = document.createElement('div')
      parent.setAttribute(attribute, value)
      document.body.append(parent)
      addTarget('action', 100, parent)
    }
    const disabled = addTarget('action')
    disabled.disabled = true
    const hidden = addTarget('action')
    hidden.style.visibility = 'hidden'
    const active = addTarget('action', 500)
    assert.ok(resolveGuideTarget('[data-guide="action"]', disabled) === active)
    active.remove()
    assert.ok(resolveGuideTarget('[data-guide="action"]') === null)
  } finally {
    restore()
    await browser.happyDOM.close()
  }
})

test('guide follows an application dialog and returns to the page when it closes', async () => {
  const { browser, restore, addTarget } = targetFixture()
  try {
    const background = addTarget('action')
    const dialog = document.createElement('div')
    dialog.setAttribute('role', 'dialog')
    document.body.append(dialog)
    const foreground = addTarget('fallback', 400, dialog)
    const selector = '[data-guide="action"], [data-guide="fallback"]'
    assert.ok(resolveGuideTarget(selector, background) === foreground)
    dialog.remove()
    assert.ok(resolveGuideTarget(selector, foreground) === background)
    const cover = addTarget('cover')
    const available = addTarget('action', 600)
    assert.ok(resolveGuideTarget(selector, background) === available)
    cover.remove()
    assert.ok(resolveGuideTarget(selector, available) === available)
  } finally {
    restore()
    await browser.happyDOM.close()
  }
})

test('guide click handling uses the highlighted DOM element when a duplicate is inserted', async () => {
  const { browser, restore, addTarget } = targetFixture()
  const container = document.createElement('div')
  document.body.append(container)
  const { createRoot } = await import('react-dom/client')
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  const activations: (string | undefined)[] = []
  const selected = addTarget('action')
  const flush = () => new Promise<void>((resolve) => browser.requestAnimationFrame(() => resolve()))
  try {
    await act(async () =>
      root.render(
        createElement(
          HeadlessMantineProvider,
          null,
          createElement(
            GuideSpotlightModal,
            {
              opened: true,
              onClose: () => undefined,
              title: 'Guide test',
              closeLabel: 'Close',
              size: 'sm',
              overlayOpacity: 0.38,
              targetSelector: '[data-guide="action"]',
              onTargetActivate: (target) => activations.push(target),
            },
            'Use the highlighted action'
          )
        )
      )
    )
    await act(flush)
    const spotlight = () => document.querySelector<HTMLElement>('[data-guide-layer="spotlight"]')
    assert.equal(spotlight()?.style.left, '92px')
    let duplicate: HTMLButtonElement
    await act(async () => {
      duplicate = addTarget('action', 500)
      document.body.prepend(duplicate)
      await flush()
    })
    assert.equal(spotlight()?.style.left, '92px')
    await act(async () => duplicate.click())
    assert.deepEqual(activations, [], 'an unhighlighted duplicate must not advance the tour')
    await act(async () => selected.click())
    assert.deepEqual(activations, ['action'])
    await act(async () => {
      selected.setAttribute('aria-disabled', 'true')
      selected.click()
      await flush()
    })
    assert.deepEqual(activations, ['action'], 'a disabled target cannot advance before the next measurement')
    assert.equal(spotlight()?.style.left, '492px')
    await act(async () => duplicate.click())
    assert.deepEqual(activations, ['action', 'action'], 'a replacement with the same guide name owns its handler')
  } finally {
    await act(async () => root.unmount())
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    restore()
    await browser.happyDOM.close()
  }
})

test('keyboard focus activates text-entry guide targets but not action buttons', async () => {
  const browser = new Window({ url: 'https://rsctf.test/teams' })
  const input = browser.document.createElement('input')
  input.type = 'text'
  const textarea = browser.document.createElement('textarea')
  const select = browser.document.createElement('select')
  const button = browser.document.createElement('button')
  const submit = browser.document.createElement('input')
  submit.type = 'submit'
  input.dataset.guide = 'team-create-name'

  assert.equal(guideTargetAcceptsKeyboardEntry(input as unknown as HTMLElement), true)
  assert.equal(guideTargetAcceptsKeyboardEntry(textarea as unknown as HTMLElement), true)
  assert.equal(guideTargetAcceptsKeyboardEntry(select as unknown as HTMLElement), true)
  assert.equal(guideTargetAcceptsKeyboardEntry(button as unknown as HTMLElement), false)
  assert.equal(guideTargetAcceptsKeyboardEntry(submit as unknown as HTMLElement), false)

  browser.document.body.append(input, textarea, select, button, submit)
  input.focus()
  assert.equal(
    guideTargetHasKeyboardEntryFocus(
      input as unknown as HTMLElement,
      browser.document.activeElement as unknown as Element
    ),
    true
  )
  assert.equal(
    guideTargetHasKeyboardEntryFocus(
      textarea as unknown as HTMLElement,
      browser.document.activeElement as unknown as Element
    ),
    false
  )
  assert.equal(
    guideTargetHasKeyboardEntryFocus(
      button as unknown as HTMLElement,
      browser.document.activeElement as unknown as Element
    ),
    false
  )
  assert.equal(
    guideTargetKeyboardActivation(
      input as unknown as HTMLElement,
      browser.document.activeElement as unknown as Element
    ),
    'team-create-name'
  )

  button.focus()
  assert.equal(
    guideTargetKeyboardActivation(
      input as unknown as HTMLElement,
      browser.document.activeElement as unknown as Element
    ),
    undefined
  )

  await browser.happyDOM.close()
})

test('segmented guide targets advance on the requested radio action, not focus alone', async () => {
  const browser = new Window({ url: 'https://rsctf.test/challenges' })
  const target = browser.document.createElement('div')
  target.dataset.guide = 'wsrx-local-mode'
  target.dataset.guideValue = 'wsrx'

  const localInput = browser.document.createElement('input')
  localInput.type = 'radio'
  localInput.id = 'proxy-mode-wsrx'
  localInput.value = 'wsrx'
  const localLabel = browser.document.createElement('label')
  localLabel.htmlFor = localInput.id
  const localText = browser.document.createElement('span')
  localLabel.append(localText)

  const wssInput = browser.document.createElement('input')
  wssInput.type = 'radio'
  wssInput.id = 'proxy-mode-wss'
  wssInput.value = 'wss'
  const wssLabel = browser.document.createElement('label')
  wssLabel.htmlFor = wssInput.id
  const wssText = browser.document.createElement('span')
  wssLabel.append(wssText)

  target.append(localInput, localLabel, wssInput, wssLabel)
  browser.document.body.append(target)

  localInput.focus()
  assert.equal(
    guideTargetKeyboardActivation(
      target as unknown as HTMLElement,
      browser.document.activeElement as unknown as Element
    ),
    undefined
  )
  assert.equal(guideTargetMatchesActivation(target as unknown as HTMLElement, localInput), true)
  assert.equal(guideTargetMatchesActivation(target as unknown as HTMLElement, localText), true)
  assert.equal(guideTargetMatchesActivation(target as unknown as HTMLElement, wssInput), false)
  assert.equal(guideTargetMatchesActivation(target as unknown as HTMLElement, wssText), false)

  await browser.happyDOM.close()
})
